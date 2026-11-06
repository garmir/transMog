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
controls are live, and aim mode helps point the camera at the latency
visualiser's screens; ffmpeg recording and a loopback output come later.

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
the bent view, as each camera frame arrives (`clock_video` off). the source
is called `transMog`; receivers list it with the host in front,
`ROTMACHINE (transMog)` on rotmachine.

it goes out at 640x360 unless `--ndi-size WxH` or the live key `ndi_size`
says otherwise; the width must be even. 1080p uyvy comes to about 125 Mb/s
of SpeedHQ, and the ssh tunnel to TouchDesigner over Wi-Fi carried 1.6 fps
of it. a sent pixel is the mean of up to 4 x 4 source texels over its
footprint, a different aspect is stretched, not cropped.

a pass scales and packs the frame to uyvy (bt.709 limited range) on the
gpu, a ring of 3 buffers reads it back, and a thread of its own maps them
and hands them to the sdk's async send. the ui thread never waits: when all three buffers are
still in flight the frame is skipped and counted. the panel shows frames
sent, skipped and failed, the receiver count, and the mean time from submit
to mapped and of the send call.

every frame carries `<tm seq="N" ts="NS"/>` as its ndi metadata: the tap's
seq and the v4l2 kernel timestamp (ns, CLOCK_MONOTONIC) of the buffer it was
made from. with the tap off, seq is the capture's own frame count. the same
seq & 0xffff is burned into the top rows after every pass, for paths that
drop metadata: 16 cells of width / 16 px over the top 32/1080 of the height,
rounded up (32 rows at 1080, 16 at 540, 11 at 360), most significant bit on
the left, Y 235 for 1 and Y 16 for 0, the scheme of the visualiser's stamp
rows. a receiver that takes the top 32/1080 of any size as the band reads
no picture rows.
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
- `preset NAME`, `ndi on|off`, `ndi_size WxH` and `qa_lock 0|1`.

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
zoom_absolute, pan_absolute, tilt_absolute and the Logitech picture
switches below at the `qa` preset's values.
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

## logitech controls

the C920 has six vendor extension units that the kernel maps to no v4l2
control, so v4l2-ctl and guvcview do not see them. transMog finds them by
GUID in the camera's USB descriptors (sysfs `descriptors`) and reads and
writes them with uvcvideo's raw `UVCIOC_CTRL_QUERY` on the controls' own
fd. they show up as ordinary controls under "Logitech Extension Units", so
the panel, presets, the control file, the websocket and `cam` lines in
`control.state` all take them by name:

    logitech_led1_mode 0          # off, on, blink, auto
    logitech_led1_frequency 20    # blink rate in 0.05 Hz steps
    logitech_rightlight 0
    logitech_test_gain 0          # 0..3
    logitech_low_light_priority 0
    logitech_pixel_defect_correction 3
    logitech_lens_shading 1       # 0..63
    logitech_gamma 1              # 0..2
    logitech_integration_time 0   # the camera reports 10..1000, reads 0

the names are libwebcam's (logitech.xml), the ranges what this C920
(046d:082d, firmware 0.11) reports, widened to its current value because
it reads pixel defect correction 3 against a maximum of 2. what each of
the test and debug switches does to the picture is not measured yet.
everything but the LED is held by `qa_lock`. only these selectors are
ever written; the unit's register, EEPROM, sensor and ISP access
selectors are refused in code. on kernel 7.2 and later the LED needs
uvcvideo's `allow_privacy_override=1`.

none of these values survive a re-enumeration, and a replugged C920
comes back with every control at its default. when the capture ends on
its own, the window tries the same camera every 2 s, then writes back the
controls as last read and the qa lock's values. stopping or picking a
device by hand cancels that.

## aim mode

the "aim" button in the top bar, the A key or `transmog gui --aim` turns
it on; it is off at start. it finds the latency visualiser's pictures in
the camera frame by their timing marks (the four ArUco squares and the
stamp block of atlas-node's `bench/seat-loop/audio-guest.c` marks table,
copied into `src/aim/marks.rs`) and draws over the preview:

- each screen's outline, labelled projector or mini, its stamp block and
  its four markers, solid when found and dashed where the fit puts them;
- per screen: in full view, clipped on which edge, or not found;
- dashed green boxes where the screens should sit, inside a 5 % margin;
- the hints in words with an arrow: tilt up or down, pan left or right,
  move back (zoom out when digital zoom is on), move closer, rotate
  clockwise or counter-clockwise by so many degrees, as seen from behind
  the camera. a missing screen is looked for on the side it hangs on,
  the projector above the mini;
- the level, from the slope of the projector's top and bottom edges,
  the median of the last 8 analyses. with only the mini found its level
  is shown but gives no rotate hint, since the mini stands at an angle
  to the camera;
- per screen the share of frames whose stamps decoded over the last
  second, the focus (laplacian variance over variance in the stamp
  block) and a note when the stamps' white clips or sits too dark.

the analysis reads the decoded frame before any gpu pass, the frame the
tap writes, on a thread of its own: a full search 4 times a second on a
half-size grey copy, checked at full size, and in between only the
stamps of each new frame where the last search put them. the ui thread
does no analysis. with one lens pass on, aim mode shows the unbent frame
in the window while the pass keeps running for ndi out; with two or more
only the text is drawn. the tap's own overlay file is not drawn while aim
mode is on.

"aim OK" shows once both screens are wholly in view and decode in 95 %
of frames for 3 s. "save calibration" then writes both screens to
`--calibration FILE` (`$XDG_CONFIG_HOME/transmog/calibration.json` by
default) in the format the latency tools' `cam/stamps.py` reads: per
screen `layout` `marks`, the frame-to-camera homography `H`, the stamp
block's camera `corners` and `thr`, plus the outline, the framing and the
decode rate. a file already there is kept as `FILE.YYYYMMDD-HHMMSS.bak`.

the panel's zoom, pan and tilt sliders write `zoom_absolute`,
`pan_absolute` and `tilt_absolute` and read all three back; no other
control is touched, and nothing goes into a preset, so applying a preset
puts its own framing back. "fit both" zooms in as far as the margin
allows, centres the screens with pan and tilt, learning how far a step
moves the picture from its first one, and zooms in again; "reset
framing" goes back to 100, 0, 0. the fit has only run against a model of
the camera in `src/aim/fit.rs`'s test, not on the c920.

## rectified tiles

`transmog gui --rect`, or the "rectified screens" checkbox in the aim
panel, maps each visualiser screen back to straight frame pixels and,
while the tap is on, writes the marks' regions of every camera frame
beside it. the decoders then read cells that are square, evenly sampled
and the same size on both screens, whatever angle the camera sees them
from. the displays themselves are drawn as they always were; all the
correction is in the camera feed.

on a thread of its own, from the decoded frame before any gpu pass:

- every 2 s, and within 250 ms of losing a marker, a full search finds
  the screens as aim mode does, then looks for any marker it missed near
  where the refined ones put it (two markers at the left fix only an
  affine map, which on a keystoned screen puts the right-hand ones over
  two marker sides off);
- every frame, each marker's white frame is cut by profiles across its
  sides, the grey crossings are fitted with lines, and the lines meet in
  sub-pixel corners, 8 a marker (outer and inner edge of the white
  frame). moves under 1 px are smoothed (0.3 of the step a frame), larger
  ones are taken at once; a marker that fails three frames running is
  dropped and the screen searched again;
- each screen gets its own homography (one marker: rotation and scale,
  two: affine, three or four: projective) and every 30 frames one radial
  lens fit, k1 alone or k1 and k2 with two screens of three markers or
  more, is fitted across them. it is taken, smoothed, only when it lowers
  the corner rms by 10 %. the "fit lens k1 k2" checkbox turns it off;
- the regions are resampled with bilinear taps at `--rect-scale` tile
  pixels a frame pixel (0.5 by default: native for the projector, four
  times the camera's pixels for the mini). where a tile pixel spans more
  than one camera pixel it averages n x n taps, n up to 4.

the tiles, in frame pixels:

| tile | x0 | y0 | x1 | y1 | holds |
| --- | --- | --- | --- | --- | --- |
| `ruler` | 0 | 88 | 520 | 136 | the ruler's row 3, left 512 px |
| `stamps` | 320 | 144 | 852 | 416 | the stamp block and its quiet border |
| `key` | 312 | 416 | 584 | 520 | the key toggle |
| `mid` | 308 | 536 | 428 | 592 | the mid frame toggle |
| `bottom` | 1256 | 984 | 1376 | 1040 | the bottom frame toggle |
| `fid0` | 72 | 144 | 296 | 368 | fiducial 0, which tells the layouts apart |

`DIR/rect-proj/` and `DIR/rect-mini/`, DIR the tap's directory:

- `NNN.rect`, NNN = seq % 64, seq the tap's seq of the source frame (the
  capture's frame count with the tap off), so a reader of tap slot NNN
  finds the same frame's tiles under the same name. written through a
  dot file and a rename, as the tap is;
- `latest`: `SEQ SLOTS`;
- `h.jsonl`: one line a frame, appended and flushed whole, moved to
  `h.jsonl.1` after 65536 lines. it carries the header's fields plus the
  geometry: `scale` (camera px per frame px, by area), `scale_x`,
  `scale_y`, `rotation_deg`, `skew_deg`, `keystone_h_pct` (right edge
  over left, less 1), `keystone_v_pct` (bottom over top), `view_deg`
  (screen normal against the line of sight, from a pinhole of 1360 px at
  1920 wide), `fids`, the corner fit `rms_px` and `max_px`, the decoded
  `N`, `A` and `M`, and `rows`, the camera row of each mark's centre
  (`N`, `A`, `M`, `top`, `key`, `mid`, `bottom`, `ruler`) for the rolling
  shutter.

a slot is a 192 byte little-endian header, then per tile a 32 byte tile
header and its grey bytes, row major:

| offset | type | field |
| --- | --- | --- |
| 0 | 8 bytes | magic `TMQARCT1` |
| 8 | u64 | seq |
| 16 | u64 | the capture's frame seq |
| 24 | i64 | the source frame's kernel timestamp, ns, CLOCK_MONOTONIC |
| 32 | i64 | written, ns, CLOCK_MONOTONIC |
| 40 | u32 | camera width |
| 44 | u32 | camera height |
| 48 | f32 | tile pixels a frame pixel |
| 52 | u32 | tile count |
| 56 | 9 x f64 | H, frame px to undistorted camera px, row major, H[8] = 1 |
| 128 | 5 x f64 | k1, k2, lens centre x, y, norm (camera px for r = 1) |
| 168 | f32 | corner fit rms, camera px |
| 172 | u32 | flags: 1 corners tracked this frame (else just searched), 2 lens fitted |
| 176 | u32 | bit i set when fiducial i was used |
| 180 | 12 bytes | zero |

| tile offset | type | field |
| --- | --- | --- |
| 0 | 8 bytes | name, nul padded |
| 8 | 4 x f32 | frame rectangle x0, y0, x1, y1 |
| 24 | u32 | width, px |
| 28 | u32 | height, px |

a frame point (x, y) was recorded at camera pixel c + (p - c)(1 + k1 r^2
+ k2 r^4), p = H (x, y, 1) divided through, c the lens centre and r =
|p - c| / norm. a tile pixel (i, j) covers frame x0 + i / scale to
x0 + (i + 1) / scale. outside the camera picture a tile reads 0.

in aim mode the checkbox, or R, shows both rectified screens side by
side in place of the camera picture, a quarter-scale warp 4 times a
second, with the table's stamp block, markers and toggles outlined, the
decoded N, A and M, the decode share over the last 30 frames, the corner
fit error and the geometry. the panel shows the same numbers, the lens
terms and the thread's cpu time a frame.
