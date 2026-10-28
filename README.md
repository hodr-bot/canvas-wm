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
- Power saving: eco mode keeps minimal animations only (short fades,
  no zoom flights), and degrades gracefully — in-flight transitions
  finish or cut cleanly with an on-screen notice first, never yanked
  mid-flight without warning

```bash
cargo test
cargo run
# Full compositor (window + input pump + GLES render):
nix build .#compositor
./result/bin/canvas-wm --winit --frames 120
# Manual run with X clients (polls for readiness, no fixed sleep):
scripts/launch-with-clients.sh xterm xcalc
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

Native Wayland (`docs/native-foot.png`, `docs/native-dnd.png`):
foot terminal + weston-terminal map as pure Wayland clients
(`xdg_shell` v3 for GTK4-era clients, own `wl_data_device_manager`
v3 since Smithay 0.3 hides its DnD grab). Clipboard round-trips
live: `wl-copy` sets, a minimal C probe pastes
(`PASTE-DATA:clipboard-test-73`, exit 0). Drag-and-drop completes:
weston-dnd `enter → motion ×6 → drop → dnd_drop_performed`,
server logs `drag grab installed` + `drop completed (accepted=true)`.

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
fullscreen (true fullscreen: the client is resized to the view, second
click restores pixel-identical); red kills
the X client and its canvas window vanishes. Glyphs (X / ⛶) land later;
colour + position carry the meaning for now. No move button: the 1s dwell
already arms moves, a third button would duplicate it.

3D planes (`docs/planes-flat.png`, `docs/planes-tilt.png`): windows render
as textured perspective planes through our own GL program (Smithay only
imports the shm buffers). Idle = flat 1:1, text readable, typing reaches
X clients. Super canvas mode = light tilt (8°/4° about the view centre,
centre pixel fixed); the logic plane stays 2D, presentation is 3D. Cost is
~zero: same textures, same fill rate, one matrix per window. Non-shm
buffers keep Smithay's flat path as fallback.

Hover keys, Super mode only, no modifiers (`docs/keys-*.png`): `F`
fullscreen (the client is resized to the view), `Q` close, `E` grab-move
(window follows the pointer while held, no button needed), `R` resize
mode while held (left/right drag on edges and corners grows the rect,
min 60px, never snaps back on repaint). Plain Idle typing never triggers
anything: every key reaches the app untouched unless the pointer hovers
a window in Super mode.
