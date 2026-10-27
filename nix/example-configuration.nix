# NixOS system config for canvas-wm (no home-manager needed).
# Paste into configuration.nix; replace <path-to> with the flake checkout.
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
