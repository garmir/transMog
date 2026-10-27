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
  libx11,
  libxcursor,
  libxi,
  libxcb,
}:

rustPlatform.buildRustPackage {
  pname = "transmog";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions ([
      ./Cargo.toml
      ./Cargo.lock
      ./src
      ./assets
    ]);
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

  postInstall = ''
    install -Dm644 assets/transmog.svg \
      $out/share/icons/hicolor/scalable/apps/transmog.svg
  '';

  # winit and wgpu dlopen the wayland, x11, xkb, vulkan and gl libraries
  # instead of linking them, so the wrapper has to point at them
  postFixup = ''
    wrapProgram $out/bin/transmog \
      --prefix LD_LIBRARY_PATH : ${
        lib.makeLibraryPath [
          wayland
          libxkbcommon
          vulkan-loader
          libGL
          libx11
          libxcursor
          libxi
          libxcb
        ]
      }
  '';

  desktopItems = [
    (makeDesktopItem {
      name = "transmog";
      desktopName = "transMog";
      exec = "transmog";
      icon = "transmog";
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
    mainProgram = "transmog";
    platforms = lib.platforms.linux;
  };
}
