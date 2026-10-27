{
  description = "transMog, a webcam viewer with lens effects";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      runtimeLibs = with pkgs; [
        wayland
        libxkbcommon
        vulkan-loader
        libGL
        libx11
        libxcursor
        libxi
        libxcb
      ];
    in
    {
      packages.${system} = {
        transmog = pkgs.callPackage ./package.nix { };
        default = self.packages.${system}.transmog;
      };

      overlays.default = final: prev: {
        transmog = final.callPackage ./package.nix { };
      };

      formatter.${system} = pkgs.nixfmt-tree;

      checks.${system}.transmog = self.packages.${system}.transmog;

      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          # v4l2-sys-mit runs bindgen at build time, and the hook points it
          # at libclang and the kernel headers
          rustPlatform.bindgenHook
          cargo
          rustc
          clippy
          rustfmt
          rust-analyzer
          pkg-config
          v4l-utils
          ffmpeg
        ];

        buildInputs = runtimeLibs;

        # winit and wgpu dlopen these at run time, so cargo run needs them
        # on the library path
        shellHook = ''
          export LD_LIBRARY_PATH=${pkgs.lib.makeLibraryPath runtimeLibs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
        '';
      };
    };
}
