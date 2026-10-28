{
  description = "canvas-wm: infinite-canvas Wayland compositor";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }: let
    system = "x86_64-linux";
    pkgs = import nixpkgs { inherit system; };
  in {
    packages.${system} = {
      default = pkgs.rustPlatform.buildRustPackage {
        pname = "canvas-wm";
        version = "0.1.0";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
      };

      # Full compositor (Smithay winit backend). Heavier system closure.
      compositor = pkgs.rustPlatform.buildRustPackage {
        pname = "canvas-wm";
        version = "0.1.0";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
        cargoBuildFlags = [ "--features" "compositor" ];
        cargoTestFlags = [ "--features" "compositor" ];
        nativeBuildInputs = with pkgs; [ pkg-config makeWrapper ];
        buildInputs = with pkgs; [
          wayland
          mesa
          libgbm
          libglvnd
          libinput
          udev
          libdrm
          dbus
          libxkbcommon
          xwayland
          xorg.libX11
          xorg.libXcursor
          xorg.libXrandr
          xorg.libXi
          xorg.libXext
          xorg.libXfixes
          xorg.libXrender
        ];
        # dlopen at runtime (winit X11, EGL, libwayland-server) needs the
        # libs on LD_LIBRARY_PATH, not just at link time. cargo test runs
        # the un-wrapped binary, so the seat e2e test needs it in checkPhase.
        preCheck = with pkgs; ''
          export LD_LIBRARY_PATH="${lib.makeLibraryPath [
            wayland
            mesa
            libglvnd
            libxkbcommon
          ]}:$LD_LIBRARY_PATH"
        '';
        postFixup = with pkgs; ''
          wrapProgram $out/bin/canvas-wm \
            --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath [
              wayland
              mesa
              libgbm
              libglvnd
              libinput
              udev
              libdrm
              dbus
              libxkbcommon
              xorg.libX11
              xorg.libXcursor
              xorg.libXrandr
              xorg.libXi
              xorg.libXext
              xorg.libXfixes
              xorg.libXrender
            ]}" \
            --prefix PATH : "${lib.makeBinPath [ xwayland ]}"
        '';
      };
    };

    apps.${system} = {
      # Zero-clone launcher: polls for readiness, discovers DISPLAY at
      # runtime, then starts the given clients. No fixed sleep, and it
      # never waits for the compositor to quit.
      #   nix run github:hodr-bot/canvas-wm#launch -- xterm xcalc
      launch = {
        type = "app";
        program = "${pkgs.stdenv.mkDerivation {
          name = "canvas-launch";
          src = ./scripts/launch-with-clients.sh;
          dontUnpack = true;
          nativeBuildInputs = [ pkgs.makeWrapper ];
          installPhase = ''
            mkdir -p $out/bin
            makeWrapper $src $out/bin/canvas-launch \
              --set-default CANVAS_CMD "${self.packages.${system}.compositor}/bin/canvas-wm --winit"
          '';
        }}/bin/canvas-launch";
      };
    };

    devShells.${system}.default = pkgs.mkShell {
      packages = with pkgs; [ cargo rustc pkg-config ];
    };

    nixosModules.default = import ./nix/module.nix;
  };
}
