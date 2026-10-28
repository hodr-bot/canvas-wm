# NixOS system config for canvas-wm (no home-manager needed).
# Paste into configuration.nix; replace <path-to> with the flake checkout.
#
# Pure configuration.nix snippet (flake input wiring done once in your
# flake.nix with `inputs.canvas-wm.url = "github:hodr-bot/canvas-wm";`
# and `canvas-wm.nixosModules.default` added to `modules`):
#   services.canvas-wm.enable = true;
{
  inputs.canvas-wm.url = "github:hodr-bot/canvas-wm";

  outputs = { nixpkgs, canvas-wm, ... }: {
    nixosConfigurations.nixos = nixpkgs.lib.nixosSystem {
      modules = [
        canvas-wm.nixosModules.default
        { services.canvas-wm.enable = true; }
      ];
    };
  };
}
