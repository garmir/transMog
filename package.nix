{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  makeDesktopItem,
  copyDesktopItems,
  wayland,
  libxkbcommon,
  vulkan-loader,
  libGL,
  ffmpeg,
}:

rustPlatform.buildRustPackage {
  pname = "camera-app";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions (
      [
        ./Cargo.toml
        ./Cargo.lock
        ./src
      ]
      ++ lib.optional (builtins.pathExists ./assets) ./assets
    );
  };

  cargoLock.lockFile = ./Cargo.lock;

  nativeBuildInputs = [
    # v4l2-sys-mit runs bindgen at build time
    rustPlatform.bindgenHook
    pkg-config
    makeWrapper
    copyDesktopItems
  ];

  buildInputs = [
    wayland
    libxkbcommon
    vulkan-loader
    libGL
  ];

  doCheck = true;

  # winit and wgpu dlopen the wayland, xkb, vulkan and gl libraries
  # instead of linking them, so the wrapper has to point at them
  postFixup = ''
    wrapProgram $out/bin/camera-app \
      --prefix LD_LIBRARY_PATH : ${
        lib.makeLibraryPath [
          wayland
          libxkbcommon
          vulkan-loader
          libGL
        ]
      } \
      --prefix PATH : ${lib.makeBinPath [ ffmpeg ]}
  '';

  desktopItems = [
    (makeDesktopItem {
      name = "camera-app";
      desktopName = "Camera App";
      exec = "camera-app";
      icon = "camera-web";
      categories = [
        "Utility"
        "Video"
        "AudioVideo"
      ];
      comment = "webcam viewer with lens effects and camera control";
    })
  ];

  meta = {
    description = "webcam viewer with lens effects and camera control";
    license = lib.licenses.mit;
    mainProgram = "camera-app";
    platforms = lib.platforms.linux;
  };
}
