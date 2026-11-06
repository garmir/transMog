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

status: milestone 2, preview, fisheye, hardware controls, the
tooooools effects. on launch it opens the first c920 it finds, else the
first capture device, at mjpeg 1280x720@30 when the camera offers it.
the pass stack and the v4l2 controls are live; ffmpeg recording and a
loopback output come later.

## passes

the side panel holds a stack of passes; "add" lists them by group and
each row has an enable box, order buttons and the pass's own widgets.
a fresh stack is the preprocess followed by the fisheye.

- lens: `fisheye`, driven by the lens and camera angles in degrees.
- preprocess: blur, grain, gamma and black and white points, which
  the site applies before every effect. its defaults leave the frame
  alone.
- tooooools, in the site's order: `stippling`, `dots`, `patterns`,
  `edge`, `distort`, `displace`, `dithering`, `bevel`, `recolor`,
  `scatter`, `cellular automata`, `gradients`, `crt`, `ascii`.

the fourteen effects follow the designs on
[tooooools.app](https://tooooools.app) by Daniil Sukhovskoy, free for
personal and commercial use with attribution appreciated, and keep its
controls, ranges and defaults. most are fragment shaders. `gradients`, `scatter`
and the floyd-steinberg mode of `dithering` run on the cpu, since each
pixel depends on the ones before it, so they cost more than the rest:
scatter at its defaults takes about 11 ms a frame on an intel uhd,
floyd-steinberg at pixel size 1 about 50 ms. `displace` draws a point
cloud with a depth buffer instead of a fullscreen quad.

`TRANSMOG_STACK=preprocess,crt` starts the app with that stack instead
of the default one: a comma-separated list of the names above, in
order. an unknown name is skipped with a warning.

three passes read files from `~/.config/transmog`:

- `patterns/*.png`: tiles for `patterns`, sorted darkest first, in
  place of the built-in set. the "reload tiles" button picks up changes.
- `distort.png`: the displacement map for `distort` when the map combo
  says so, stretched over the frame; the red channel is the amount.
- `scatter/*.png`: sprites for `scatter` in place of the built-in disc.
