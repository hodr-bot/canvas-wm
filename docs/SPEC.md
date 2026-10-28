# canvas-wm SPEC

Infinite-canvas Wayland compositor. Floating windows only, game-feel camera.

## Lot 0 (current)

Canvas core builds — arbitrary placement, Super overview with
multi-select kept in view, XWayland + multi-output planned, no status bar
(chrome lives on the windows themselves).

- `Super` tap: slight zoom-out overview with style (animation lands in lot 1)
- Left-click in overview: toggle window selection
- `Super` again: keep selection in view via temporary fit-view from approach side
- `Esc` / canvas click: back to previous multi-window view
- Directional focus without wrap (`S+D` = bottom-right or nothing)
- XWayland on, multi-output = viewports on one shared canvas
- No zones (rejected), drawings/effects in V2

## Power saving

Eco mode keeps minimal animations only: short fades, no zoom flights.
It degrades gracefully — in-flight transitions finish or cut cleanly
with an on-screen notice first, never yanked mid-flight without warning.
