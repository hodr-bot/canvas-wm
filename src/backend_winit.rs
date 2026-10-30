//! Winit dev backend (feature `compositor` only).
//!
//! Opens a window titled `canvas-wm`, renders the canvas background color
//! derived from the camera, and pumps Smithay input events. Keysym mapping
//! (Super-tap) and session wiring land next; this proves the window,
//! renderer, and event-pump path end to end.

use std::cell::RefCell;
use std::error::Error;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::Duration;

use smithay::backend::{
    input::{
        Axis, AxisSource as InputAxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, MouseButton, PointerAxisEvent, PointerButtonEvent,
        PointerMotionAbsoluteEvent,
    },
    renderer::{buffer_type, BufferType, Frame, ImportDma, ImportShm, Texture, Transform},
    winit::{self, WinitEvent, WinitInputError},
};
use smithay::backend::allocator::{dmabuf::Dmabuf, Buffer as BufferDims};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_pointer::{Axis as WlAxis, AxisSource};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Buffer as BufferCoord, Physical, Rectangle, Size};
use smithay::wayland::compositor::{with_states, BufferAssignment, SurfaceAttributes};
use smithay::wayland::seat::AxisFrame;
use smithay::wayland::shm::with_buffer_contents;
use smithay::xwayland::{XWayland, XWaylandEvent};

use crate::canvas::{window_screen_rect, Camera};
use crate::keys::{is_super, KeyMapper, TapTracker, KEY_Escape};
use crate::server::{Server, ServerState};
use crate::session::{drag_offset, Session, SuperEvent};
use crate::xwm::Xwm;

/// X display sockets before XWayland starts, to spot its display number.
fn x11_sockets() -> Vec<String> {
    std::fs::read_dir("/tmp/.X11-unix")
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// winit mouse buttons to evdev codes clients expect.
fn button_code(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 0x110,
        MouseButton::Right => 0x111,
        MouseButton::Middle => 0x112,
        MouseButton::Other(code) => 0x110 + code as u32,
    }
}

/// Pick the destination of a key release: the XTEST xid recorded at
/// press time when there is one (a press in A, a click on B, then the
/// release must still reach A or A's key sticks down), otherwise the
/// xid under the current focus (press predates us). None both ways
/// means the wl seat. Pure so the anti-stick rule stays test-locked.
fn resolve_release(
    routes: &std::collections::HashMap<u32, Option<u32>>,
    evdev: u32,
    focus_xid: Option<u32>,
) -> Option<u32> {
    match routes.get(&evdev) {
        Some(recorded) => *recorded,
        None => focus_xid,
    }
}

/// Memorise a press destination, first press wins: auto-repeats
/// re-fire pressed while the key is held and must NOT re-route after
/// a focus change, or the release follows the repeat instead of the
/// press and the first window sticks. Returns the recorded dest.
fn note_press(
    routes: &mut std::collections::HashMap<u32, Option<u32>>,
    evdev: u32,
    focus_xid: Option<u32>,
) -> Option<u32> {
    *routes.entry(evdev).or_insert(focus_xid)
}

/// Tell a client its size in SCREEN px so the buffer follows (real
/// resize, never a stretch-zoom): X via ConfigureWindow, natives via
/// configure. `fullscreen` carries the state flag for natives; X
/// ignores it. The size is explicit (not read off the canvas rect)
/// because canvas px != screen px off zoom 1: a fullscreen client
/// owns exactly the view, whatever the live zoom.
fn push_client_size(
    server: &crate::server::Server,
    xwm: Option<&Xwm>,
    id: u64,
    win: &crate::server::ClientWindow,
    fullscreen: bool,
    px: (u32, u32),
) {
    let (w, h) = (px.0 as i32, px.1 as i32);
    if let Some(xid) = win.x11 {
        if let Some(wm) = xwm {
            wm.configure(xid, w as u32, h as u32);
        }
        return;
    }
    if let Some(surf) = win.surface.clone() {
        if !server.configure_native_stable(&surf, w, h, fullscreen) {
            crate::xdg_v3::configure_toplevel(id, w, h, fullscreen);
        }
    }
}

/// Demo props (CANVAS_DEMO_PROPS=1): boxes and dust around the origin
/// showing parallax + inverse inertia. Off by default: zero props,
/// zero cost, zero clutter. Windows are never props (always rigid).
fn demo_props() -> Vec<crate::canvas::Prop> {
    use crate::canvas::{Prop, Vec2};
    let cam = Vec2::new(0.0, 0.0);
    vec![
        Prop::parallax(Vec2::new(-320.0, -200.0), 0.25, 26.0, [0.35, 0.45, 0.75, 1.0]),
        Prop::parallax(Vec2::new(300.0, 180.0), 0.5, 18.0, [0.35, 0.45, 0.75, 1.0]),
        Prop::laggy(Vec2::new(-160.0, 120.0), 0.8, 22.0, cam, 54.0, [0.85, 0.55, 0.25, 1.0]),
        Prop::laggy(Vec2::new(160.0, -140.0), 0.5, 40.0, cam, 44.0, [0.45, 0.75, 0.55, 1.0]),
        Prop::laggy(Vec2::new(0.0, 220.0), 1.0, 14.0, cam, 64.0, [0.75, 0.35, 0.45, 1.0]),
        Prop::rigid(Vec2::new(0.0, -220.0), 36.0, [0.55, 0.55, 0.60, 1.0]),
    ]
}
/// Background color of the canvas, derived from the camera.
/// Dark slate that deepens as you zoom out, so overview reads as a void.
pub fn background_color(camera: &Camera) -> [f32; 4] {
    let t = ((camera.zoom - 0.05) / 1.95).clamp(0.0, 1.0) as f32;
    let base = 0.04 + 0.05 * t;
    [base * 0.9, base, base * 1.3, 1.0]
}

/// Draw one menu text line: smooth vector glyphs from the fontdue
/// atlas (coverage alpha, kerning included). Screen space, y down;
/// `u` is the output scale, the face is laid out at MENU_PX and the
/// quads are scaled to physical px (linear filtering smooths them).
#[allow(clippy::too_many_arguments)]
unsafe fn draw_menu_line(
    gl: &smithay::backend::renderer::gles2::ffi::Gles2,
    tprog: (u32, i32, i32, i32, u32, u32),
    mvp: &[f32; 16],
    atlas: u32,
    cache: &mut crate::fontvec::FontCache,
    x: f32,
    y: f32,
    u: f32,
    text: &str,
) {
    let (_, glyphs) = cache.layout_line(text);
    for g in glyphs {
        let slot = match cache.slot(g.ch) {
            Some(s) if s.w > 0 => *s,
            _ => continue,
        };
        let x0 = x + g.x * u;
        let y0 = y + g.y * u;
        let x1 = x0 + g.w * u;
        let y1 = y0 + g.h * u;
        let verts: [f32; 16] = [
            x0, y0, slot.u0, slot.v0, //
            x1, y0, slot.u1, slot.v0, //
            x0, y1, slot.u0, slot.v1, //
            x1, y1, slot.u1, slot.v1,
        ];
        crate::render3d::draw_textured(gl, tprog, mvp, atlas, &verts, false);
    }
}

/// Buffer size whatever the sharing mode: shm via its contents, dmabuf
/// via its handle. None for anything else (never imported, never drawn,
/// so exotic buffers warn once instead of breaking the frame).
fn client_buffer_size(buffer: &WlBuffer) -> Option<(i32, i32)> {
    match buffer_type(buffer) {
        Some(BufferType::Shm) => {
            with_buffer_contents(buffer, |_, data| (data.width, data.height)).ok()
        }
        Some(BufferType::Dma) => buffer
            .as_ref()
            .user_data()
            .get::<Dmabuf>()
            .map(|d| (d.width() as i32, d.height() as i32)),
        _ => None,
    }
}

/// Open the dev window and run until closed or `max_frames` is reached.
/// `max_frames` exists for headless smoke tests (e.g. under Xvfb).
pub fn run(max_frames: Option<u64>) -> Result<(), Box<dyn Error>> {
    let (mut graphics, mut input) = winit::init(None)?;
    graphics.window().set_title("canvas-wm");

    let mut mapper = KeyMapper::new().ok_or("xkb keymap unavailable (missing xkb-data?)")?;
    let mut session = Session::new();
    let mut tap = TapTracker::default();
    // Client windows land here as xdg-toplevels connect.
    let mut server_state = ServerState::default();
    // GPU formats the GL driver can import: advertised on zwp_linux_dmabuf
    // so GL toolkits (Kate, Alacritty) can present. Empty on drivers
    // without dma-buf (llvmpipe headless): the global is still announced.
    let dmabuf_formats: Vec<smithay::backend::allocator::Format> =
        graphics.renderer().dmabuf_formats().cloned().collect();
    log::info!("renderer offers {} dmabuf formats", dmabuf_formats.len());
    let (mut server, socket) =
        Server::new(dmabuf_formats).map_err(|e| format!("wayland bind: {}", e))?;
    log::info!("wayland socket ready on {:?}", socket);
    // Logical size of the winit window; refreshed every frame below
    // (fullscreen/resize changes it).
    let mut view = crate::canvas::Vec2::new(1280.0, 800.0);
    let mut last_view = view;
    let mut pointer = crate::canvas::Vec2::new(640.0, 400.0);
    let mut frames: u64 = 0;
    // Window ids the seat already knows about; a change re-asserts focus so
    // late-bound clients receive keyboard Enter on the newest window.
    let mut known_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();
    // Focused canvas window.
    let mut focused_id: Option<u64> = None;
    // Press routing memory: evdev keycode -> XTEST xid at press time
    // (None = the wl seat). A release follows its PRESS destination,
    // never the current focus: press in A, click B, release must reach
    // A or A's key sticks down forever (repeat). Releases with no
    // recorded press (predates us) fall back to the current focus.
    let mut key_routes: std::collections::HashMap<u32, Option<u32>> =
        std::collections::HashMap::new();
    // Popup holding keyboard focus: (popup id, parent id). A press inside
    // a menu/tooltip focuses its surface so menus take keys — but when the
    // popup dies (tooltip timeout, menu dismiss) the seat would point at a
    // dead surface and the app reads as frozen (no more keyboard until the
    // next click). The per-frame check below folds focus back to the parent
    // as soon as the popup is gone.
    let mut popup_focus: Option<(u64, u64)> = None;
    // Live canvas gestures: left-drag on empty canvas space pans the canvas
    // (Canvas mode only); a dwell-armed window drag moves that window.
    // Press canvas coords + last canvas coords + whether past threshold.
    let mut grab: Option<(u64, crate::canvas::Vec2, crate::canvas::Vec2, bool)> = None;
    let mut panning: Option<crate::canvas::Vec2> = None;
    // Hover chrome: last frame's revealed buttons (window id + layout).
    let mut last_chrome: Option<(u64, crate::chrome::ChromeLayout)> = None;
    // Hover keyboard shortcuts (Canvas mode, or Super held in Idle):
    // held-modifier set, F/Q repeat guards, E grab-move (window id +
    // last canvas pointer), R resize mode. The modifier set (not a
    // counter) stays correct across key repeats: repeats re-insert an
    // already-present code instead of drifting a count upward forever.
    let mut held_mods: std::collections::HashSet<u32> =
        std::collections::HashSet::new();
    let mut super_held = false;
    let mut f_held = false;
    let mut q_held = false;
    let mut r_held = false;
    let mut k_held = false;
    // Toy marbles (K key, Canvas mode): canvas-anchored physics bodies.
    let mut marbles: Vec<crate::marbles::Marble> = Vec::new();
    let mut marble_seed: u64 = 0xC0BEu64;
    // Frame counter: phases the far-marble LOD steps.
    let mut frame_no: u64 = 0;
    let mut prev_canvas_offset = session.canvas.offset;
    let mut e_grab: Option<(u64, crate::canvas::Vec2)> = None;
    // Centre-anchored scale (R held): window id + pinned centre +
    // size at grab + mouse canvas pos at grab + last pushed client
    // size (configure only on integer change).
    let mut r_scale: Option<(u64, crate::canvas::Vec2, crate::canvas::Vec2, crate::canvas::Vec2, (u32, u32))> =
        None;
    // Solid-colour GL program for window chrome (seed of the 3D engine).
    let mut solid_prog: Option<(u32, i32, i32, u32)> = None;
    // Rounded-rect GL program for the Options panel (SDF corners).
    let mut round_prog: Option<(u32, i32, i32, i32, i32, u32, u32)> = None;
    // Textured GL program: windows as perspective planes in 3D.
    let mut tex_prog: Option<(u32, i32, i32, i32, u32, u32)> = None;
    // Options menu + user settings (Options menu edits these live).
    let mut settings = crate::settings::Settings::default();
    session.super_dolly = settings.dolly_factor();
    let mut menu = crate::menu::MenuState::default();
    // Slide-in animation clock: set when the menu opens, cleared on
    // close; the panel drops a few px over ~0.18s.
    let mut menu_opened_at: Option<std::time::Instant> = None;
    // Ambient background: compiled program, animation clock (seconds at
    // current speed; frozen in eco), and last repaint for the slow-drift
    // throttle. Off = flat clear, zero shader cost.
    let mut bg_prog: Option<crate::render3d::BgProg> = None;
    let mut bg_failed = false;
    let mut bg_time: f32 = 0.0;
    // Super edge-glow clocks: own flow time (eco-frozen like the
    // wallpaper) plus a fade gliding with Canvas mode.
    let mut glow_time: f32 = 0.0;
    let mut glow_f: f32 = 0.0;
    // Wave field clock: own travel time (eco-frozen with the rest).
    let mut wave_time: f32 = 0.0;
    let mut last_bg_paint = std::time::Instant::now();
    // Vector menu face (DejaVu via fontdue) with its self-packing
    // coverage atlas; upload happens below, re-upload on dirty.
    let mut font_cache = crate::fontvec::FontCache::new();
    // Glyph atlas texture for menu text (uploaded once, 512x512 RGBA).
    let mut atlas_tex: Option<
        smithay::backend::renderer::gles2::Gles2Texture,
    > = None;
    // Canvas-mode press point: released without moving = plain canvas click.
    let mut pan_start: Option<crate::canvas::Vec2> = None;

    // calloop loop just for the XWayland source; polled every frame.
    let mut event_loop: EventLoop<()> =
        EventLoop::try_new().map_err(|e| format!("calloop: {}", e))?;
    let x11_ready: Rc<RefCell<Option<UnixStream>>> = Rc::new(RefCell::new(None));
    let (xwayland, xwayland_source) = XWayland::new(event_loop.handle(), server.display(), None);
    {
        let x11_ready = Rc::clone(&x11_ready);
        event_loop
            .handle()
            .insert_source(xwayland_source, move |event, &mut (), _| {
                if let XWaylandEvent::Ready { connection, .. } = event {
                    *x11_ready.borrow_mut() = Some(connection);
                }
            })
            .map_err(|e| format!("xwayland source: {}", e))?;
    }
    let x11_before = x11_sockets();
    xwayland
        .start()
        .map_err(|e| format!("Xwayland spawn: {}", e))?;
    let mut xwm: Option<Xwm> = None;

    // Unmatched X surface links wait here; the surface may be created
    // server-side after the client message arrives.
    let mut pending_x: Vec<(u32, u32)> = Vec::new();
    let frame_clock = std::time::Instant::now();
    // Camera flight clock: last spring step (dt source for smooth moves).
    let mut last_cam_step = std::time::Instant::now();
    // WASD repeat guards (W/A/S/D order): held repeats re-hit the same
    // target, so only fresh presses act — releases clear the guard.
    let mut wasd_held = [false; 4];
    // Canvas props (decor layer): empty unless CANVAS_DEMO_PROPS=1.
    // Windows are never props — always rigid, never listening.
    let mut props: Vec<crate::canvas::Prop> = if std::env::var("CANVAS_DEMO_PROPS").is_ok() {
        let demo = demo_props();
        log::info!("demo props: {}", demo.len());
        demo
    } else {
        Vec::new()
    };
    // Headless proof hook (screenshots): enter Canvas without a keyboard.
    // Same path as a real Super tap (overview, dolly, dwell all armed).
    if std::env::var("CANVAS_FORCE_CANVAS").is_ok() {
        let action = session.on_event(
            crate::session::SuperEvent::SuperTap,
            &|wid| server_state.windows.get(&wid).map(|c| c.canvas),
            view,
        );
        log::info!("forced Canvas for proof: {:?}", action);
    }
    // Headless proof hook (frame-callback pacing): freeze FX like the
    // Eco toggle does, so pending client callbacks are the ONLY thing
    // that can force a frame. If the probe still paints fast, pacing
    // works; if it stalls, paced clients (Kate) would freeze.
    if std::env::var("CANVAS_FORCE_ECO").is_ok() {
        settings.eco = true;
        log::info!("forced eco for proof: FX frozen");
    }
    // Headless proof hooks (screenshots): pin motif / waveform by index
    // so new shader branches are visible without a harness keyboard.
    if let Ok(n) = std::env::var("CANVAS_FORCE_PATTERN") {
        settings.bg_pattern = n.parse().unwrap_or(1).min(7);
        log::info!("forced bg_pattern for proof: {}", settings.bg_pattern);
    }
    if let Ok(n) = std::env::var("CANVAS_FORCE_WSHAPE") {
        settings.wave_shape = n.parse().unwrap_or(0).min(5);
        log::info!("forced wave_shape for proof: {}", settings.wave_shape);
    }
    // Headless proof hook (screenshots): pre-drop marbles across the
    // opening view so the fall/roll/rest cycle is visible without a
    // working harness keyboard.
    if let Ok(n) = std::env::var("CANVAS_SPAWN_MARBLES")
        .or_else(|_| std::env::var("CANVAS_SPAWN_CUBES"))
    {
        let count: usize = n.parse().unwrap_or(3).min(8);
        for i in 0..count {
            let fx = 0.3 + 0.2 * (i as f64 % 3.0);
            let fy = 0.35 + 0.15 * ((i / 3) as f64 % 3.0);
            let at = session.to_canvas(
                crate::canvas::Vec2::new(view.x * fx, view.y * fy),
                view,
            );
            marble_seed = marble_seed
                .wrapping_mul(0x9E3779B97F4A7C15)
                .wrapping_add(0xC0BE + i as u64);
            crate::marbles::add_marble(
                &mut marbles,
                crate::marbles::spawn(at.x, at.y, marble_seed),
            );
        }
        log::info!("proof marbles spawned: {}", marbles.len());
    }

    loop {
        if let Err(e) = event_loop.dispatch(Some(Duration::from_millis(0)), &mut ()) {
            log::warn!("calloop dispatch: {}", e);
        }
        // XWayland ready => become its window manager (once).
        if xwm.is_none() {
            if let Some(stream) = x11_ready.borrow_mut().take() {
                match Xwm::new(stream) {
                    Ok(wm) => {
                        for sock in x11_sockets() {
                            if !x11_before.contains(&sock) {
                                if let Some(num) = sock.strip_prefix('X') {
                                    let display = format!(":{}", num);
                                    std::env::set_var("DISPLAY", &display);
                                    log::info!("XWayland ready on DISPLAY={}", display);
                                }
                            }
                        }
                        xwm = Some(wm);
                    }
                    Err(e) => log::warn!("Xwm failed: {}", e),
                }
            }
        }
        if let Some(wm) = xwm.as_mut() {
            wm.poll();
        }
        server.pump(&mut server_state);
        // Link X windows to their Wayland surfaces (zero-copy). Fresh links
        // from the WM plus retries; surfaces may lag the client message.
        if let Some(wm) = xwm.as_mut() {
            pending_x.extend(wm.surface_links.drain(..));
            let mut still_pending = Vec::new();
            for (xid, sid) in pending_x.drain(..) {
                match server_state.surface_by_id(sid) {
                    Some(surface) => {
                        if let Some(id) = server_state
                            .windows
                            .iter()
                            .find(|(_, c)| c.x11 == Some(xid))
                            .map(|(id, _)| *id)
                        {
                            server_state.windows.get_mut(&id).unwrap().surface = Some(surface);
                        } else {
                            let id = server_state.add_x11_window(surface, xid);
                            log::info!("linked X window {} -> canvas {}", xid, id);
                        }
                    }
                    None => still_pending.push((xid, sid)),
                }
            }
            pending_x = still_pending;
            // Drop canvas windows whose X window died.
            for (xid, sid) in wm.dead.drain(..) {
                if let Some(id) = server_state
                    .windows
                    .iter()
                    .find(|(_, c)| c.x11 == Some(xid))
                    .map(|(id, _)| *id)
                {
                    server_state.windows.remove(&id);
                }
                if let Some(sid) = sid {
                    server_state.forget_surface(sid);
                }
            }
        }
        // Mirror canvas raises into X stacking order: X button delivery
        // targets the X-topmost window at the pointer, so a canvas raise
        // without its X mirror sends clicks to the wrong client wherever
        // X windows overlap in root space.
        for xid in server_state.take_restacks() {
            if let Some(wm) = xwm.as_ref() {
                wm.restack_above(xid);
            }
        }
        // Newest canvas window steals keyboard focus, like any floating WM.
        let ids: std::collections::HashSet<u64> = server_state.windows.keys().copied().collect();
        if ids != known_ids {
            known_ids = ids;
            let newest = server_state.canvas_ids().iter().copied().max();
            let surface = newest
                .and_then(|id| server_state.windows.get(&id))
                .and_then(|c| c.surface.clone());
            // Bounce through None so Enter is re-sent even when the
            // surface is unchanged (clients that bound late missed it).
            server.focus_keyboard(None);
            server.focus_keyboard(surface.as_ref());
            focused_id = newest;
            // X-backed focus also moves the X input focus.
            if let Some(id) = newest {
                let xid = server_state
                    .windows
                    .get(&id)
                    .and_then(|c| c.x11);
                if let (Some(wm), Some(xid)) = (xwm.as_ref(), xid) {
                    wm.focus(xid);
                    server.focus_keyboard(None);
                }
            }
        }
        let mut need_render = false;
        // Output size, refreshed every frame BEFORE input: going
        // fullscreen/resize changes the logical view, and every mapping
        // hangs off it (ortho projection, pick centres, seat origins).
        // A stale view scales all motion (2x on a 2K fullscreen) and
        // offsets every native click.
        {
            let ws = graphics.window_size();
            view = crate::canvas::Vec2::new(
                ws.physical_size.w as f64 / ws.scale_factor,
                ws.physical_size.h as f64 / ws.scale_factor,
            );
            if view != last_view {
                log::info!(
                    "view {:.0}x{:.0} scale={:.2}",
                    view.x,
                    view.y,
                    ws.scale_factor
                );
                last_view = view;
                need_render = true;
            }
        }
        // Fullscreen windows cover the LIVE view every frame: the rect
        // is a function of view + live zoom, never a toggle-time
        // snapshot. F pressed in Super (dolly 0.9) or mid wheel-glide
        // otherwise bakes a canvas rect that no longer fills the view
        // once the zoom lands — the app then looks "zoomed into".
        // Refit here keeps rect * zoom == view exactly at any zoom, so
        // the client buffer (view px, pushed once at toggle) always
        // displays 1:1. Idempotent at rest: no extra frames. The saved
        // pre-fullscreen rect is untouched, toggle-off still restores.
        {
            let mut fs_moved = false;
            for win in server_state.windows.values_mut() {
                if win.fullscreen_prev.is_some() {
                    let rect = crate::chrome::fullscreen_rect_for(view, &|s| {
                        session.to_canvas(s, view)
                    });
                    if win.canvas.pos != rect.pos || win.canvas.size != rect.size {
                        win.canvas.pos = rect.pos;
                        win.canvas.size = rect.size;
                        fs_moved = true;
                    }
                }
            }
            if fs_moved {
                need_render = true;
            }
        }
        match input.dispatch_new_events(|event| match event {
            InputEvent::Keyboard { event } => {
                let pressed = matches!(event.state(), KeyState::Pressed);
                let (sym, evdev) = mapper.resolve(event.key_code(), pressed);
                log::debug!(
                    "key scancode={} sym={} evdev={} pressed={}",
                    event.key_code(),
                    sym,
                    evdev,
                    pressed
                );
                // Options menu open: it owns nav presses (Up/Down/Enter nav,
                // Esc closes). Releases still reach the app so nothing
                // sticks. Anything else falls through to normal handling:
                // swallowing every shortcut while the menu is open made
                // all of F/Q/E/R/WASD/Super look dead after opening it.
                if menu.open {
                    if sym == KEY_Escape && pressed {
                        menu.open = false;
                        log::info!("menu closed");
                        need_render = true;
                        return;
                    }
                    if pressed {
                        match evdev {
                            crate::menu::KEY_UP => {
                                menu.up();
                                need_render = true;
                                return;
                            }
                            crate::menu::KEY_DOWN => {
                                menu.down();
                                need_render = true;
                                return;
                            }
                            crate::menu::KEY_LEFT => {
                                if let Some(id) = menu.adjust(-1, &mut settings) {
                                    log::info!("menu option {} changed", id);
                                }
                                session.super_dolly = settings.dolly_factor();
                                need_render = true;
                                return;
                            }
                            crate::menu::KEY_RIGHT => {
                                if let Some(id) = menu.adjust(1, &mut settings) {
                                    log::info!("menu option {} changed", id);
                                }
                                session.super_dolly = settings.dolly_factor();
                                need_render = true;
                                return;
                            }
                            crate::menu::KEY_ENTER => {
                                if let Some(id) =
                                    crate::menu::MenuState::activate_at(
                                        menu.tab,
                                        menu.selected,
                                        &mut settings,
                                    )
                                {
                                    log::info!("menu option {} changed", id);
                                }
                                session.super_dolly = settings.dolly_factor();
                                need_render = true;
                                return;
                            }
                            crate::menu::KEY_TAB => {
                                menu.next_tab();
                                log::info!("menu tab {}", crate::menu::TABS[menu.tab]);
                                need_render = true;
                                return;
                            }
                            _ => {}
                        }
                    } else {
                        // Release while open: follow the press destination
                        // (anti-stick), current focus only as fallback.
                        let focus_xid = focused_id
                            .and_then(|id| server_state.windows.get(&id))
                            .and_then(|c| c.x11);
                        let xid = resolve_release(&key_routes, evdev, focus_xid);
                        key_routes.remove(&evdev);
                        match (xwm.as_ref(), xid) {
                            (Some(wm), Some(xid)) => wm.xtest_key(xid, evdev, false),
                            _ => server.input_key(evdev, false),
                        }
                        return;
                    }
                }
                if sym == KEY_Escape && pressed {
                    let action = session.on_event(
                        SuperEvent::Esc,
                        &|wid| server_state.windows.get(&wid).map(|c| c.canvas),
                        view,
                    );
                    log::info!(
                        "super {:?} state={:?} zoom={:.2} selected={}",
                        action,
                        session.state,
                        session.camera.zoom,
                        session.overview.selected.len()
                    );
                    need_render = true;
                } else if is_super(sym) {
                    if pressed {
                        super_held = true;
                        tap.note_super_press();
                    } else {
                        super_held = false;
                        if tap.note_super_release() {
                            let action = session.on_event(
                                SuperEvent::SuperTap,
                                &|wid| server_state.windows.get(&wid).map(|c| c.canvas),
                                view,
                            );
                            log::info!(
                                "super {:?} state={:?} zoom={:.2} selected={}",
                                action,
                                session.state,
                                session.camera.zoom,
                                session.overview.selected.len()
                            );
                            need_render = true;
                        }
                    }
                } else {
                    if pressed {
                        tap.note_other_press();
                    }
                    // Modifier tracking as a set: key repeats re-insert the
                    // same code (idempotent) instead of drifting a counter
                    // upward and killing every later bare shortcut.
                    if crate::chrome::MOD_CODES.contains(&evdev) {
                        if pressed {
                            held_mods.insert(evdev);
                        } else {
                            held_mods.remove(&evdev);
                        }
                    }
                    // Options menu toggle: Super + physical comma (macOS
                    // Cmd+,) or Super + physical I (Windows Win+I) — or
                    // the bare key in Canvas mode, where apps are frozen
                    // and no Super hold is needed. Positional codes, so it
                    // holds on any layout, and Super-gated in Idle, so app
                    // keys (Ctrl+,) are never stolen.
                    // Consumed — never reaches the app.
                    if pressed
                        && (super_held
                            || session.state == crate::session::SuperState::Canvas)
                        && (evdev == crate::menu::KEY_COMMA || evdev == crate::menu::KEY_I)
                    {
                        menu.open = !menu.open;
                        menu_opened_at = if menu.open {
                            Some(std::time::Instant::now())
                        } else {
                            None
                        };
                        log::info!("menu open={}", menu.open);
                        need_render = true;
                        return;
                    }
                    // Hover shortcuts (Canvas mode, or Super held in Idle
                    // so hold-Super+key works like a normal WM): F
                    // fullscreen, Q close, E grab-move, R resize mode.
                    // Consumed — never reaches the app.
                    // Idle sends every key to the app untouched, so normal
                    // typing never triggers an action.
                    let mut consumed = false;
                    if session.state == crate::session::SuperState::Canvas || super_held {
                        // WASD spatial focus (positional codes = the physical
                        // gamer cluster, ZQSD-labelled on azerty): D pulls
                        // focus to the window on the right, A left, W up,
                        // S down — strictly in that half-plane, or nothing.
                        // Canvas-modal and consumed, so apps never see it.
                        // Keyboard window move: Shift + WASD shoves the
                        // focused window (else the hovered one) by a fixed
                        // canvas step — every press AND auto-repeat, so
                        // holding the key glides it. Deliberately outside
                        // the `wasd_held` guard below: repeats ARE the
                        // motion here. Size-independent by construction:
                        // the step never depends on the window, so big and
                        // small windows travel at the same speed.
                        // Raise + select, like the E-grab.
                        // Canvas-modal (or Super held) and consumed.
                        const WASD_KEYS: [u32; 4] = [17, 30, 31, 32];
                        const WASD_DIRS: [(f64, f64, &str); 4] = [
                            (0.0, -1.0, "north"), // W
                            (-1.0, 0.0, "west"),  // A
                            (0.0, 1.0, "south"),  // S
                            (1.0, 0.0, "east"),   // D
                        ];
                        /// Keyboard shove per press/repeat, canvas px.
                        const KBD_MOVE_STEP: f64 = 32.0;
                        let shift_down =
                            held_mods.contains(&42) || held_mods.contains(&54);
                        if pressed && shift_down {
                            if let Some(wi) =
                                WASD_KEYS.iter().position(|c| *c == evdev)
                            {
                                // Swallow the focus guard too: a held D must
                                // not refocus when Shift is released first.
                                wasd_held[wi] = true;
                                let (dx, dy, name) = WASD_DIRS[wi];
                                let cworld = session.to_canvas(pointer, view);
                                let target = focused_id
                                    .or_else(|| server_state.topmost_at(cworld));
                                match target {
                                    Some(id) => {
                                        match server_state.windows.get_mut(&id) {
                                            Some(cw) => {
                                                cw.canvas.pos.x += dx * KBD_MOVE_STEP;
                                                cw.canvas.pos.y += dy * KBD_MOVE_STEP;
                                                need_render = true;
                                            }
                                            None => {}
                                        }
                                        server_state.raise(id);
                                        session.overview.select(id);
                                        focused_id = Some(id);
                                        known_ids.insert(id);
                                        log::info!(
                                            "kbd move window {} {} +{:.0}",
                                            id,
                                            name,
                                            KBD_MOVE_STEP
                                        );
                                    }
                                    None => {
                                        log::info!("kbd move no window {}", name);
                                    }
                                }
                                consumed = true;
                            }
                        }
                        if let Some(wi) =
                            WASD_KEYS.iter().position(|c| *c == evdev)
                        {
                            let (dx, dy, name) = WASD_DIRS[wi];
                            if !pressed {
                                wasd_held[wi] = false;
                            } else if !wasd_held[wi] && !shift_down {
                                wasd_held[wi] = true;
                                let centre = crate::canvas::Vec2::new(
                                    view.x / 2.0,
                                    view.y / 2.0,
                                );
                                let from = focused_id
                                    .and_then(|id| {
                                        server_state.windows.get(&id)
                                    })
                                    .map(|c| c.canvas.center())
                                    .unwrap_or_else(|| {
                                        session.to_canvas(centre, view)
                                    });
                                let wins: Vec<crate::canvas::CanvasWindow> =
                                    server_state
                                        .windows
                                        .values()
                                        .map(|c| c.canvas)
                                        .collect();
                                match crate::canvas::focus_in_direction(
                                    from,
                                    crate::canvas::Vec2::new(dx, dy),
                                    &wins,
                                ) {
                                    Some(id) => {
                                        // Focus + raise, like click-focus:
                                        // what you type stays visible. The
                                        // camera flies to the window centre
                                        // (view-centre world = pos - offset,
                                        // so the goal is centre + offset).
                                        if let Some(c) =
                                            server_state.windows.get(&id)
                                        {
                                            server.focus_keyboard(
                                                c.surface.as_ref(),
                                            );
                                            if let (Some(wm), Some(xid)) =
                                                (xwm.as_ref(), c.x11)
                                            {
                                                wm.focus(xid);
                                                server.focus_keyboard(None);
                                            }
                                            let ctr = c.canvas.center();
                                            session.camera.retarget(
                                                crate::canvas::Vec2::new(
                                                    ctr.x + session.canvas.offset.x,
                                                    ctr.y + session.canvas.offset.y,
                                                ),
                                            );
                                        }
                                        server_state.raise(id);
                                        focused_id = Some(id);
                                        known_ids.insert(id);
                                        log::info!(
                                            "wasd focus window {} {}",
                                            id,
                                            name
                                        );
                                        need_render = true;
                                    }
                                    None => {
                                        log::info!(
                                            "wasd no window {} from=({:.0},{:.0}) wins={}",
                                            name,
                                            from.x,
                                            from.y,
                                            wins.len()
                                        )
                                    }
                                }
                            }
                            consumed = true;
                        }
                        if let Some(action) =
                            crate::chrome::hover_shortcut(evdev, !held_mods.is_empty())
                        {
                            let cworld = session.to_canvas(pointer, view);
                            let hover = server_state.topmost_at(cworld);
                            match action {
                                crate::chrome::HoverKey::Fullscreen => {
                                    if pressed && !f_held {
                                        if let Some(id) = hover {
                                            let rect = crate::chrome::fullscreen_rect_for(
                                                view,
                                                &|s| session.to_canvas(s, view),
                                            );
                                            let on =
                                                server_state.toggle_fullscreen(id, &rect);
                                            // True fullscreen: tell the client its
                                            // new size so the buffer follows (real
                                            // resize, never a stretch-zoom). The
                                            // client owns exactly the view in
                                            // screen px — independent of zoom.
                                            push_client_size(
                                                &server,
                                                xwm.as_ref(),
                                                id,
                                                &server_state.windows[&id],
                                                on,
                                                (view.x as u32, view.y as u32),
                                            );
                                            log::info!(
                                                "hover key fullscreen window {} on={}",
                                                id,
                                                on
                                            );
                                            need_render = true;
                                        }
                                    }
                                    f_held = pressed;
                                    consumed = true;
                                }
                                crate::chrome::HoverKey::Close => {
                                    if pressed && !q_held {
                                        if let Some(id) = hover {
                                            log::info!("hover key close window {}", id);
                                            if let Some(xid) = server_state.close_window(id) {
                                                if let Some(wm) = xwm.as_ref() {
                                                    wm.kill(xid);
                                                }
                                            }
                                            if focused_id == Some(id) {
                                                focused_id = None;
                                            }
                                            need_render = true;
                                        }
                                    }
                                    q_held = pressed;
                                    consumed = true;
                                }
                                crate::chrome::HoverKey::Grab => {
                                    if pressed {
                                        e_grab = hover.map(|id| (id, cworld));
                                        if let Some(id) = hover {
                                            // Moving grabs the window: raise
                                            // it so it never slides behind
                                            // others mid-drag, and select it.
                                            server_state.raise(id);
                                            session.overview.select(id);
                                            let z = server_state
                                                .windows
                                                .get(&id)
                                                .map(|c| c.z)
                                                .unwrap_or(0);
                                            log::info!(
                                                "hover key grab window {} raised z={} selected={}",
                                                id,
                                                z,
                                                session.overview.selected.len()
                                            );
                                        }
                                    } else {
                                        e_grab = None;
                                    }
                                    consumed = true;
                                }
                                crate::chrome::HoverKey::Scale => {
                                    if pressed && !r_held {
                                        // Arm the radial scale ONCE on the
                                        // hovered window (NOT on auto-repeat:
                                        // re-arming would drag the grab point
                                        // along with the mouse and collapse
                                        // the factor every repeat — the scale
                                        // would stutter instead of tracking).
                                        // Raise + select like the E-grab, pin
                                        // the centre now.
                                        if let Some(id) = hover {
                                            server_state.raise(id);
                                            session.overview.select(id);
                                            focused_id = Some(id);
                                            known_ids.insert(id);
                                            if let Some(c) = server_state
                                                .windows
                                                .get(&id)
                                            {
                                                let center = c.canvas.center();
                                                let size0 = c.canvas.size;
                                                let pushed = (
                                                    size0.x as u32,
                                                    size0.y as u32,
                                                );
                                                r_scale = Some((
                                                    id, center, size0, cworld, pushed,
                                                ));
                                                log::info!(
                                                    "scale arm window {} center=({:.0},{:.0})",
                                                    id, center.x, center.y
                                                );
                                            }
                                        } else {
                                            log::info!("scale no window");
                                        }
                                    } else if !pressed {
                                        // Release drops the scale. (A repeat —
                                        // pressed with r_held — must NOT land
                                        // here: clearing on repeat disarms
                                        // mid-gesture and the window stops
                                        // tracking the mouse entirely.)
                                        r_scale = None;
                                    }
                                    r_held = pressed;
                                    consumed = true;
                                }
                                crate::chrome::HoverKey::Marble => {
                                    // Marble drop: fresh K press (no
                                    // auto-repeat spam) plops one glass
                                    // marble right at the cursor — low and
                                    // still, so perspective never offsets
                                    // it from the pointer (worst near the
                                    // screen edges, where height shifts the
                                    // projected spot most). It rolls with
                                    // slides, and rides pans.
                                    // Canvas-modal like F/Q/E/R — Idle
                                    // typing never spawns.
                                    if pressed && !k_held {
                                        marble_seed = marble_seed
                                            .wrapping_mul(0x9E3779B97F4A7C15)
                                            .wrapping_add(0xC0BE);
                                        let at = session.to_canvas(pointer, view);
                                        let mut m = crate::marbles::spawn(
                                            at.x, at.y, marble_seed,
                                        );
                                        // Plop, not sky-drop: at the cursor.
                                        m.z = 12.0;
                                        m.vz = 0.0;
                                        m.vel =
                                            crate::canvas::Vec2::new(0.0, 0.0);
                                        crate::marbles::add_marble(
                                            &mut marbles, m,
                                        );
                                        log::info!(
                                            "marble drop at ({:.0},{:.0}) n={}",
                                            at.x,
                                            at.y,
                                            marbles.len()
                                        );
                                        need_render = true;
                                    }
                                    k_held = pressed;
                                    consumed = true;
                                }
                            }
                        }
                    }
                    if consumed {
                        return;
                    }
                    // Canvas mode is command mode: presses never reach the
                    // apps (Super/Esc/hover-shortcuts act there instead).
                    // Releases still go through so a key held across the
                    // toggle never sticks down in the app.
                    if session.state == crate::session::SuperState::Canvas {
                        if !pressed {
                            let focus_xid = focused_id
                                .and_then(|id| server_state.windows.get(&id))
                                .and_then(|c| c.x11);
                            let xid =
                                resolve_release(&key_routes, evdev, focus_xid);
                            key_routes.remove(&evdev);
                            match (xwm.as_ref(), xid) {
                                (Some(wm), Some(xid)) => {
                                    wm.xtest_key(xid, evdev, false)
                                }
                                _ => server.input_key(evdev, false),
                            }
                        }
                        return;
                    }
                    // X-backed focus eats keys via XTEST, not the wl seat.
                    // Presses memorise their destination; releases follow
                    // it (anti-stick across focus changes).
                    let focus_xid = focused_id
                        .and_then(|id| server_state.windows.get(&id))
                        .and_then(|c| c.x11);
                    let xid = if pressed {
                        // First press wins: auto-repeats re-fire pressed
                        // while held, and must NOT re-route after a focus
                        // change, or the release follows the repeat, not
                        // the press, and the first window sticks.
                        let dest = note_press(&mut key_routes, evdev, focus_xid);
                        log::debug!(
                            "key press evdev={} dest {:?} routes={}",
                            evdev,
                            dest,
                            key_routes.len()
                        );
                        dest
                    } else {
                        let found = key_routes.get(&evdev).copied();
                        let dest = resolve_release(&key_routes, evdev, focus_xid);
                        key_routes.remove(&evdev);
                        log::debug!(
                            "key release evdev={} dest {:?} (focus now {:?}) found={:?}",
                            evdev, dest, focus_xid, found
                        );
                        dest
                    };
                    match (xwm.as_ref(), xid) {
                        (Some(wm), Some(xid)) => wm.xtest_key(xid, evdev, pressed),
                        _ => server.input_key(evdev, pressed),
                    }
                }
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let scale = graphics.window_size().scale_factor;
                let logical = crate::canvas::Vec2::new(event.x() / scale, event.y() / scale);
                pointer = logical;
                // Live gestures before picking, in canvas-plane coordinates.
                // Canvas pan: content follows the cursor, velocity-ready.
                if let Some(last) = panning {
                    let dx = pointer.x - last.x;
                    let dy = pointer.y - last.y;
                    session.pan_canvas(crate::canvas::Vec2::new(dx, dy));
                    panning = Some(pointer);
                    need_render = true;
                }
                if let Some((id, press_world, last_world, moved)) = grab {
                    let world_now = session.to_canvas(pointer, view);
                    // Past the threshold, every motion moves the window by its
                    // delta — including the crossing event itself, so even a
                    // single-jump drag (press, one motion, release) moves.
                    if moved || drag_offset(press_world, world_now).is_some() {
                        let dx = world_now.x - last_world.x;
                        let dy = world_now.y - last_world.y;
                        match server_state.windows.get_mut(&id) {
                            Some(cw) => {
                                cw.canvas.pos.x += dx;
                                cw.canvas.pos.y += dy;
                                need_render = true;
                            }
                            None => grab = None,
                        }
                        if grab.is_some() {
                            grab = Some((id, press_world, world_now, true));
                        }
                    }
                }
                // E-grab: while E is held, the window follows the pointer —
                // no button needed, release drops it.
                if let Some((id, last)) = e_grab {
                    let now = session.to_canvas(pointer, view);
                    match server_state.windows.get_mut(&id) {
                        Some(cw) => {
                            cw.canvas.pos.x += now.x - last.x;
                            cw.canvas.pos.y += now.y - last.y;
                            need_render = true;
                            e_grab = Some((id, now));
                        }
                        None => e_grab = None,
                    }
                }
                // R-scale is applied once per FRAME (see the per-frame
                // block below), never per motion event: a fast mouse
                // delivers many motions per frame, and reconfiguring the
                // client + rewriting the rect each time tears (artefacts).
                // Capped at the screen FPS by construction.
                let world = session.to_world(logical, view);
                // Hover chrome reveals on proximity: repaint on motion so
                // buttons appear/disappear with the pointer (Idle only;
                // Canvas mode freezes the apps and their chrome).
                if session.state == crate::session::SuperState::Idle {
                    need_render = true;
                }
                // Hit-test in canvas-plane coordinates (windows live there).
                let cworld = session.to_canvas(logical, view);
                log::debug!(
                    "motion physical=({},{}) scale={} logical=({},{}) world=({},{})",
                    event.x(),
                    event.y(),
                    scale,
                    logical.x,
                    logical.y,
                    world.x,
                    world.y
                );
                let hit_id = server_state.topmost_at(cworld);
                let hit = hit_id.and_then(|id| {
                    server_state.windows.get(&id).map(|c| (id, c))
                });
                // Hyprland-style focus-follows-mouse: the window under the
                // cursor takes keyboard focus (both modes, on change only,
                // never mid-drag). Clicking still raises it to the front.
                // Seat offset = surface origin in logical space; Smithay
                // reports enter/motion coords as pointer minus this origin.
                // X-backed windows additionally get XTEST motion in local px.
                let mut x_motion: Option<(u32, i32, i32)> = None;
                let focus = hit.and_then(|(_, c)| {
                    c.surface.clone().map(|s| {
                        let local = cworld - c.canvas.pos;
                        if let Some(xid) = c.x11 {
                            x_motion = Some((xid, local.x as i32, local.y as i32));
                        }
                        let (ox, oy, _, _) =
                            window_screen_rect(&session.view_camera(), &c.canvas, view, scale);
                        (s, ((ox / scale) as i32, (oy / scale) as i32))
                    })
                });
                // xdg popup overlays ride ABOVE windows for the seat:
                // menus and tooltips take enter/motion first. Canvas
                // picking (grab, focus, chrome) never sees them.
                let focus = server_state
                    .popup_at(cworld)
                    .and_then(|pid| {
                        let pop = server_state.popups.get(&pid)?;
                        let parent_pos =
                            server_state.windows.get(&pop.parent)?.canvas.pos;
                        let (org, _) = pop.rect(parent_pos)?;
                        pop.surface.clone().map(|s| {
                            let scr = session.to_screen(org, view);
                            (s, (scr.x as i32, scr.y as i32))
                        })
                    })
                    .or(focus);
                server.input_motion(
                    (logical.x, logical.y),
                    // Canvas mode freezes the apps: pointer motion never
                    // reaches clients there (no hover-highlight, no menu
                    // arming under a gesture like R-scale — the window you
                    // fly over must not react). Idle keeps focus routing
                    // for focus-follows-mouse.
                    if session.state == crate::session::SuperState::Canvas {
                        None
                    } else {
                        focus
                    },
                );
                if session.state != crate::session::SuperState::Canvas {
                    if let (Some(wm), Some((xid, lx, ly))) = (xwm.as_ref(), x_motion) {
                        wm.xtest_motion(xid, lx, ly);
                    }
                }
                // Focus follows the pointer (keeps last focus over empty
                // canvas; never steals mid-drag).
                    if grab.is_none()
                        && panning.is_none()
                        && e_grab.is_none()
                        && r_scale.is_none()
                {
                    if hit_id != focused_id {
                        if let Some(id) = hit_id {
                            if let Some(c) = server_state.windows.get(&id) {
                                server.focus_keyboard(c.surface.as_ref());
                                if let (Some(wm), Some(xid)) =
                                    (xwm.as_ref(), c.x11)
                                {
                                    wm.focus(xid);
                                    server.focus_keyboard(None);
                                }
                            }
                            focused_id = Some(id);
                            known_ids.insert(id);
                        }
                    }
                }
            }
            InputEvent::PointerButton { event } => {
                let pressed = event.state() == ButtonState::Pressed;
                let left = event.button() == MouseButton::Left;
                // Options menu open: left press on an item activates it,
                // left press outside closes; nothing reaches the canvas.
                if menu.open {
                    if pressed && left {
                        // Fixed screen-space popup: layout around the view
                        // centre in SCREEN px (never canvas), hit-tested
                        // with the raw pointer — pan/zoom proof.
                        let lay = crate::menu::layout(
                            menu.tab,
                            view.x / 2.0,
                            view.y / 2.0,
                        );
                        // The tab bar switches tabs; items activate.
                        if let Some(t) = crate::menu::tab_at(&lay, pointer.x, pointer.y) {
                            menu.set_tab(t);
                            log::info!("menu tab {}", crate::menu::TABS[menu.tab]);
                            need_render = true;
                            return;
                        }
                        match crate::menu::item_at(&lay, pointer.x, pointer.y) {
                            Some(i) => {
                                menu.selected = i;
                                if let Some(id) = crate::menu::MenuState::activate_at(
                                    menu.tab,
                                    menu.selected,
                                    &mut settings,
                                ) {
                                    log::info!("menu option {} changed", id);
                                }
                                session.super_dolly = settings.dolly_factor();
                            }
                            None => {
                                menu.open = false;
                                log::info!("menu closed");
                            }
                        }
                        need_render = true;
                    }
                    return;
                }
                // Canvas-mode press starts here; Idle falls through to focus.
                if session.state == crate::session::SuperState::Canvas {
                    // Canvas mode: apps are frozen, nothing reaches clients.
                    // Armed window + press = move it (Hyprland-style, dwell
                    // replaces the held key); empty press starts a canvas pan.
                    if pressed && left {
                        let cw = session.to_canvas(pointer, view);
                        let hit = server_state.topmost_at(cw);
                        match hit {
                            Some(id) if session.dwell.armed == Some(id) => {
                                grab = Some((id, cw, cw, true));
                                server_state.raise(id);
                                session.overview.select(id);
                                log::info!("canvas armed move window {}", id);
                            }
                            Some(id) => {
                                let action = session.on_event(
                                    SuperEvent::WindowClick(id),
                                    &|wid| server_state.windows.get(&wid).map(|c| c.canvas),
                                    view,
                                );
                                log::info!(
                                    "super {:?} click window {} state={:?} zoom={:.2} selected={}",
                                    action,
                                    id,
                                    session.state,
                                    session.camera.zoom,
                                    session.overview.selected.len()
                                );
                            }
                            None => {
                                pan_start = Some(pointer);
                                panning = Some(pointer);
                            }
                        }
                    } else if !pressed && left {
                        grab = None;
                        panning = None;
                        // Press + release without moving = plain canvas click.
                        if let Some(start) = pan_start.take() {
                            if drag_offset(start, pointer).is_none() {
                                let action = session.on_event(
                                    SuperEvent::CanvasClick,
                                    &|wid| server_state.windows.get(&wid).map(|c| c.canvas),
                                    view,
                                );
                                log::info!(
                                    "super {:?} click canvas state={:?} zoom={:.2} selected={}",
                                    action,
                                    session.state,
                                    session.camera.zoom,
                                    session.overview.selected.len()
                                );
                            }
                        }
                    }
                    need_render = true;
                } else {
                    // Idle: click focuses the window, then the client
                    // receives the button. Plain left NEVER moves windows.
                    if pressed {
                        // Window chrome first: a click on a revealed button
                        // acts and never reaches the app.
                        if let Some((chrome_id, layout)) = last_chrome {
                            if let Some(btn) = layout.hit(pointer) {
                                match btn {
                                    crate::chrome::ChromeButton::Close => {
                                        log::info!("chrome close window {}", chrome_id);
                                        if let Some(xid) = server_state.close_window(chrome_id) {
                                            if let Some(wm) = xwm.as_ref() {
                                                wm.kill(xid);
                                            }
                                        }
                                        if focused_id == Some(chrome_id) {
                                            focused_id = None;
                                        }
                                    }
                                    crate::chrome::ChromeButton::Fullscreen => {
                                        let rect = crate::chrome::fullscreen_rect_for(view, &|s| {
                                            session.to_canvas(s, view)
                                        });
                                        let on =
                                            server_state.toggle_fullscreen(chrome_id, &rect);
                                        // True fullscreen: tell the client its
                                        // new size so the buffer follows (real
                                        // resize, never a stretch-zoom). The
                                        // client owns exactly the view in
                                        // screen px — independent of zoom.
                                        push_client_size(
                                            &server,
                                            xwm.as_ref(),
                                            chrome_id,
                                            &server_state.windows[&chrome_id],
                                            on,
                                            (view.x as u32, view.y as u32),
                                        );
                                        log::info!(
                                            "chrome fullscreen window {} on={}",
                                            chrome_id,
                                            on
                                        );
                                    }
                                }
                                need_render = true;
                                last_chrome = None;
                                return;
                            }
                        }
                        let cworld = session.to_canvas(pointer, view);
                        // Plain windows pick here (R-scale needs no button:
                        // it arms straight off the R key).
                        // Seat-first popup: a press inside a menu/tooltip goes
                        // to the popup surface (keyboard too, so menus take
                        // arrows); the PARENT raises and takes canvas focus.
                        if let Some(pid) = server_state.popup_at(cworld) {
                            if let Some(pop) = server_state.popups.get(&pid) {
                                let parent = pop.parent;
                                if let Some(ps) = pop.surface.clone() {
                                    server.focus_keyboard(Some(&ps));
                                }
                                popup_focus = Some((pid, parent));
                                server_state.raise(parent);
                                focused_id = Some(parent);
                                known_ids.insert(parent);
                                log::info!("click popup {} parent={}", pid, parent);
                            }
                        } else if let Some(id) = server_state.topmost_at(cworld) {
                            if let Some(c) = server_state.windows.get(&id) {
                                server.focus_keyboard(c.surface.as_ref());
                            }
                            popup_focus = None;
                            server_state.raise(id);
                            focused_id = Some(id);
                            known_ids.insert(id);
                            // Click diagnostics: which window the pick resolved
                            // to, in canvas-local px and its X id (if any).
                            if let Some(c) = server_state.windows.get(&id) {
                                let local = cworld - c.canvas.pos;
                                log::info!(
                                    "click window {} local=({},{}) x11={:?}",
                                    id,
                                    local.x as i32,
                                    local.y as i32,
                                    c.x11
                                );
                            }
                        }
                    }
                    let code = button_code(event.button());
                    server.input_button(code, pressed);
                    // X-backed focus: mirror press/release via XTEST.
                    let xid = focused_id
                        .and_then(|id| server_state.windows.get(&id))
                        .and_then(|c| c.x11);
                    if let (Some(wm), Some(xid)) = (xwm.as_ref(), xid) {
                        if pressed {
                            // Restack BEFORE delivery: X button routing
                            // targets the X-topmost window, so the focused
                            // window must already be top when the press
                            // lands (the per-frame drain only covers
                            // raises from other paths).
                            wm.restack_above(xid);
                            wm.focus(xid);
                            server.focus_keyboard(None);
                        }
                        wm.xtest_button(xid, code, pressed);
                    }
                }
            }
            InputEvent::PointerAxis { event } => {
                // Wheel: in Canvas mode it ALWAYS zooms at the pointer —
                // apps are frozen, so scrolling a client would go nowhere
                // (that dead wheel over windows was the bug). Anywhere
                // else (Idle) the wheel NEVER zooms: it always goes to
                // the client as scroll, even over empty canvas. Zoom
                // glides (smooth fade, momentum kept), never snaps by
                // notches.
                let vertical = event
                    .amount(Axis::Vertical)
                    .or_else(|| event.amount_discrete(Axis::Vertical).map(|s| s as f64))
                    .unwrap_or(0.0);
                if vertical != 0.0 {
                    let canvas_mode =
                        session.state == crate::session::SuperState::Canvas;
                    if canvas_mode {
                        let factor = if vertical > 0.0 { 1.15 } else { 1.0 / 1.15 };
                        session.zoom_glide_at(pointer, view, factor);
                        log::info!("wheel zoom->{:.2}", session.camera.zoom_target);
                        need_render = true;
                        return;
                    }
                }
                let mut frame =
                    AxisFrame::new(event.time()).source(match event.source() {
                        InputAxisSource::Wheel => AxisSource::Wheel,
                        InputAxisSource::WheelTilt => AxisSource::WheelTilt,
                        InputAxisSource::Finger => AxisSource::Finger,
                        InputAxisSource::Continuous => AxisSource::Continuous,
                    });
                if let Some(value) = event.amount(Axis::Horizontal) {
                    frame = frame.value(WlAxis::HorizontalScroll, value);
                }
                if let Some(value) = event.amount(Axis::Vertical) {
                    frame = frame.value(WlAxis::VerticalScroll, value);
                }
                if let Some(steps) = event.amount_discrete(Axis::Horizontal) {
                    frame = frame.discrete(WlAxis::HorizontalScroll, steps as i32);
                }
                if let Some(steps) = event.amount_discrete(Axis::Vertical) {
                    frame = frame.discrete(WlAxis::VerticalScroll, steps as i32);
                }
                server.input_axis(frame);
            }
            InputEvent::Special(evt) => match evt {
                WinitEvent::Refresh => need_render = true,
                WinitEvent::Resized { .. } => need_render = true,
                WinitEvent::Focus(focused) => {
                    log::info!("focus {}", focused);
                    if !focused {
                        // Keys released outside never arrive: drop every
                        // held state or it wedges (E-grab stuck, Super
                        // phantom-held, focus guard blind) until restart.
                        e_grab = None;
                        grab = None;
                        panning = None;
                        pan_start = None;
                        r_scale = None;
                        held_mods.clear();
                        super_held = false;
                        f_held = false;
                        q_held = false;
                        r_held = false;
                        for h in wasd_held.iter_mut() {
                            *h = false;
                        }
                        session.dwell.disarm();
                        need_render = true;
                    }
                }
            },
            _ => {}
        }) {
            Ok(()) => {}
            Err(WinitInputError::WindowClosed) => {
                log::info!("window closed after {} frames", frames);
                break;
            }
        }

        // Dwell-to-arm runs every frame (a still pointer sends no motion
        // events): 1s inside one window arms it for moving, leaving resets.
        // Popup focus fallback: the keyboard went to a popup surface on
        // press — if that popup has since died, fold focus back to the
        // parent window NOW (not at the next click), or the app reads as
        // frozen. Runs every frame because dismiss arrives via dispatch.
        if let Some((pid, parent)) = popup_focus {
            if !server_state.popups.contains_key(&pid) {
                if let Some(c) = server_state.windows.get(&parent) {
                    server.focus_keyboard(c.surface.as_ref());
                    log::info!("popup {} gone: focus back to window {}", pid, parent);
                }
                popup_focus = None;
            }
        }
        // R-scale, once per FRAME (never per motion): while R is held the
        // window grows/shrinks with the mouse DISTANCE from the pinned
        // centre (radial — cursor at centre = tiny, further = bigger in
        // every direction), no button needed, release drops it. Motions
        // can outnumber frames many-to-one; applying per frame caps the
        // client reconfigures at the screen FPS so nothing tears. The
        // client is reconfigured live (integer changes only) so the app
        // reflows instead of stretching.
        if let Some((id, center, size0, mouse0, pushed)) = r_scale {
            let now = session.to_canvas(pointer, view);
            let (pos, size) =
                crate::chrome::apply_radial_scale(center, size0, mouse0, now);
            let mut gone = false;
            match server_state.windows.get_mut(&id) {
                Some(cw) => {
                    cw.canvas.pos = pos;
                    cw.canvas.size = size;
                    need_render = true;
                }
                None => gone = true,
            }
            if gone {
                r_scale = None;
            } else {
                let want = (size.x.max(0.0) as u32, size.y.max(0.0) as u32);
                if want != pushed {
                    if let Some(win) = server_state.windows.get(&id) {
                        push_client_size(&server, xwm.as_ref(), id, win, false, want);
                        log::debug!("scale window {} now {}x{}", id, want.0, want.1);
                    }
                    r_scale = Some((id, center, size0, mouse0, want));
                } else {
                    r_scale = Some((id, center, size0, mouse0, pushed));
                }
            }
        }
        if session.state == crate::session::SuperState::Canvas {
            let now_ms = frame_clock.elapsed().as_millis() as u64;
            let cw = session.to_canvas(pointer, view);
            let inside = server_state.topmost_at(cw);
            if session.dwell.update(inside, now_ms).is_some() {
                log::debug!("dwell armed {:?}", session.dwell.armed);
            }
        }

        // Camera flight: spring toward target every frame; while flying,
        // force frames so the motion renders. The spring settles to exact
        // rest, so the repaint budget ends with the motion — physics is a
        // few floats, repaints only last as long as the flight (~0.5s).
        // Props chase the same clock against the view camera (offset
        // included, so trail and projection never disagree); any moving
        // prop also holds frames until it sleeps.
        {
            let now = std::time::Instant::now();
            let dt = now.duration_since(last_cam_step).as_secs_f64();
            last_cam_step = now;
            if session.camera.step(dt) {
                need_render = true;
            }
            // Wheel-zoom tracking: keep the pinned world under its
            // screen point through the fade (drops itself on landing).
            session.track_zoom_anchor(view);
            if session.zoom_anchor.is_some() {
                need_render = true;
            }
            let vcam = session.view_camera().pos;
            for p in props.iter_mut() {
                if p.step(dt, vcam) {
                    need_render = true;
                }
            }
            // Marbles: same clock; windows are static hitboxes (they
            // never move for marbles — a grabbed window shoves through,
            // the push-out ejects the marble). The VIEW slides under the
            // marbles (camera minus canvas offset): WASD flights and
            // left-drag pans both grip resting marbles, hard stops fling
            // them further like beads on a yanked tablecloth. Zoom never
            // moves them: anchor tracking banks its offset as zoom slip,
            // drained here before the slide is measured.
            {
                let walls: Vec<(f64, f64, f64, f64)> = server_state
                    .windows
                    .values()
                    .map(|c| {
                        (c.canvas.pos.x, c.canvas.pos.y, c.canvas.size.x, c.canvas.size.y)
                    })
                    .collect();
                let dt_c = dt.max(1e-4);
                let slip = session.drain_zoom_slip();
                let canvas_vel = crate::canvas::Vec2::new(
                    (session.canvas.offset.x - prev_canvas_offset.x - slip.x) / dt_c,
                    (session.canvas.offset.y - prev_canvas_offset.y - slip.y) / dt_c,
                );
                prev_canvas_offset = session.canvas.offset;
                // LOD prologue: the viewport in canvas coords. Marbles
                // inside 2x the screen span step every frame; out to 10x
                // they step every 4th frame (4x dt, same motion, quarter
                // the cost); past 10x they are dropped — rolled too far
                // to matter, but never popped while visible.
                frame_no = frame_no.wrapping_add(1);
                let ctl = session.to_canvas(
                    crate::canvas::Vec2::new(0.0, 0.0),
                    view,
                );
                let cbr = session.to_canvas(view, view);
                let (rx0, rx1) = if ctl.x < cbr.x {
                    (ctl.x, cbr.x)
                } else {
                    (cbr.x, ctl.x)
                };
                let (ry0, ry1) = if ctl.y < cbr.y {
                    (ctl.y, cbr.y)
                } else {
                    (cbr.y, ctl.y)
                };
                let span = ((rx1 - rx0).max(ry1 - ry0)).max(1.0);
                marbles.retain(|m| {
                    let dx = if m.pos.x < rx0 {
                        rx0 - m.pos.x
                    } else if m.pos.x > rx1 {
                        m.pos.x - rx1
                    } else {
                        0.0
                    };
                    let dy = if m.pos.y < ry0 {
                        ry0 - m.pos.y
                    } else if m.pos.y > ry1 {
                        m.pos.y - ry1
                    } else {
                        0.0
                    };
                    crate::marbles::lod_for((dx * dx + dy * dy).sqrt(), span)
                        != crate::marbles::Lod::Cull
                });
                let slide = crate::canvas::Vec2::new(
                    session.camera.vel.x - canvas_vel.x,
                    session.camera.vel.y - canvas_vel.y,
                );
                for c in marbles.iter_mut() {
                    let dx = if c.pos.x < rx0 {
                        rx0 - c.pos.x
                    } else if c.pos.x > rx1 {
                        c.pos.x - rx1
                    } else {
                        0.0
                    };
                    let dy = if c.pos.y < ry0 {
                        ry0 - c.pos.y
                    } else if c.pos.y > ry1 {
                        c.pos.y - ry1
                    } else {
                        0.0
                    };
                    let dist = (dx * dx + dy * dy).sqrt();
                    let stepped = match crate::marbles::lod_for(dist, span) {
                        crate::marbles::Lod::Full => c.step(dt, slide, &walls),
                        crate::marbles::Lod::Reduced
                            if (frame_no + c.seed as u64) % 4 == 0 =>
                        {
                            c.step(dt * 4.0, slide, &walls)
                        }
                        _ => false,
                    };
                    if stepped {
                        need_render = true;
                    }
                }
            }
        }

        // (Bar removed: no status strip; window chrome lives on windows.)

        // Newly committed client buffers (native or XWayland) need a
        // frame even with no input: otherwise live apps (xterm text,
        // clocks) stay stuck on their first imported buffer. Damage-only
        // commits (no new attach) count too: X clients repaint into the
        // same shm buffer and only send damage.
        for cw in server_state.windows.values() {
            if let Some(surface) = cw.surface.as_ref() {
                let pending = with_states(surface, |states| {
                    let current = states.cached_state.current::<SurfaceAttributes>();
                    current.buffer.is_some() || !current.damage.is_empty()
                });
                if matches!(pending, Ok(true)) {
                    need_render = true;
                    break;
                }
            }
        }

        // FX clocks: wallpaper drift and glow flow advance by real
        // elapsed time through their own speeds (eco freezes both);
        // re-anchored when frozen so unpausing never jumps. The glow
        // fade glides toward Canvas mode (eco never blocks the
        // transition, only the flow) and holds frames while moving.
        let fx_now = std::time::Instant::now();
        let fx_dt = fx_now.duration_since(last_bg_paint).as_secs_f32();
        if settings.bg_speed() > 0.0 {
            bg_time += fx_dt * settings.bg_speed();
        }
        if settings.glow_flow() > 0.0 {
            glow_time += fx_dt * settings.glow_flow();
        }
        if settings.wave_speed() > 0.0 {
            wave_time += fx_dt * settings.wave_speed();
        }
        last_bg_paint = fx_now;
        // Menu slide-in holds a few frames while dropping in.
        if menu.open {
            if let Some(t) = menu_opened_at {
                if t.elapsed().as_secs_f32() < 0.25 {
                    need_render = true;
                }
            }
        }
        let glow_target =
            if session.state == crate::session::SuperState::Canvas
                && settings.glow_intensity() > 0.0
            {
                1.0
            } else {
                0.0
            };
        let glow_step = 2.5 * fx_dt;
        glow_f += (glow_target - glow_f).clamp(-glow_step, glow_step);
        if (glow_f - glow_target).abs() < 0.002 {
            glow_f = glow_target;
        }
        if glow_f != glow_target {
            need_render = true;
        }
        let glow_animating = glow_f > 0.0 && settings.glow_flow() > 0.0;

        // Ambient background drift: a slow repaint cadence while anything
        // moves (wallpaper or Super glow); eco or all-off = frozen, zero
        // extra frames.
        if settings.bg_frame_interval(glow_animating).is_some()
            && last_bg_paint.elapsed() >= settings.bg_frame_interval(glow_animating).unwrap()
        {
            need_render = true;
        }
        // Paced clients (Kate, Alacritty, Qt/GTK) repaint only after a
        // frame callback: with FX frozen (eco/all-off) nothing renders,
        // no callback ever goes out, and the client stalls forever after
        // its first hover-driven repaint (opens, reacts once, then
        // freezes; resizes stretch the stale buffer). Pending callbacks
        // force a frame, restoring the request/render/done loop. Idle
        // with no pending callback still renders zero frames.
        if !need_render {
            need_render = server_state.windows.values().any(|cw| {
                cw.surface.as_ref().map_or(false, |surface| {
                    with_states(surface, |states| {
                        !states
                            .cached_state
                            .current::<SurfaceAttributes>()
                            .frame_callbacks
                            .is_empty()
                    })
                    .unwrap_or(false)
                })
            });
        }

        if need_render || frames == 0 || max_frames.is_some() {
            let scale = graphics.window_size().scale_factor;
            let color = background_color(&session.camera);
            graphics.render(|renderer, frame| {
                if settings.bg_anim || glow_f > 0.0 {
                    if bg_prog.is_none() && !bg_failed {
                        match renderer.with_context(|_, gl| {
                            crate::render3d::compile_bg(gl)
                        }) {
                            Ok(Ok(p)) => {
                                bg_prog = Some(p);
                                log::info!("background GL program ready");
                            }
                            Ok(Err(e)) => {
                                log::warn!("background GL program: {}", e);
                                bg_failed = true;
                            }
                            Err(e) => {
                                log::warn!("background GL context: {:?}", e);
                                bg_failed = true;
                            }
                        }
                    }
                    if let Some(prog) = bg_prog {
                        let sc = scale as f32;
                        let (gcol_a, gcol_b) = settings.glow_hue();
                        let tint = settings.bg_tint();
                        let wcol = settings.wave_col();
                        // Super mode dims the wallpaper into a backdrop.
                        let dim = settings.bg_dim()
                            * if session.state == crate::session::SuperState::Canvas {
                                settings.super_dim()
                            } else {
                                1.0
                            };
                        let _ = renderer.with_context(|_, gl| unsafe {
                            crate::render3d::draw_bg(
                                gl,
                                prog,
                                view.x as f32 * sc,
                                view.y as f32 * sc,
                                bg_time,
                                session.view_camera().zoom as f32,
                                dim,
                                1.0,
                                settings.depth_on(),
                                if settings.bg_anim { 1.0 } else { 0.0 },
                                settings.bg_pattern() as f32,
                                settings.bg_scale(),
                                settings.bg_grain(),
                                settings.bg_vig(),
                                tint,
                                settings.wave_amp(),
                                settings.wave_shape() as f32,
                                settings.wave_octaves() as f32,
                                settings.wave_freq(),
                                settings.wave_dir(),
                                settings.wave_crisp(),
                                wcol,
                                wave_time,
                                glow_f,
                                gcol_a,
                                gcol_b,
                                settings.glow_intensity(),
                                settings.glow_width(),
                                glow_time,
                                0.0,
                            );
                        });
                    } else if let Err(err) = frame.clear(color) {
                        log::error!("clear failed: {}", err);
                    }
                } else if let Err(err) = frame.clear(color) {
                    log::error!("clear failed: {}", err);
                }
                // Canvas props (decor layer, behind windows): parallax dust
                // and laggy boxes through the solid program. Empty by
                // default — this whole block is skipped when no demo.
                if !props.is_empty() {
                    if solid_prog.is_none() {
                        match renderer.with_context(|_, gl| {
                            crate::render3d::compile_solid(gl)
                        }) {
                            Ok(Ok(p)) => {
                                solid_prog = Some(p);
                                log::info!("chrome GL program ready");
                            }
                            Ok(Err(e)) => log::warn!("chrome GL program: {}", e),
                            Err(e) => log::warn!("chrome GL context: {:?}", e),
                        }
                    }
                    if let Some(prog) = solid_prog {
                        let mvp = crate::render3d::ortho(view.x as f32, view.y as f32);
                        let sc = scale as f32;
                        let cam = session.view_camera();
                        let _ = renderer.with_context(|_, gl| unsafe {
                            for p in &props {
                                let (x, y, w, h) = p.screen_rect(&cam, view);
                                crate::render3d::draw_solid(
                                    gl,
                                    prog,
                                    &mvp,
                                    p.shade,
                                    (
                                        x as f32 * sc,
                                        y as f32 * sc,
                                        w as f32 * sc,
                                        h as f32 * sc,
                                    ),
                                );
                            }
                        });
                    }
                }
                // Painter's algorithm: lowest z first, so the visual top
                // always matches the pick top (topmost_at). HashMap
                // iteration is random; unsorted, an occluded window can
                // paint over the focused one while clicks resolve to the
                // focused one (click lands on an invisible window).
                for id in server_state.paint_order() {
                    let cw = match server_state.windows.get_mut(&id) {
                        Some(cw) => cw,
                        None => continue,
                    };
                    // View-locked layer panels stick to the viewport:
                    // recompute the canvas rect from the anchor every
                    // frame so camera moves never drag panels along.
                    if let Some(lock) = cw.view_lock {
                        let org = session.to_world(
                            crate::canvas::Vec2::new(0.0, 0.0),
                            view,
                        );
                        let (w, h) = (cw.canvas.size.x, cw.canvas.size.y);
                        let x = if lock.anchor & crate::layer::ANCHOR_RIGHT != 0 {
                            org.x + view.x - w - lock.margins[1] as f64
                        } else {
                            org.x + lock.margins[3] as f64
                        };
                        let y = if lock.anchor & crate::layer::ANCHOR_BOTTOM != 0 {
                            org.y + view.y - h - lock.margins[2] as f64
                        } else {
                            org.y + lock.margins[0] as f64
                        };
                        cw.canvas.pos.x = x;
                        cw.canvas.pos.y = y;
                    }
                    // Import freshly committed shm buffers. New attaches
                    // upload the new buffer; damage-only commits repaint
                    // into the kept buffer, so re-upload that.
                    if let Some(surface) = cw.surface.as_ref() {
                        let (assignment, damaged) = with_states(surface, |states| {
                            let mut current =
                                states.cached_state.current::<SurfaceAttributes>();
                            let assignment = current.buffer.take();
                            let damaged = !current.damage.is_empty();
                            current.damage.clear();
                            (assignment, damaged)
                        })
                        .unwrap_or((None, false));
                        let buffer = match assignment {
                            Some(BufferAssignment::NewBuffer { buffer, .. }) => Some(buffer),
                            _ => None,
                        };
                        let buffer = buffer.or_else(|| {
                            if damaged {
                                cw.last_buffer.clone()
                            } else {
                                None
                            }
                        });
                        if let Some(buffer) = buffer.as_ref() {
                            cw.last_buffer = Some(buffer.clone());
                            let dims = client_buffer_size(buffer);
                            if let Some((w, h)) = dims {
                                let damage = Rectangle::<i32, BufferCoord>::from_loc_and_size(
                                    (0, 0),
                                    (w, h),
                                );
                                // GL clients present dmabuf, not shm: import
                                // through the matching path, same texture
                                // slot either way.
                                let imported = match buffer_type(&buffer) {
                                    Some(BufferType::Dma) => {
                                        renderer.import_dma_buffer(&buffer)
                                    }
                                    _ => renderer.import_shm_buffer(&buffer, None, &[damage]),
                                };
                                match imported {
                                    Ok(tex) => {
                                        // Follow the buffer only when the CLIENT
                                        // resizes (first map or configure):
                                        // manual moves/resizes never snap back.
                                        // Fullscreen windows keep the exact view
                                        // rect (true fullscreen); the client
                                        // buffer follows via configure.
                                        let buf = (w as u32, h as u32);
                                        if cw.fullscreen_prev.is_none()
                                            && cw.buf_size != Some(buf)
                                        {
                                            cw.buf_size = Some(buf);
                                            cw.canvas.size.x = w as f64;
                                            cw.canvas.size.y = h as f64;
                                            log::info!(
                                                "window {} sized to {}x{}",
                                                id,
                                                w,
                                                h
                                            );
                                        }
                                        log::info!(
                                            "imported {}x{} shm buffer for window {}",
                                            w,
                                            h,
                                            id
                                        );
                                        cw.texture = Some(tex)
                                    }
                                    Err(err) => log::warn!("buffer import failed: {}", err),
                                }
                            }
                        }
                    }
                    // Windows render as textured perspective planes (3D).
                    // shm buffers go through our own program (zero tilt,
                    // flat 1:1, in every mode); anything else keeps Smithay's
                    // flat path so exotic buffers never break the frame.
                    if let Some(tex) = cw.texture.as_ref() {
                        let is_shm = matches!(
                            cw.last_buffer.as_ref().and_then(buffer_type),
                            Some(BufferType::Shm)
                        );
                        if tex_prog.is_none() {
                            match renderer.with_context(|_, gl| {
                                crate::render3d::compile_textured(gl)
                            }) {
                                Ok(Ok(p)) => {
                                    tex_prog = Some(p);
                                    log::info!("textured GL program ready");
                                }
                                Ok(Err(e)) => log::warn!("textured GL program: {}", e),
                                Err(e) => log::warn!("textured GL context: {:?}", e),
                            }
                        }
                        if is_shm {
                            if let Some(prog) = tex_prog {
                                // Zero tilt, always, in every mode: windows
                                // stay axis-aligned. (The old Canvas-mode
                                // lean snapped on/off instantly while the
                                // zoom glided underneath — that double
                                // transform read as a dezoom-then-zoom
                                // stutter and left everything leaning.)
                                // The perspective MVP at zero tilt maps 1:1.
                                let tilt = (0.0, 0.0);
                                // Canvas-plane quad, y negated for clip space, with
                                // upright UVs (buffer row 0 = top). The MVP
                                // maps canvas -> screen; physical px = logical
                                // times the output scale.
                                let sc = scale as f32;
                                let (qx, qy) = (
                                    cw.canvas.pos.x as f32 * sc,
                                    -(cw.canvas.pos.y as f32) * sc,
                                );
                                let (qw, qh) = (
                                    cw.canvas.size.x as f32 * sc,
                                    -(cw.canvas.size.y as f32) * sc,
                                );
                                let vw = view.x as f32 * sc;
                                let vh = view.y as f32 * sc;
                                let mid = session.to_world(
                                    crate::canvas::Vec2::new(view.x / 2.0, view.y / 2.0),
                                    view,
                                );
                                let mvp = crate::render3d::canvas_mvp(
                                    (vw, vh),
                                    (mid.x as f32 * sc, mid.y as f32 * sc),
                                    session.view_camera().zoom as f32,
                                    tilt,
                                );
                                let verts: [f32; 16] = [
                                    qx, qy, 0.0, 0.0, //
                                    qx + qw, qy, 1.0, 0.0, //
                                    qx, qy + qh, 0.0, 1.0, //
                                    qx + qw, qy + qh, 1.0, 1.0,
                                ];
                                let tid = tex.tex_id();
                                // X-format buffers carry no real alpha (the X
                                // byte is server droppings); without forcing
                                // opaque, the window renders transparent
                                // wherever nothing opaque was drawn.
                                let opaque = cw
                                    .last_buffer
                                    .as_ref()
                                    .map(|buffer| {
                                        with_buffer_contents(buffer, |_, data| {
                                            crate::render3d::shm_format_is_opaque(
                                                data.format,
                                            )
                                        })
                                        .unwrap_or(false)
                                    })
                                    .unwrap_or(false);
                                let _ = renderer.with_context(|_, gl| unsafe {
                                    crate::render3d::draw_textured(
                                        gl, prog, &mvp, tid, &verts, opaque,
                                    );
                                });
                                continue;
                            }
                        }
                        let (x, y, w, h) =
                            window_screen_rect(&session.view_camera(), &cw.canvas, view, scale);
                        let dest = Rectangle::<f64, Physical>::from_loc_and_size((x, y), (w, h));
                        let src = Rectangle::<i32, BufferCoord>::from_loc_and_size(
                            (0, 0),
                            tex.size(),
                        );
                        if let Err(err) = frame.render_texture_from_to(
                            tex,
                            src,
                            dest,
                            Transform::Normal,
                            1.0,
                        ) {
                            log::warn!("draw failed: {}", err);
                        }
                    }
                }
                // xdg popup overlays (tooltips, menus): imported and drawn
                // above ALL windows at parent.pos + offset. The offset is
                // fixed from the cursor at first map (popups open at the
                // pointer); the size follows the first committed buffer.
                // Never pickable, never focusable as windows.
                let mut pids: Vec<u64> = server_state.popups.keys().cloned().collect();
                pids.sort_unstable();
                for pid in pids {
                    let (parent_pos, cursor_home) = match server_state
                        .popups
                        .get(&pid)
                        .and_then(|pop| {
                            server_state
                                .windows
                                .get(&pop.parent)
                                .map(|w| (w.canvas.pos, pop.offset.is_none()))
                        }) {
                        Some(v) => v,
                        None => continue,
                    };
                    if cursor_home {
                        let home = session.to_canvas(pointer, view);
                        if let Some(pop) = server_state.popups.get_mut(&pid) {
                            pop.offset = Some(crate::canvas::Vec2::new(
                                home.x - parent_pos.x,
                                home.y - parent_pos.y,
                            ));
                            log::info!(
                                "popup {} placed parent=({:.0},{:.0})",
                                pid,
                                parent_pos.x,
                                parent_pos.y
                            );
                        }
                    }
                    if let Some(surface) = server_state
                        .popups
                        .get(&pid)
                        .and_then(|pop| pop.surface.clone())
                    {
                        let (assignment, damaged) = with_states(&surface, |states| {
                            let mut current =
                                states.cached_state.current::<SurfaceAttributes>();
                            let assignment = current.buffer.take();
                            let damaged = !current.damage.is_empty();
                            current.damage.clear();
                            (assignment, damaged)
                        })
                        .unwrap_or((None, false));
                        let buffer = match assignment {
                            Some(BufferAssignment::NewBuffer { buffer, .. }) => Some(buffer),
                            _ => None,
                        };
                        let pop_last = server_state
                            .popups
                            .get(&pid)
                            .and_then(|pop| pop.last_buffer.clone());
                        let buffer = buffer.or_else(|| {
                            if damaged {
                                pop_last
                            } else {
                                None
                            }
                        });
                        if let Some(buffer) = buffer.as_ref() {
                            let dims = client_buffer_size(buffer);
                            if let Some((w, h)) = dims {
                                let damage = Rectangle::<i32, BufferCoord>::from_loc_and_size(
                                    (0, 0),
                                    (w, h),
                                );
                                // GL clients present dmabuf, not shm: import
                                // through the matching path, same texture
                                // slot either way.
                                let imported = match buffer_type(&buffer) {
                                    Some(BufferType::Dma) => {
                                        renderer.import_dma_buffer(&buffer)
                                    }
                                    _ => renderer.import_shm_buffer(&buffer, None, &[damage]),
                                };
                                match imported {
                                    Ok(tex) => {
                                        if let Some(pop) =
                                            server_state.popups.get_mut(&pid)
                                        {
                                            pop.last_buffer = Some(buffer.clone());
                                            pop.size = crate::canvas::Vec2::new(
                                                w as f64,
                                                h as f64,
                                            );
                                            pop.texture = Some(tex);
                                        }
                                    }
                                    Err(err) => {
                                        log::warn!("popup shm import failed: {}", err)
                                    }
                                }
                            }
                        }
                    }
                    let quad = server_state.popups.get(&pid).and_then(|pop| {
                        server_state
                            .windows
                            .get(&pop.parent)
                            .and_then(|w| pop.rect(w.canvas.pos))
                    });
                    let tex_clone = server_state
                        .popups
                        .get(&pid)
                        .and_then(|pop| pop.texture.as_ref().map(|t| t.tex_id()));
                    if let (Some((org, size)), Some(tid)) = (quad, tex_clone) {
                        if tex_prog.is_none() {
                            match renderer.with_context(|_, gl| {
                                crate::render3d::compile_textured(gl)
                            }) {
                                Ok(Ok(p)) => {
                                    tex_prog = Some(p);
                                }
                                Ok(Err(e)) => log::warn!("textured GL program: {}", e),
                                Err(e) => log::warn!("textured GL context: {:?}", e),
                            }
                        }
                        if let Some(prog) = tex_prog {
                            let tilt = (0.0, 0.0);
                            let sc = scale as f32;
                            let (qx, qy) =
                                (org.x as f32 * sc, -(org.y as f32) * sc);
                            let (qw, qh) =
                                (size.x as f32 * sc, -(size.y as f32) * sc);
                            let vw = view.x as f32 * sc;
                            let vh = view.y as f32 * sc;
                            let mid = session.to_world(
                                crate::canvas::Vec2::new(view.x / 2.0, view.y / 2.0),
                                view,
                            );
                            let mvp = crate::render3d::canvas_mvp(
                                (vw, vh),
                                (mid.x as f32 * sc, mid.y as f32 * sc),
                                session.view_camera().zoom as f32,
                                tilt,
                            );
                            let verts: [f32; 16] = [
                                qx, qy, 0.0, 0.0, //
                                qx + qw, qy, 1.0, 0.0, //
                                qx, qy + qh, 0.0, 1.0, //
                                qx + qw, qy + qh, 1.0, 1.0,
                            ];
                            let _ = renderer.with_context(|_, gl| unsafe {
                                crate::render3d::draw_textured(
                                    gl, prog, &mvp, tid, &verts, true,
                                );
                            });
                        }
                    }
                }
                // Window chrome (Idle only): probe the top hit window; when
                // the pointer lingers near its top-right corner, two solid
                // squares draw there — red close, blue fullscreen.
                let mut chrome_now = None;
                if session.state == crate::session::SuperState::Idle {
                    let cworld = session.to_canvas(pointer, view);
                    if let Some(id) = server_state.topmost_at(cworld) {
                        if let Some(cw) = server_state.windows.get(&id) {
                            let origin = session.to_screen(cw.canvas.pos, view);
                            let zoom = session.view_camera().zoom;
                            let size = crate::canvas::Vec2::new(
                                cw.canvas.size.x * zoom,
                                cw.canvas.size.y * zoom,
                            );
                            if let Some(layout) =
                                crate::chrome::layout_for(origin, size, pointer)
                            {
                                chrome_now = Some((id, layout));
                            }
                        }
                    }
                }
                if solid_prog.is_none() {
                    match renderer.with_context(|_, gl| {
                        crate::render3d::compile_solid(gl)
                    }) {
                        Ok(Ok(p)) => {
                            solid_prog = Some(p);
                            log::info!("chrome GL program ready");
                        }
                        Ok(Err(e)) => log::warn!("chrome GL program: {}", e),
                        Err(e) => log::warn!("chrome GL context: {:?}", e),
                    }
                }
                if let Some((_, layout)) = chrome_now {
                    if let Some(prog) = solid_prog {
                        let mvp =
                            crate::render3d::ortho(view.x as f32, view.y as f32);
                        let sc = scale as f32;
                        let b = crate::chrome::CHROME_BUTTON as f32 * sc;
                        let _ = renderer.with_context(|_, gl| unsafe {
                            crate::render3d::draw_solid(
                                gl,
                                prog,
                                &mvp,
                                [0.85, 0.2, 0.2, 1.0],
                                (
                                    layout.close.x as f32 * sc,
                                    layout.close.y as f32 * sc,
                                    b,
                                    b,
                                ),
                            );
                            crate::render3d::draw_solid(
                                gl,
                                prog,
                                &mvp,
                                [0.2, 0.45, 0.9, 1.0],
                                (
                                    layout.fullscreen.x as f32 * sc,
                                    layout.fullscreen.y as f32 * sc,
                                    b,
                                    b,
                                ),
                            );
                        });
                    }
                }
                // Toy marbles (K key): canvas-anchored glass spheres.
                // Screen-space layered discs with the solid program
                // (no new shader, no depth buffer: painter's algorithm).
                // All marbles far-to-near by depth. Before glow/menu so
                // both wash over the toys.
                if !marbles.is_empty() {
                    if let Some(prog) = solid_prog {
                        let sc = scale as f32;
                        let vw = view.x as f32 * sc;
                        let vh = view.y as f32 * sc;
                        let mid = session.to_world(
                            crate::canvas::Vec2::new(view.x / 2.0, view.y / 2.0),
                            view,
                        );
                        let mvp = crate::render3d::canvas_mvp(
                            (vw, vh),
                            (mid.x as f32 * sc, mid.y as f32 * sc),
                            session.view_camera().zoom as f32,
                            (0.0, 0.0),
                        );
                        let omvp =
                            crate::render3d::ortho(view.x as f32, view.y as f32);
                        // World (canvas px, z up) -> screen px + NDC depth.
                        // Mirrors the window draws: x*sc, -(y)*sc, z*sc.
                        let proj = |wx: f64, wy: f64, wz: f64| {
                            let n = crate::render3d::project(
                                &mvp,
                                wx as f32 * sc,
                                -(wy as f32) * sc,
                                wz as f32 * sc,
                            );
                            (
                                (n.0 * 0.5 + 0.5) * view.x as f32,
                                (1.0 - (n.1 * 0.5 + 0.5)) * view.y as f32,
                                n.2,
                            )
                        };
                        let walls: Vec<(f64, f64, f64, f64)> = server_state
                            .windows
                            .values()
                            .map(|c| {
                                (
                                    c.canvas.pos.x,
                                    c.canvas.pos.y,
                                    c.canvas.size.x,
                                    c.canvas.size.y,
                                )
                            })
                            .collect();
                        let _ = renderer.with_context(|_, gl| unsafe {
                            // Marbles: glass spheres, painter's algorithm (far
                            // first). Layered screen-space discs through the
                            // solid program: dark rim, mid glass, light
                            // core, inner motif (per-marble pattern, it
                            // advects with the roll), fixed speculars
                            // top-left + a faint bounce light.
                            let mut order: Vec<usize> =
                                (0..marbles.len()).collect();
                            {
                                let depths: Vec<f32> = marbles
                                    .iter()
                                    .map(|m| {
                                        proj(m.pos.x, m.pos.y, m.z + m.r).2
                                    })
                                    .collect();
                                order.sort_by(|a, b| {
                                    depths[*b]
                                        .partial_cmp(&depths[*a])
                                        .unwrap()
                                });
                            }
                            let tau = std::f32::consts::TAU;
                            let disc =
                                |cx: f32, cy: f32, r: f32, n: i32| -> Vec<f32> {
                                    let mut v =
                                        Vec::with_capacity((n as usize) * 6);
                                    for i in 0..n {
                                        let a0 = i as f32 / n as f32 * tau;
                                        let a1 = (i + 1) as f32 / n as f32
                                            * tau;
                                        v.extend_from_slice(&[
                                            cx,
                                            cy,
                                            cx + a0.cos() * r,
                                            cy + a0.sin() * r,
                                            cx + a1.cos() * r,
                                            cy + a1.sin() * r,
                                        ]);
                                    }
                                    v
                                };
                            let quad = |x0: f32,
                                        y0: f32,
                                        x1: f32,
                                        y1: f32,
                                        x2: f32,
                                        y2: f32,
                                        x3: f32,
                                        y3: f32|
                             -> [f32; 12] {
                                [
                                    x0, y0, x1, y1, x2, y2, x0, y0, x2, y2,
                                    x3, y3,
                                ]
                            };
                            for idx in order.iter() {
                                let m = &marbles[*idx];
                                let pc = proj(m.pos.x, m.pos.y, m.z + m.r);
                                let pr =
                                    proj(m.pos.x + m.r, m.pos.y, m.z + m.r);
                                let rs =
                                    (pr.0 - pc.0).abs().max(1.0) as f32;
                                let (cx, cy) = (pc.0 as f32, pc.1 as f32);
                                let t = m.tint;
                                let tone = |k: f64| {
                                    [
                                        (t[0] * k) as f32,
                                        (t[1] * k) as f32,
                                        (t[2] * k) as f32,
                                        1.0,
                                    ]
                                };
                                // Milk: tint pulled toward white.
                                let milk = |k: f32| {
                                    [
                                        t[0] as f32
                                            + (1.0 - t[0] as f32) * k,
                                        t[1] as f32
                                            + (1.0 - t[1] as f32) * k,
                                        t[2] as f32
                                            + (1.0 - t[2] as f32) * k,
                                        1.0,
                                    ]
                                };
                                // Rim / glass / core.
                                crate::render3d::draw_tris(
                                    gl,
                                    prog,
                                    &omvp,
                                    tone(0.30),
                                    &disc(cx, cy, rs, 18),
                                );
                                crate::render3d::draw_tris(
                                    gl,
                                    prog,
                                    &omvp,
                                    tone(0.55),
                                    &disc(cx, cy, rs * 0.80, 18),
                                );
                                crate::render3d::draw_tris(
                                    gl,
                                    prog,
                                    &omvp,
                                    tone(0.80),
                                    &disc(cx, cy, rs * 0.58, 18),
                                );
                                // Inner motif, advected with the roll.
                                let mx = cx + m.flow.x as f32 * rs;
                                let my = cy + m.flow.y as f32 * rs;
                                let mr = rs * 0.45;
                                match m.pattern {
                                    // Cat's eye: bright vertical streak.
                                    0 => {
                                        crate::render3d::draw_tris(
                                            gl,
                                            prog,
                                            &omvp,
                                            milk(0.70),
                                            &quad(
                                                mx - mr * 0.16,
                                                my - mr * 0.80,
                                                mx + mr * 0.16,
                                                my - mr * 0.80,
                                                mx + mr * 0.16,
                                                my + mr * 0.80,
                                                mx - mr * 0.16,
                                                my + mr * 0.80,
                                            ),
                                        );
                                        crate::render3d::draw_tris(
                                            gl,
                                            prog,
                                            &omvp,
                                            [1.0, 1.0, 1.0, 0.95],
                                            &quad(
                                                mx - mr * 0.06,
                                                my - mr * 0.72,
                                                mx + mr * 0.06,
                                                my - mr * 0.72,
                                                mx + mr * 0.06,
                                                my + mr * 0.72,
                                                mx - mr * 0.06,
                                                my + mr * 0.72,
                                            ),
                                        );
                                    }
                                    // Swirl: three bars turning slowly.
                                    1 => {
                                        for kk in 0..3 {
                                            let ang = m.spin as f32
                                                + kk as f32
                                                    * std::f32::consts::PI
                                                    / 3.0;
                                            let (s, c) = ang.sin_cos();
                                            let len = mr
                                                * (0.85 - kk as f32 * 0.12);
                                            let wid = mr * 0.22;
                                            let dx = c * len;
                                            let dy = s * len;
                                            let px = -s * wid;
                                            let py = c * wid;
                                            crate::render3d::draw_tris(
                                                gl,
                                                prog,
                                                &omvp,
                                                milk(0.75
                                                    - kk as f32 * 0.15),
                                                &quad(
                                                    mx + dx + px,
                                                    my + dy + py,
                                                    mx + dx - px,
                                                    my + dy - py,
                                                    mx - dx - px,
                                                    my - dy - py,
                                                    mx - dx + px,
                                                    my - dy + py,
                                                ),
                                            );
                                        }
                                    }
                                    // Ribbon: pale band across the middle.
                                    _ => {
                                        for (off, hh, kk) in [
                                            (-0.30, 0.24, 0.55),
                                            (0.0, 0.30, 0.85),
                                            (0.30, 0.24, 0.55),
                                        ] {
                                            crate::render3d::draw_tris(
                                                gl,
                                                prog,
                                                &omvp,
                                                milk(kk),
                                                &quad(
                                                    mx - mr * 0.85,
                                                    my + mr * (off - hh),
                                                    mx + mr * 0.85,
                                                    my + mr * (off - hh),
                                                    mx + mr * 0.85,
                                                    my + mr * (off + hh),
                                                    mx - mr * 0.85,
                                                    my + mr * (off + hh),
                                                ),
                                            );
                                        }
                                    }
                                }
                                // Speculars toward the lamp (upper-left):
                                // a soft glint + a hot dot.
                                crate::render3d::draw_tris(
                                    gl,
                                    prog,
                                    &omvp,
                                    [1.0, 1.0, 1.0, 0.75],
                                    &quad(
                                        cx - rs * 0.48,
                                        cy - rs * 0.52,
                                        cx - rs * 0.20,
                                        cy - rs * 0.52,
                                        cx - rs * 0.20,
                                        cy - rs * 0.24,
                                        cx - rs * 0.48,
                                        cy - rs * 0.24,
                                    ),
                                );
                                crate::render3d::draw_tris(
                                    gl,
                                    prog,
                                    &omvp,
                                    [1.0, 1.0, 1.0, 0.95],
                                    &quad(
                                        cx - rs * 0.44,
                                        cy - rs * 0.48,
                                        cx - rs * 0.32,
                                        cy - rs * 0.48,
                                        cx - rs * 0.32,
                                        cy - rs * 0.36,
                                        cx - rs * 0.44,
                                        cy - rs * 0.36,
                                    ),
                                );
                                // Faint bounce light, lower-right.
                                crate::render3d::draw_tris(
                                    gl,
                                    prog,
                                    &omvp,
                                    [t[0] as f32, t[1] as f32, t[2] as f32, 0.5],
                                    &quad(
                                        cx + rs * 0.08,
                                        cy + rs * 0.26,
                                        cx + rs * 0.42,
                                        cy + rs * 0.26,
                                        cx + rs * 0.42,
                                        cy + rs * 0.36,
                                        cx + rs * 0.08,
                                        cy + rs * 0.36,
                                    ),
                                );
                            }
                        });
                    }
                }
                // Options menu, always topmost: bordered panel, selection
                // highlight, rows drawn from the data-driven item list.
                // Super glow OVER the windows (additive light, no
                // wallpaper): Canvas mode only, menu stays on top.
                if session.state == crate::session::SuperState::Canvas
                    && glow_f > 0.01
                    && settings.glow_intensity() > 0.0
                {
                    if let Some(prog) = bg_prog {
                        let sc = scale as f32;
                        let (gcol_a, gcol_b) = settings.glow_hue();
                        let dim = settings.bg_dim() * settings.super_dim();
                        let _ = renderer.with_context(|_, gl| unsafe {
                            use smithay::backend::renderer::gles2::ffi;
                            gl.BlendFunc(ffi::ONE, ffi::ONE);
                            crate::render3d::draw_bg(
                                gl,
                                prog,
                                view.x as f32 * sc,
                                view.y as f32 * sc,
                                bg_time,
                                session.view_camera().zoom as f32,
                                dim,
                                1.0,
                                settings.depth_on(),
                                if settings.bg_anim { 1.0 } else { 0.0 },
                                settings.bg_pattern() as f32,
                                settings.bg_scale(),
                                settings.bg_grain(),
                                settings.bg_vig(),
                                settings.bg_tint(),
                                settings.wave_amp(),
                                settings.wave_shape() as f32,
                                settings.wave_octaves() as f32,
                                settings.wave_freq(),
                                settings.wave_dir(),
                                settings.wave_crisp(),
                                settings.wave_col(),
                                wave_time,
                                glow_f,
                                gcol_a,
                                gcol_b,
                                settings.glow_intensity(),
                                settings.glow_width(),
                                glow_time,
                                1.0,
                            );
                            gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
                        });
                    }
                }
                if menu.open {
                    // Atlas bytes for this frame (copied only when the
                    // cache packed new glyphs: rare after warmup).
                    let atlas_dirty = font_cache.take_dirty();
                    let atlas_bytes = if atlas_dirty || atlas_tex.is_none() {
                        Some(font_cache.atlas().to_vec())
                    } else {
                        None
                    };
                    if atlas_tex.is_none() {
                        let px: &[u8] = atlas_bytes
                            .as_deref()
                            .unwrap_or_else(|| font_cache.atlas());
                        let up = renderer.with_context(|r, gl| unsafe {
                            use smithay::backend::renderer::gles2::ffi;
                            let mut tex = 0;
                            gl.GenTextures(1, &mut tex as *mut _);
                            gl.BindTexture(ffi::TEXTURE_2D, tex);
                            gl.TexImage2D(
                                ffi::TEXTURE_2D,
                                0,
                                ffi::RGBA as i32,
                                crate::fontvec::ATLAS_SIDE as i32,
                                crate::fontvec::ATLAS_SIDE as i32,
                                0,
                                ffi::RGBA,
                                ffi::UNSIGNED_BYTE as u32,
                                px.as_ptr() as *const _,
                            );
                            gl.TexParameteri(
                                ffi::TEXTURE_2D,
                                ffi::TEXTURE_MIN_FILTER,
                                ffi::LINEAR as i32,
                            );
                            gl.TexParameteri(
                                ffi::TEXTURE_2D,
                                ffi::TEXTURE_MAG_FILTER,
                                ffi::LINEAR as i32,
                            );
                            gl.TexParameteri(
                                ffi::TEXTURE_2D,
                                ffi::TEXTURE_WRAP_S,
                                ffi::CLAMP_TO_EDGE as i32,
                            );
                            gl.TexParameteri(
                                ffi::TEXTURE_2D,
                                ffi::TEXTURE_WRAP_T,
                                ffi::CLAMP_TO_EDGE as i32,
                            );
                            smithay::backend::renderer::gles2::Gles2Texture::from_raw(
                                r,
                                tex,
                                Size::<i32, BufferCoord>::from((512, 512)),
                            )
                        });
                        match up {
                            Ok(t) => {
                                atlas_tex = Some(t);
                                log::info!("menu vector atlas ready");
                            }
                            Err(e) => log::warn!("menu atlas upload: {:?}", e),
                        }
                    }
                    // New glyphs packed this frame: refresh the texture.
                    if atlas_dirty {
                        if let Some(bytes) = atlas_bytes {
                            let _ = renderer.with_context(|_, gl| unsafe {
                                use smithay::backend::renderer::gles2::ffi;
                                if let Some(atlas) = atlas_tex.as_ref() {
                                    gl.BindTexture(ffi::TEXTURE_2D, atlas.tex_id());
                                    gl.TexSubImage2D(
                                        ffi::TEXTURE_2D,
                                        0,
                                        0,
                                        0,
                                        crate::fontvec::ATLAS_SIDE as i32,
                                        crate::fontvec::ATLAS_SIDE as i32,
                                        ffi::RGBA,
                                        ffi::UNSIGNED_BYTE as u32,
                                        bytes.as_ptr() as *const _,
                                    );
                                }
                            });
                        }
                    }
                    if tex_prog.is_none() {
                        match renderer.with_context(|_, gl| {
                            crate::render3d::compile_textured(gl)
                        }) {
                            Ok(Ok(p)) => {
                                tex_prog = Some(p);
                                log::info!("textured GL program ready");
                            }
                            Ok(Err(e)) => log::warn!("textured GL program: {}", e),
                            Err(e) => log::warn!("textured GL context: {:?}", e),
                        }
                    }
                    if solid_prog.is_none() {
                        match renderer.with_context(|_, gl| {
                            crate::render3d::compile_solid(gl)
                        }) {
                            Ok(Ok(p)) => {
                                solid_prog = Some(p);
                                log::info!("chrome GL program ready");
                            }
                            Ok(Err(e)) => log::warn!("chrome GL program: {}", e),
                            Err(e) => log::warn!("chrome GL context: {:?}", e),
                        }
                    }
                    if round_prog.is_none() {
                        match renderer.with_context(|_, gl| {
                            crate::render3d::compile_rounded(gl)
                        }) {
                            Ok(Ok(p)) => {
                                round_prog = Some(p);
                                log::info!("rounded GL program ready");
                            }
                            Ok(Err(e)) => log::warn!("rounded GL program: {}", e),
                            Err(e) => log::warn!("rounded GL context: {:?}", e),
                        }
                    }
                    if let (Some(sprog), Some(tprog), Some(atlas), Some(rprog)) =
                        (solid_prog, tex_prog, atlas_tex.as_ref(), round_prog)
                    {
                        let mvp = crate::render3d::ortho(view.x as f32, view.y as f32);
                        let sc = scale as f32;
                        // Fixed screen-space popup: centred on the view in
                        // SCREEN px, drawn 1:1 — Super dolly and pans never
                        // resize or move it.
                        let lay = crate::menu::layout(
                            menu.tab,
                            view.x / 2.0,
                            view.y / 2.0,
                        );
                        let to_px =
                            |wx: f64, wy: f64| ((wx as f32) * sc, (wy as f32) * sc);
                        // Slide-in: the panel drops into place over ~0.18s.
                        let slide_k = menu_opened_at
                            .map(|t| {
                                (std::time::Instant::now()
                                    .duration_since(t)
                                    .as_secs_f32()
                                    / 0.18)
                                    .clamp(0.0, 1.0)
                            })
                            .unwrap_or(1.0);
                        let slide_px = -26.0 * sc * (1.0 - slide_k);
                        let (px0, py0) = to_px(lay.x, lay.y);
                        let py0 = py0 + slide_px;
                        // Popup scale is the output scale ONLY — never the
                        // canvas zoom (impossible to resize by zooming).
                        let pw = lay.w as f32 * sc;
                        let ph = lay.h as f32 * sc;
                        let pad = crate::menu::PANEL_PAD as f32 * sc;
                        let row_h = crate::menu::ROW_H as f32 * sc;
                        let atlas_id = atlas.tex_id();
                        // House accent: deep teal, ambience-grade.
                        let accent = [0.20, 0.75, 0.70, 1.0];
                        let rows = crate::menu::tab_items(menu.tab);
                        let feet = crate::menu::feet();
                        let _ = renderer.with_context(|_, gl| unsafe {
                            // Drop shadow, then border, then panel — all
                            // rounded (border = bigger rounded rect under
                            // the panel one).
                            let corner = 14.0 * sc;
                            crate::render3d::draw_rounded(
                                gl,
                                rprog,
                                &mvp,
                                [0.01, 0.015, 0.03, 1.0],
                                (px0 - 6.0 * sc, py0 - 2.0 * sc + 12.0 * sc, pw + 12.0 * sc, ph + 8.0 * sc),
                                corner,
                            );
                            crate::render3d::draw_rounded(
                                gl,
                                rprog,
                                &mvp,
                                [0.16, 0.21, 0.33, 1.0],
                                (px0 - 2.0 * sc, py0 - 2.0 * sc, pw + 4.0 * sc, ph + 4.0 * sc),
                                corner,
                            );
                            crate::render3d::draw_rounded(
                                gl,
                                rprog,
                                &mvp,
                                [0.078, 0.094, 0.13, 1.0],
                                (px0, py0, pw, ph),
                                corner,
                            );
                            // Tab bar: names centred in their cell, the active
                            // one underlined in the house accent.
                            let tab_top = py0 + pad;
                            let cell_w = pw / crate::menu::TABS.len() as f32;
                            for (ti, name) in crate::menu::TABS.iter().enumerate() {
                                let active = ti == menu.tab;
                                let name_w =
                                    font_cache.measure(name) * sc;
                                let tx = px0 + ti as f32 * cell_w
                                    + (cell_w - name_w) / 2.0;
                                draw_menu_line(
                                    gl, tprog, &mvp, atlas_id, &mut font_cache, tx,
                                    tab_top + crate::menu::TEXT_DY as f32 * sc, sc, name,
                                );
                                if active {
                                    crate::render3d::draw_solid(
                                        gl,
                                        sprog,
                                        &mvp,
                                        accent,
                                        (
                                            tx - 4.0 * sc,
                                            tab_top + row_h - 5.0 * sc,
                                            name_w + 8.0 * sc,
                                            3.0 * sc,
                                        ),
                                    );
                                }
                            }
                            // Option rows below the tab bar.
                            let mut item_idx = 0usize;
                            for (i, row) in rows.iter().enumerate() {
                                let (_, rpy) = to_px(lay.x, lay.y + crate::menu::PANEL_PAD + (i + 1) as f64 * crate::menu::ROW_H);
                                let rpy = rpy + slide_px;
                                let ty = rpy + crate::menu::TEXT_DY as f32 * sc;
                                let crate::menu::Row::Item(it) = row else {
                                    continue;
                                };
                                if item_idx == menu.selected {
                                    crate::render3d::draw_rounded(
                                        gl,
                                        rprog,
                                        &mvp,
                                        [0.13, 0.16, 0.26, 1.0],
                                        (px0 + 8.0 * sc, rpy + 1.0 * sc, pw - 16.0 * sc, row_h - 2.0 * sc),
                                        6.0 * sc,
                                    );
                                    crate::render3d::draw_solid(
                                        gl,
                                        sprog,
                                        &mvp,
                                        accent,
                                        (px0 + 8.0 * sc, rpy + 4.0 * sc, 4.0 * sc, row_h - 8.0 * sc),
                                    );
                                }
                                let line = format!(
                                    "{}{}",
                                    it.label,
                                    it.state_text(&settings)
                                );
                                draw_menu_line(
                                    gl, tprog, &mvp, atlas_id, &mut font_cache, px0 + pad, ty, sc,
                                    &line,
                                );
                                // Slider track + fill under the text.
                                if let Some(fill) = it.slider_fill(&settings) {
                                    let tw = 260.0 * sc;
                                    let th = crate::menu::TRACK_H as f32 * sc;
                                    let tx0 = px0 + pad;
                                    let ty0 = rpy + row_h
                                        - (crate::menu::TRACK_PAD_BOTTOM as f32
                                            + crate::menu::TRACK_H as f32)
                                            * sc;
                                    crate::render3d::draw_rounded(
                                        gl,
                                        rprog,
                                        &mvp,
                                        [0.17, 0.22, 0.33, 1.0],
                                        (tx0, ty0, tw, th),
                                        crate::menu::TRACK_H as f32 * sc / 2.0,
                                    );
                                    crate::render3d::draw_rounded(
                                        gl,
                                        rprog,
                                        &mvp,
                                        accent,
                                        (tx0, ty0, tw * fill.clamp(0.0, 1.0), th),
                                        crate::menu::TRACK_H as f32 * sc / 2.0,
                                    );
                                }
                                // Toggle pill, right-aligned: accent when
                                // on, dark when off, knob rides the side.
                                if let crate::menu::MenuKind::Toggle { is_on, .. } = &it.kind {
                                    let on = is_on(&settings);
                                    let plw = 46.0 * sc;
                                    let plh = 20.0 * sc;
                                    let plx = px0 + pw - pad - plw;
                                    let ply = rpy + (row_h - plh) * 0.5;
                                    let knob = 16.0 * sc;
                                    crate::render3d::draw_rounded(
                                        gl,
                                        rprog,
                                        &mvp,
                                        if on { accent } else { [0.17, 0.22, 0.33, 1.0] },
                                        (plx, ply, plw, plh),
                                        10.0 * sc,
                                    );
                                    crate::render3d::draw_rounded(
                                        gl,
                                        rprog,
                                        &mvp,
                                        [0.92, 0.95, 1.0, 1.0],
                                        (
                                            if on { plx + plw - knob - 2.0 * sc } else { plx + 2.0 * sc },
                                            ply + 2.0 * sc,
                                            knob,
                                            knob,
                                        ),
                                        8.0 * sc,
                                    );
                                }
                                // Live colour swatch, right-aligned.
                                if let Some(rgb) = it.swatch.map(|f| f(&settings)) {
                                    let sw = 20.0 * sc;
                                    let sx = px0 + pw - pad - sw;
                                    let sy = rpy + 5.0 * sc;
                                    crate::render3d::draw_rounded(
                                        gl,
                                        rprog,
                                        &mvp,
                                        [0.16, 0.21, 0.33, 1.0],
                                        (sx - 2.0 * sc, sy - 2.0 * sc, sw + 4.0 * sc, sw + 4.0 * sc),
                                        4.0 * sc,
                                    );
                                    crate::render3d::draw_rounded(
                                        gl,
                                        rprog,
                                        &mvp,
                                        [
                                            rgb[0] as f32 / 255.0,
                                            rgb[1] as f32 / 255.0,
                                            rgb[2] as f32 / 255.0,
                                            1.0,
                                        ],
                                        (sx, sy, sw, sw),
                                        4.0 * sc,
                                    );
                                }
                                item_idx += 1;
                            }
                            // Feet under the rows.
                            for (fi, foot) in feet.iter().enumerate() {
                                let (_, fpy) = to_px(lay.x, lay.y + crate::menu::PANEL_PAD + (rows.len() + 1 + fi) as f64 * crate::menu::ROW_H);
                                draw_menu_line(
                                    gl, tprog, &mvp, atlas_id, &mut font_cache, px0 + pad,
                                    fpy + slide_px + crate::menu::TEXT_DY as f32 * sc, sc, foot,
                                );
                            }
                        });
                    }
                }
                last_chrome = chrome_now;
            })?;
            // Frame callbacks: tell clients this frame was presented, so
            // paced clients (Xwayland damage throttling, animations) keep
            // committing instead of stalling after their first buffer.
            // Output enter goes out first sight per surface: Qt waits for
            // `wl_surface.enter` before its first frame.
            let now_ms = frame_clock.elapsed().as_millis() as u32;
            let surfaces: Vec<WlSurface> = server_state
                .windows
                .values()
                .filter_map(|cw| cw.surface.clone())
                .collect();
            for surface in &surfaces {
                if server_state.claim_output_enter(surface) {
                    server.enter_output(surface);
                }
            }
            for cw in server_state.windows.values() {
                if let Some(surface) = cw.surface.as_ref() {
                    let pending = with_states(surface, |states| {
                        std::mem::take(
                            &mut states
                                .cached_state
                                .current::<SurfaceAttributes>()
                                .frame_callbacks,
                        )
                    });
                    if let Ok(callbacks) = pending {
                        for callback in callbacks {
                            callback.done(now_ms);
                        }
                    }
                }
            }
            frames += 1;
            if let Some(max) = max_frames {
                if frames >= max {
                    log::info!("frame budget reached ({}), exiting", frames);
                    break;
                }
            }
        }
        // Frame pump: winit run_return sleeps without OS events, so
        // without this every glide/fade/flow advances only on incoming
        // events — one spring step per event, i.e. visible notches
        // (wheel zoom jumping per notch, dolly stuttering). While this
        // iteration asked for a frame, queue a redraw: it wakes the
        // next dispatch at once and chains until nothing moves, then
        // stops by itself (a single spare frame at most). Zero frames
        // at rest, always; spring feel untouched.
        if need_render {
            graphics.window().request_redraw();
        }

        std::thread::sleep(Duration::from_millis(8));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_deepens_when_zoomed_out() {
        let near = Camera {
            zoom: 1.5,
            ..Camera::new()
        };
        let far = Camera {
            zoom: 0.1,
            ..Camera::new()
        };
        let c_near = background_color(&near);
        let c_far = background_color(&far);
        assert!(c_far[0] < c_near[0]);
        assert!(c_far[1] < c_near[1]);
        for c in [c_near, c_far] {
            assert_eq!(c[3], 1.0);
            assert!(c[0] >= 0.0 && c[1] <= 0.2);
        }
    }

    #[test]
    fn release_follows_press_not_focus() {
        use std::collections::HashMap;
        // Press in A (xid 11), focus moves to B (xid 22): the release
        // still goes to A.
        let mut routes: HashMap<u32, Option<u32>> = HashMap::new();
        routes.insert(30, Some(11));
        assert_eq!(resolve_release(&routes, 30, Some(22)), Some(11));
        // Press on the seat (native focus), focus moves to X: release
        // goes to the seat, never to X.
        routes.insert(31, None);
        assert_eq!(resolve_release(&routes, 31, Some(22)), None);
        // No recorded press: current focus wins (predating press).
        assert_eq!(resolve_release(&routes, 99, Some(22)), Some(22));
        assert_eq!(resolve_release(&routes, 99, None), None);
    }

    #[test]
    fn first_press_wins_over_repeats() {
        use std::collections::HashMap;
        let mut routes: HashMap<u32, Option<u32>> = HashMap::new();
        // Initial press in A records A.
        assert_eq!(note_press(&mut routes, 30, Some(11)), Some(11));
        // Auto-repeat after a focus move to B must NOT re-route.
        assert_eq!(note_press(&mut routes, 30, Some(22)), Some(11));
        // The release then follows the press, not the focus.
        assert_eq!(resolve_release(&routes, 30, Some(22)), Some(11));
    }
}
