# Reused by the NixOS module when the flake package is not wired directly.
{ lib, rustPlatform }:
rustPlatform.buildRustPackage {
  pname = "canvas-wm";
  version = "0.1.0";
  src = ../.;
  cargoLock.lockFile = ../Cargo.lock;
}
