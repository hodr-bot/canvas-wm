# canvas-wm

Infinite-canvas Wayland compositor. Floating windows only, game-feel camera.

Lot 0 (this): canvas core builds — arbitrary placement, Super overview with
multi-select kept in view, XWayland + multi-output planned, no status bar
(chrome lives on the windows themselves).

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
XWayland proof (`docs/e2e-xwayland.png`): real xeyes under rootless Xwayland,
linked zero-copy via WL_SURFACE_ID to canvas 1, buffer-sized 152x102
(no stretch), pupils track the pointer.

V1 photo series (all live under Xvfb, `docs/v1-*.png`):
`v1-base` — two native clients in cascade, no chrome anywhere;
`v1-overview` — Super tap, camera ×0.7, background deepens;
`v1-select` — left-click window 1 then window 2 in overview
(log: `click window 1 … selected=1`, `click window 2 … selected=2`);
`v1-fit` — re-Super keeps both in view
(log: `QuitWithFit … zoom=2.00 selected=2`, both windows fill the frame);
`v1-xwayland` — xeyes alone, white eyes + pupils tracking the pointer.

Real apps (`docs/v1-real-apps.png`, `docs/v1-real-overview.png`):
xterm with typed `echo toile-infinie-V1` (keyboard works end-to-end
through seat/XTEST), xclock analog face, xcalc with full labels —
all live XWayland clients with core fonts + Xft configured headless.
Fixes that made it possible: frame callbacks (`wl_callback.done`
every presented frame — Xwayland throttles damage without them) and
damage-only commit re-import from the kept shm buffer.

Live canvas (`docs/v1-drag.png`, `docs/v1-zoom.png`, `docs/v1-pan.png`):
left-drag past 6 world px moves the window (single-jump drags included),
wheel over empty canvas zooms at the pointer, middle-drag pans the camera;
wheel over a window still scrolls the client.

Controls v2 (`docs/v2-armed-move.png`, `docs/v2-canvas-pan.png`): plain
left never moves windows (pixel-identical before/after); Super toggles
canvas mode with no zoom change; 1s dwell inside a window arms it, then
drag moves it Hyprland-style; drag on empty space pans the canvas (offset
+ velocity fields, camera fixed); wheel zoom unchanged.

Window chrome (`docs/chrome-hover.png`, `docs/chrome-full.png`,
`docs/chrome-closed.png`): no bar anywhere — hovering near a window's
top-right corner reveals two squares (red close, blue fullscreen, drawn by
a minimal solid-colour GL program that seeds the 3D engine). Blue toggles
fullscreen (edge-to-edge, second click restores pixel-identical); red kills
the X client and its canvas window vanishes. Glyphs (X / ⛶) land later;
colour + position carry the meaning for now. No move button: the 1s dwell
already arms moves, a third button would duplicate it.
