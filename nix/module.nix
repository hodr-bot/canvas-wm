# NixOS system module for canvas-wm (no home-manager required).
# Usage in configuration.nix:
#   inputs.canvas-wm.nixosModules.default
#   services.canvas-wm.enable = true;
{ config, lib, pkgs, ... }:
let cfg = config.services.canvas-wm;
in {
  options.services.canvas-wm = {
    enable = lib.mkEnableOption "canvas-wm infinite-canvas compositor";
    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix { };
      description = "canvas-wm package to run.";
    };
  };
  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
  };
}
