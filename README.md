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
Bar proof (`docs/e2e-bar.png`): internal bar client maps first as overlay
window 0, full-width strip with one green chip per canvas window.
Verified live: chip click focuses without leaking the button to clients,
canvas click delivers enter at exact local coords + button 272.
XWayland proof (`docs/e2e-xwayland.png`): real xeyes under rootless Xwayland,
linked zero-copy via WL_SURFACE_ID to canvas 1, buffer-sized 152x102
(no stretch), pupils track the pointer.

V1 photo series (all live under Xvfb, `docs/v1-*.png`):
`v1-base` — two native clients in cascade + minimal bar (2 chips);
`v1-overview` — Super tap, camera ×0.7, background deepens;
`v1-select` — left-click window 1 then window 2 in overview
(log: `click window 1 … selected=1`, `click window 2 … selected=2`);
`v1-fit` — re-Super keeps both in view
(log: `QuitWithFit … zoom=2.00 selected=2`, both windows fill the frame);
`v1-xwayland` — xeyes alone, white eyes + pupils tracking the pointer.
