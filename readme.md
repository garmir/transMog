# camera-app

camera-app shows a v4l2 webcam in a window, runs the picture through a
stack of gpu passes (a fisheye lens is the first one) and exposes the
camera's own controls, such as exposure, focus and white balance, as
widgets. it is written in rust on eframe and wgpu and talks to the
camera directly through v4l2, so it needs no pipewire or gstreamer.

run it with `nix run github:garmir/camera-app` or, from a checkout,
`nix run /path/to/camera-app`. `nix develop` gives a shell with cargo
and the runtime libraries on the library path, so `cargo run` works
there too. the name camera-app is a placeholder.

status: milestone 1, preview, fisheye, hardware controls. on launch it
opens the first c920 it finds, else the first capture device, at mjpeg
1280x720@30 when the camera offers it. the fisheye pass and the v4l2
controls are live; ffmpeg recording and a loopback output come later.
