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
        nativeBuildInputs = with pkgs; [ pkg-config ];
        buildInputs = with pkgs; [
          wayland
          mesa
          libgbm
          libinput
          udev
          libdrm
          dbus
          libxkbcommon
        ];
      };
    };

    devShells.${system}.default = pkgs.mkShell {
      packages = with pkgs; [ cargo rustc pkg-config ];
    };

    nixosModules.default = import ./nix/module.nix;
  };
}
