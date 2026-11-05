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

