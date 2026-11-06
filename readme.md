# transMog

transMog shows a v4l2 webcam in a window, runs the picture through a
stack of gpu passes (a fisheye lens is the first one) and exposes the
camera's own controls, such as exposure, focus and white balance, as
widgets. it is written in rust on eframe and wgpu and talks to the
camera directly through v4l2, so it needs no pipewire or gstreamer.

run it with `nix run github:garmir/transMog` or, from a checkout,
`nix run /path/to/transMog`. `nix develop` gives a shell with cargo
and the runtime libraries on the library path, so `cargo run` works
there too.

status: milestone 1, preview, fisheye, hardware controls. on launch it
opens the first c920 it finds, else the first capture device, at mjpeg
1280x720@30 when the camera offers it. the fisheye pass and the v4l2
controls are live; ffmpeg recording and a loopback output come later.

## presets

a preset is a text file of `name value` lines in
`$XDG_CONFIG_HOME/transmog/presets/<name>.preset`, applied in file order.
control names are the ones `v4l2-ctl --list-ctrls` prints, so a preset can
be written by hand. an optional `format mjpeg:1920x1080@30` line restarts
the capture in that mode first. a value the camera refuses while its auto
mode is still on is retried once the rest have landed, and every control is
then read back from the camera; the presets panel shows wanted against read
back, and the status line says how many differ.

"save current" writes the camera's writable controls with the mode
switches first. `transmog preset list`, `transmog preset save NAME` and
`transmog preset apply NAME` do the same from a shell; the format line is
left to the window, which owns the stream. `transmog gui --preset NAME`
applies one at start.

## qa tap

`transmog gui --tap [DIR]`, or the checkbox under "qa tap", writes every
buffer the driver hands back to a ring of files, before the mjpeg decode
and before any gpu pass, so the lens never touches it. DIR defaults to
`$XDG_RUNTIME_DIR/transmog/tap`, a tmpfs. the window keeps showing the
camera.

- `NNN.frame`, NNN = seq % 64, three digits: a 64 byte header and then the
  buffer as the camera sent it (an mjpeg frame or raw yuyv).
- `latest`: `SEQ SLOTS`, the newest seq and the ring size.

the header is little-endian:

| offset | type | field |
| --- | --- | --- |
| 0 | 8 bytes | magic `TMQATAP1` |
| 8 | u64 | seq, from 1, counting on across capture restarts |
| 16 | u32 | v4l2 sequence; a gap is a frame the driver dropped |
| 20 | u32 | v4l2 buffer flags (uvcvideo: monotonic, start of exposure) |
| 24 | i64 | kernel timestamp, ns, CLOCK_MONOTONIC |
| 32 | i64 | dequeue time, ns, CLOCK_MONOTONIC |
| 40 | u32 | width |
| 44 | u32 | height |
| 48 | 4 bytes | fourcc, `MJPG` or `YUYV` |
| 52 | u32 | payload length |
| 56 | 8 bytes | zero |

each slot and `latest` are written to a dot file and renamed into place, so
a reader opens either the old file or the whole new one and never has to
lock anything. to follow the stream, read `latest`, then open slots from the
last seq seen up to it and skip any whose header seq differs (the ring
moved past it). 64 slots hold about 2 s at 30 fps. the dequeue time minus
the kernel timestamp is the usb transfer plus any wait for the capture
thread; mjpeg decode cost shows up there and as gaps in the v4l2 sequence.

a program reading the tap can write `DIR/overlay`, and transMog draws it
over the preview: `quad LABEL x0 y0 x1 y1 x2 y2 x3 y3` in camera pixels and
`text ANY WORDS` lines. outlines only line up with the lens off, so they
are hidden while a pass is enabled; the text always shows, marked stale
after 5 s without a change.

## ndi out

`transmog gui --ndi`, or the "NDI out" checkbox under "ndi", sends the
picture as it leaves the last gpu pass, so with the lens on a receiver gets
the bent view, at the capture size and as each camera frame arrives
(`clock_video` off). the source is called `transMog`; receivers list it with
the host in front, `ROTMACHINE (transMog)` on rotmachine.

a pass packs the frame to uyvy (bt.709 limited range) on the gpu, a ring of
3 buffers reads it back, and a thread of its own maps them and hands them to
the sdk's async send. the ui thread never waits: when all three buffers are
still in flight the frame is skipped and counted. the panel shows frames
sent, skipped and failed, the receiver count, and the mean time from submit
to mapped and of the send call.

every frame carries `<tm seq="N" ts="NS"/>` as its ndi metadata: the tap's
seq and the v4l2 kernel timestamp (ns, CLOCK_MONOTONIC) of the buffer it was
made from. with the tap off, seq is the capture's own frame count. the same
seq & 0xffff is burned into the top 32 rows after every pass, for paths that
drop metadata: 16 cells of width / 16 px, most significant bit on the left,
Y 235 for 1 and Y 16 for 0, the scheme of the visualiser's stamp rows.
`src/ndi/stamp.rs` has the encoder and a decoder that averages the middle
of each cell.

libndi is opened with dlopen when ndi out starts, so the binary runs without
it. the flake and the package bring nixpkgs' `ndi-6`, unfree, allowed for
that one package; `$NDI_RUNTIME_DIR_V6` points at another copy. ndi
republishes the installer under one url, so the flake overrides the hash
nixpkgs has; when the download stops matching, the new hash goes in
`overlays.ndi`.

## live control

transMog reads `key value` lines from `<tap dir>/control`, or from the
file `gui --control-file PATH` names, checking its mtime every 100 ms the
way it checks the overlay. `gui --control ws://HOST:PORT/PATH` also
subscribes to a websocket, TouchDesigner's `/viz_ctrl/ws` on ki1be, and
takes the object at `assets.camera.transmog` from each json message as the
same pairs. both can run at once.

- a v4l2 control by its v4l2-ctl name: `zoom_absolute 150`,
  `focus_automatic_continuous 0`, `power_line_frequency 1`. values are
  integers; `on`, `off`, `true`, `false` and fractions (rounded) are taken
  too.
- a pass parameter as `<pass>_<field>`, the fields of the pass's struct:
  `fisheye_enabled`, `fisheye_projection` (a label or its index in the
  menu), `fisheye_lens_fov`, `fisheye_source_fov`, `fisheye_fit`,
  `fisheye_zoom`, `fisheye_k1`, `fisheye_yaw`, `fisheye_pitch`,
  `fisheye_roll`, `fisheye_edge`, `fisheye_centre_x`, `fisheye_centre_y`.
  angles are in degrees and clamped as the sliders clamp them.
- `preset NAME`, `ndi on|off` and `qa_lock 0|1`.

a pair is applied only when its value differs from the last one the same
source sent. viz_ctrl repeats its whole state about 56 times a second, so
without this a slider moved in the window would snap back on the next
message. when the file and the websocket change one key in the same tick
the file wins; otherwise the later change wins. the window's own widgets
show the live values, since they draw from the same pass fields and the
same control read-back.

after each change the controls are read back, and `control.state` beside
the control file is rewritten through a dot file and a rename whenever its
content moves, by a change from either source or from the window:

    t 1793921606.898878
    qa_lock 1
    ndi on
    file /run/user/1000/transmog/tap/control
    ws ws://192.168.1.242:9980/viz_ctrl/ws connected
    set fisheye_k1 0.3 0.3 ok ws:7644
    set gain 200 159 locked file
    cam zoom_absolute 100
    lens fisheye_lens_fov 180

a `set` line is `KEY WANTED GOT STATUS SOURCE [MESSAGE]`, the status one of
`ok`, `differs` (written, read back otherwise; the C920 keeps gain 160 as
159), `locked` or `error`, and the source `file` or `ws:SEQ` with the
message's seq. `cam` lines are every v4l2 control as last read, `lens`
lines every pass parameter.

`qa_lock`, on from the start, holds exposure_time_absolute, auto_exposure,
exposure_dynamic_framerate, gain, focus_absolute,
focus_automatic_continuous, white_balance_*, power_line_frequency,
zoom_absolute, pan_absolute and tilt_absolute at the `qa` preset's values.
the latency ruler decodes stamp cells off the camera picture: a moving
exposure smears them, and its screen calibration is in camera pixels, so
digital zoom, pan or tilt moves the cells off their quads. a held key from
either source is reported `locked` and not written, the window greys those
widgets out, another preset skips them, and turning the lock back on, or
"reset all", writes the qa values again. the pass parameters are never
held; they act after the tap, so the ruler never sees them. the lock does nothing while no `qa` preset
exists. the checkbox under "live control" turns it off.

the websocket client is std only and speaks plain `ws://`: text frames,
pings answered, no tls, and it does not check the server's accept hash. it
reconnects from 0.5 s to 10 s apart and drops a link that has been silent
for 5 s.
