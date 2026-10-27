# canvas-wm

Infinite-canvas Wayland compositor. Floating windows only, game-feel camera.

Lot 0 (this): canvas core builds — arbitrary placement, Super overview with
multi-select kept in view, XWayland + multi-output planned, minimal status bar.

- `Super` tap: slight zoom-out overview with style (animation lands in lot 1)
- Left-click in overview: toggle window selection
- `Super` again: keep selection in view via temporary fit-view from approach side
- `Esc` / canvas click: back to previous multi-window view
- Directional focus without wrap (`S+D` = bottom-right or nothing)
- XWayland on, multi-output = viewports on one shared canvas
- No zones (rejected), drawings/effects in V2

```bash
cargo test
cargo run
# Full compositor (window + input pump + GLES render):
nix build .#compositor
./result/bin/canvas-wm --winit --frames 120
```

NixOS (system module, no home-manager required): see `nix/module.nix`.

E2E proof (`docs/e2e-render.png`): real xdg client under Xvfb, dark canvas +
solid client window rendered at its cascade position through the camera.
