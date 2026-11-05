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
