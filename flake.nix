{
  description = "transMog, a webcam viewer with lens effects";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        # the NDI SDK is the one unfree package transMog needs
        config.allowUnfreePredicate = pkg: nixpkgs.lib.getName pkg == "ndi-6";
        overlays = [ self.overlays.ndi ];
      };
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

      # NDI republishes the 6.x installer under one url, so the hash in
      # nixpkgs goes stale with every point release; this one is the
      # 2026-11-05 tarball, which unpacks libndi 6.3.2
      overlays.ndi = final: prev: {
        ndi-6 = prev.ndi-6.overrideAttrs (old: {
          src = final.fetchurl {
            inherit (old.src) url;
            hash = "sha256-8DFPJFRG3vxIi2POtGiazxqWWu79ray3BXG7IWqMwYM=";
          };
        });
      };

      overlays.default = final: prev: {
        transmog = final.callPackage ./package.nix { };
      };

      formatter.${system} = pkgs.nixfmt-tree;

      # nix flake check is the whole of CI, so these run the same on a
      # laptop and in the workflow. Each one reuses the package's vendored
      # crates and build inputs and swaps its build for one cargo command
      checks.${system} =
        let
          transmog = self.packages.${system}.transmog;
          cargoCheck =
            name: tools: command:
            transmog.overrideAttrs (old: {
              pname = "transmog-${name}";
              nativeBuildInputs = old.nativeBuildInputs ++ tools;
              buildPhase = ''
                runHook preBuild
                ${command}
                runHook postBuild
              '';
              doCheck = false;
              installPhase = "touch $out";
              postInstall = "";
              postFixup = "";
              desktopItems = [ ];
              dontFixup = true;
            });
        in
        {
          inherit transmog;
          fmt = cargoCheck "fmt" [ pkgs.rustfmt ] "cargo fmt --check";
          clippy = cargoCheck "clippy" [
            pkgs.clippy
          ] "cargo clippy --offline --all-targets -- -D warnings";
          # the package build runs the tests too, but in release; this runs
          # them in the dev profile, with debug assertions and overflow
          # checks on. The sandbox has no wgpu adapter, so the kittest tests
          # take their no-renderer branch, and the camera and gpu tests stay
          # #[ignore]d
          test = cargoCheck "test" [ ] "cargo test --offline --frozen";
        };

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

        buildInputs = runtimeLibs ++ [ pkgs.ndi-6 ];

        # winit, wgpu and the NDI sender dlopen these at run time, so cargo
        # run needs them on the library path
        shellHook = ''
          export LD_LIBRARY_PATH=${
            pkgs.lib.makeLibraryPath (runtimeLibs ++ [ pkgs.ndi-6 ])
          }''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
        '';
      };
    };
}
