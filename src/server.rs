//! Wayland frontend skeleton (feature `compositor` only).
//!
//! Display + socket + core globals (compositor, shm, xdg_shell). Every new
//! xdg-toplevel becomes a CanvasWindow with cascade placement on the shared
//! canvas. Client buffer rendering and seat input land next.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use smithay::backend::input::KeyState;
use smithay::backend::renderer::gles2::Gles2Texture;
use smithay::reexports::wayland_protocols::unstable::xdg_shell::v6::server::zxdg_shell_v6::ZxdgShellV6;
use smithay::reexports::wayland_protocols::xdg_shell::server::xdg_wm_base::XdgWmBase;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_data_device_manager::WlDataDeviceManager;
use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::protocol::wl_subcompositor::WlSubcompositor;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_pointer::ButtonState as WlButtonState;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Display, Global};
use slog::Drain as _;
use smithay::utils::{Logical, Point};
use smithay::wayland::compositor::compositor_init;
use smithay::wayland::seat::{AxisFrame, KeyboardHandle, PointerHandle, Seat, XkbConfig};
use smithay::wayland::shell::xdg::{xdg_shell_init, ShellState, XdgRequest};
use smithay::wayland::shm::init_shm_global;
use smithay::wayland::Serial;

use crate::canvas::{CanvasWindow, Vec2};

/// Default size for a fresh client window; the client configures itself after.
pub const DEFAULT_SIZE: (f64, f64) = (800.0, 600.0);

/// Cascade placement on the canvas: window n sits one step down-right from
/// the previous, wrapping every 8 so the pile stays near the origin.
pub fn place_window(id: u64, n: usize) -> CanvasWindow {
    let step = (n % 8) as f64;
    CanvasWindow::new(
        id,
        96.0 + step * 64.0,
        96.0 + step * 48.0,
        DEFAULT_SIZE.0,
        DEFAULT_SIZE.1,
    )
}

#[derive(Default)]
pub struct ServerState {
    pub windows: HashMap<u64, ClientWindow>,
    next_id: u64,
    /// Stacking high-water mark; each raise bumps it.
    z_top: u64,
    /// Every live wl_surface by its numeric id, XWayland's included.
    /// XWayland publishes each X window's surface id through the
    /// WL_SURFACE_ID client message; the backend resolves it here and
    /// renders the real surface zero-copy (no GetImage bridge).
    surface_ids: HashMap<u32, WlSurface>,
}

/// A client window: canvas placement plus live surface and GPU texture.
/// The texture is refreshed from newly committed shm buffers each frame.
/// `x11` marks XWayland-backed windows: the surface is XWayland's own
/// (resolved from its WL_SURFACE_ID), rendered zero-copy like any window.
pub struct ClientWindow {
    pub canvas: CanvasWindow,
    pub surface: Option<WlSurface>,
    pub texture: Option<Gles2Texture>,
    /// Last attached buffer, kept so damage-only commits (repaint into
    /// the same shm buffer, no new attach) can re-upload pixels.
    pub last_buffer: Option<WlBuffer>,
    pub x11: Option<u32>,
    /// Fullscreen state: previous canvas rect, restored on toggle-off.
    pub fullscreen_prev: Option<CanvasWindow>,
    /// Last imported buffer dims: the canvas follows the buffer only when
    /// the client resizes (manual moves/resizes never snap back).
    pub buf_size: Option<(u32, u32)>,
    /// View lock for layer-shell panels: positioned per-frame from the
    /// viewport anchor (sticks to the screen, never joins canvas layout,
    /// hit-testing or keyboard focus).
    pub view_lock: Option<crate::layer::ViewLock>,
    /// Stacking order: higher is on top. Bumped on click-focus and grab.
    pub z: u64,
}

impl ClientWindow {
    fn new(id: u64, surface: Option<WlSurface>, n: usize) -> Self {
        Self {
            canvas: place_window(id, n),
            surface,
            texture: None,
            last_buffer: None,
            x11: None,
            fullscreen_prev: None,
            buf_size: None,
            view_lock: None,
            z: 0,
        }
    }
}

impl ServerState {
    pub fn add_toplevel(&mut self, surface: Option<WlSurface>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let win = ClientWindow::new(id, surface, self.windows.len());
        self.windows.insert(id, win);
        self.raise(id);
        id
    }

    /// Create a canvas window for an XWayland surface (resolved from its
    /// WL_SURFACE_ID) and tag it with its X window id for XTEST routing.
    pub fn add_x11_window(&mut self, surface: WlSurface, xid: u32) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut win = ClientWindow::new(id, Some(surface), self.windows.len());
        win.x11 = Some(xid);
        self.windows.insert(id, win);
        self.raise(id);
        id
    }

    /// Create a view-locked layer-shell panel: no cascade placement, no
    /// focus steal, positioned per-frame by the backend from its anchor.
    pub fn add_layer_panel(
        &mut self,
        surface: Option<WlSurface>,
        lock: crate::layer::ViewLock,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut win = ClientWindow::new(id, surface, 0);
        win.view_lock = Some(lock);
        self.windows.insert(id, win);
        self.raise(id);
        id
    }

    /// Resolve a live surface by its numeric Wayland id (XWayland's
    /// WL_SURFACE_ID).
    pub fn surface_by_id(&self, id: u32) -> Option<WlSurface> {
        self.surface_ids.get(&id).cloned()
    }

    /// Drop a surface id when its window dies (called with the sid the Xwm
    /// reported for the destroyed X window).
    pub fn forget_surface(&mut self, id: u32) {
        self.surface_ids.remove(&id);
    }

    /// Close a window: XWayland-backed windows die through the WM (the
    /// backend calls back with the xid); native windows drop here.
    /// Returns the X window id when the backend must kill it.
    pub fn close_window(&mut self, id: u64) -> Option<u32> {
        self.windows.remove(&id).and_then(|w| w.x11)
    }

    /// Topmost window containing a canvas-plane point (highest z wins).
    /// All hover/click/dwell picks go through here so overlapping windows
    /// resolve to the visible one.
    pub fn topmost_at(&self, p: crate::canvas::Vec2) -> Option<u64> {
        self.windows
            .iter()
            .filter(|(_, c)| c.view_lock.is_none() && c.canvas.contains(p))
            .max_by_key(|(_, c)| c.z)
            .map(|(id, _)| *id)
    }

    /// Bring a window to the front (click-focus, grab).
    pub fn raise(&mut self, id: u64) {
        if self.windows.contains_key(&id) {
            self.z_top += 1;
            if let Some(w) = self.windows.get_mut(&id) {
                w.z = self.z_top;
            }
        }
    }

    /// Toggle fullscreen: the window takes the whole view, restored on
    /// toggle-off. The rect fills the view exactly (true fullscreen);
    /// the backend also configures X clients to the new size so their
    /// buffer follows instead of stretching.
    pub fn toggle_fullscreen(&mut self, id: u64, view: &CanvasWindow) -> bool {
        if let Some(win) = self.windows.get_mut(&id) {
            if let Some(prev) = win.fullscreen_prev.take() {
                win.canvas = prev;
                false
            } else {
                win.fullscreen_prev = Some(win.canvas);
                win.canvas = CanvasWindow::new(win.canvas.id, view.pos.x, view.pos.y, view.size.x, view.size.y);
                true
            }
        } else {
            false
        }
    }

    /// Windows on the canvas, sorted by id.
    pub fn canvas_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .windows
            .iter()
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        ids
    }
}

/// Stderr drain for Smithay's slog seat logs. Level follows
/// `CANVAS_SEAT_TRACE` (trace when set, debug otherwise); keycodes are
/// numbers only, no keysyms or text ever reach the log.
struct SeatLogDrain;
impl slog::Drain for SeatLogDrain {
    type Ok = ();
    type Err = slog::Never;

    fn log(
        &self,
        record: &slog::Record,
        _values: &slog::OwnedKVList,
    ) -> Result<(), slog::Never> {
        eprintln!("[seat {}] {}", record.level(), record.msg());
        Ok(())
    }
}

fn seat_logger() -> slog::Logger {
    let level = if std::env::var("CANVAS_SEAT_TRACE").is_ok() {
        slog::Level::Trace
    } else {
        slog::Level::Debug
    };
    let drain = slog::LevelFilter::new(SeatLogDrain, level).fuse();
    slog::Logger::root(drain, slog::o!())
}

/// Owns the Wayland display and keeps the globals alive. Pump it from the
/// backend event loop; client windows appear in the passed ServerState.
///
/// Also owns the seat: one keyboard + one pointer. The backend feeds raw
/// input here; the seat routes it to the focused surfaces.
pub struct Server {
    display: Rc<RefCell<Display>>,
    _compositor: Global<WlCompositor>,
    _subcompositor: Global<WlSubcompositor>,
    _shm: Global<wl_shm::WlShm>,
    _shell_state: Arc<Mutex<ShellState>>,
    _xdg: Global<XdgWmBase>,
    _zxdg: Global<ZxdgShellV6>,
    /// xdg_shell version 3 (modern GTK binds the highest version).
    _xdg_v3: Global<smithay::reexports::wayland_protocols::xdg_shell::server::xdg_wm_base::XdgWmBase>,
    /// Clipboard + drag-and-drop manager (foot, GTK, Qt all require it).
    _data_manager: Global<WlDataDeviceManager>,
    /// Layer shell for panels and notifications (mako).
    _layer_shell:
        Global<smithay::reexports::wayland_protocols::wlr::unstable::layer_shell::v1::server::zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    /// Device registry shared with protocol handlers and the DnD grab.
    data_registry: crate::data::SharedRegistry,
    /// Last pressed pointer button, for drag-grab start data.
    last_press: Option<u32>,
    /// Last pointer location + focus, for drag-grab start data.
    last_pointer: (f64, f64),
    last_focus: Option<(WlSurface, (i32, i32))>,
    _seat: Seat,
    _seat_global: Global<smithay::reexports::wayland_server::protocol::wl_seat::WlSeat>,
    /// Single wl_output v4 (Smithay's advertises v3, which mako refuses).
    _output_global: Global<WlOutput>,
    keyboard: KeyboardHandle,
    pointer: PointerHandle,
    serial: u32,
    start: Instant,
}

impl Server {
    pub fn new() -> io::Result<(Self, OsString)> {
        let display = Rc::new(RefCell::new(Display::new()));
        let (compositor, subcompositor) = compositor_init(
            &mut display.borrow_mut(),
            |surface, mut dispatch| {
                if let Some(state) = dispatch.get::<ServerState>() {
                    let sid = surface.as_ref().id();
                    log::info!("surface created id={}", sid);
                    state.surface_ids.insert(sid, surface.clone());
                }
            },
            None,
        );
        let shm = init_shm_global(
            &mut display.borrow_mut(),
            vec![wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888],
            None,
        );
        let (shell_state, xdg, zxdg) = xdg_shell_init(
            &mut display.borrow_mut(),
            |request, mut dispatch| match request {
                XdgRequest::NewToplevel { surface } => {
                    if let Some(state) = dispatch.get::<ServerState>() {
                        let wl_surface = surface.get_surface().cloned();
                        let id = state.add_toplevel(wl_surface);
                        log::info!("new toplevel -> canvas window {}", id);
                        surface.send_configure();
                    }
                }
                _ => {}
            },
            None,
        );
        // Modern clients (GTK 4) refuse the v1 global above; offer v3.
        let xdg_v3 = crate::xdg_v3::init_xdg_v3(&mut display.borrow_mut());
        // Clipboard + DnD via our own manager (Smithay 0.3 hides the
        // device registry and drag grab from downstream crates).
        let data_registry = crate::data::new_registry();
        let data_manager =
            crate::data::init_manager(&mut display.borrow_mut(), data_registry.clone());
        // Layer shell for panels and notifications (mako).
        let layer_shell = crate::layer::init_layer_shell(&mut display.borrow_mut());
        let socket = display.borrow_mut().add_socket_auto()?;
        let (mut seat, seat_global) =
            Seat::new(&mut display.borrow_mut(), "seat-0".into(), Some(seat_logger()));
        let keyboard = seat
            .add_keyboard(XkbConfig::default(), 200, 25, |_, _| {})
            .map_err(|err| io::Error::new(io::ErrorKind::Other, format!("seat keyboard: {:?}", err)))?;
        let pointer = seat.add_pointer(|_| {});
        // One output viewport on the shared canvas. XWayland sizes its
        // screen from wl_output and stalls without one; real clients use
        // it for DPI/positioning. Our own v4 global: Smithay's advertises
        // v3, which mako refuses to bind.
        let output_global = crate::output::init_output(&mut display.borrow_mut());
        Ok((
            Self {
                display,
                _compositor: compositor,
                _subcompositor: subcompositor,
                _shm: shm,
                _shell_state: shell_state,
                _xdg: xdg,
                _zxdg: zxdg,
                _xdg_v3: xdg_v3,
                _data_manager: data_manager,
                _layer_shell: layer_shell,
                data_registry,
                last_press: None,
                last_pointer: (0.0, 0.0),
                last_focus: None,
                _seat: seat,
                _seat_global: seat_global,
                _output_global: output_global,
                keyboard,
                pointer,
                serial: 0,
                start: Instant::now(),
            },
            socket,
        ))
    }

    /// Process pending client messages without blocking, then flush replies.
    pub fn pump(&mut self, state: &mut ServerState) {
        let _ = self
            .display
            .borrow_mut()
            .dispatch(Duration::from_millis(0), state);
        self.display.borrow_mut().flush_clients(state);
        self.drain_data_device();
    }

    /// Shared display handle, for XWayland startup (same-thread use).
    pub fn display(&self) -> Rc<RefCell<Display>> {
        Rc::clone(&self.display)
    }

    fn next_serial(&mut self) -> Serial {
        self.serial = self.serial.wrapping_add(1);
        Serial::from(self.serial)
    }

    fn now_ms(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    /// Forward a key to the focused client. `evdev` is the kernel keycode.
    /// Super and Esc never reach here: the backend consumes them as gestures.
    pub fn input_key(&mut self, evdev: u32, pressed: bool) {
        log::debug!("seat input_key evdev={} pressed={}", evdev, pressed);
        let state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        let serial = self.next_serial();
        let time = self.now_ms();
        self.keyboard.input(evdev, state, serial, time, |_, _| true);
    }

    /// Move keyboard focus. Call with None then the new surface when the
    /// window set changes, so late-bound clients also receive Enter.
    pub fn focus_keyboard(&mut self, surface: Option<&WlSurface>) {
        let serial = self.next_serial();
        self.keyboard.set_focus(surface, serial);
        // Selection offers must follow keyboard focus for copy/paste.
        self.focus_data_device(surface);
    }

    /// Mirror keyboard focus into the registry so selection and DnD offers
    /// reach the right client. X11-backed windows clear it.
    pub fn focus_data_device(&self, surface: Option<&WlSurface>) {
        crate::data::set_focus_client(&self.data_registry, surface);
    }

    /// Install a client-requested drag as a pointer grab once per frame.
    pub fn drain_data_device(&mut self) {
        if crate::data::drag_finished(&self.data_registry) {
            crate::data::clear_drag(&self.data_registry);
            return;
        }
        let serial = self.next_serial();
        let button = self.last_press.unwrap_or(0x110);
        let location: Point<f64, Logical> = self.last_pointer.into();
        let focus = self.last_focus.clone().map(|(s, (x, y))| {
            (
                s,
                Point::<i32, Logical>::from((x, y)),
            )
        });
        crate::data::install_pending_drag(
            &self.data_registry,
            &self._seat,
            serial,
            button,
            location,
            focus,
        );
    }

    /// Forward pointer motion. `logical` is the output-space position,
    /// `focus` the surface under the cursor with the surface ORIGIN in
    /// logical space (Smithay subtracts it to report surface-local coords).
    pub fn input_motion(
        &mut self,
        logical: (f64, f64),
        focus: Option<(WlSurface, (i32, i32))>,
    ) {
        log::debug!(
            "seat input_motion logical=({},{}) focus={}",
            logical.0,
            logical.1,
            focus.is_some()
        );
        self.last_pointer = logical;
        self.last_focus = focus.clone();
        let serial = self.next_serial();
        let time = self.now_ms();
        let location: Point<f64, Logical> = logical.into();
        let focus = focus.map(|(surface, (x, y))| {
            let local: Point<i32, Logical> = (x, y).into();
            (surface, local)
        });
        self.pointer.motion(location, focus, serial, time);
    }

    /// Forward a pointer button. `code` is the evdev button code.
    pub fn input_button(&mut self, code: u32, pressed: bool) {
        let state = if pressed {
            WlButtonState::Pressed
        } else {
            WlButtonState::Released
        };
        if pressed {
            self.last_press = Some(code);
        }
        let serial = self.next_serial();
        let time = self.now_ms();
        self.pointer.button(code, state, serial, time);
    }

    /// Forward a scroll frame to the focused client.
    pub fn input_axis(&mut self, frame: AxisFrame) {
        self.pointer.axis(frame);
    }
}

/// Canvas position of the nth window, for tests and future tiling helpers.
pub fn cascade_origin(n: usize) -> Vec2 {
    let step = (n % 8) as f64;
    Vec2::new(96.0 + step * 64.0, 96.0 + step * 48.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cascade_steps_down_right_and_wraps() {
        let a = cascade_origin(0);
        let b = cascade_origin(1);
        assert!(b.x > a.x && b.y > a.y);
        assert_eq!(cascade_origin(8), cascade_origin(0));
    }

    #[test]
    fn toplevels_get_unique_ids_and_windows() {
        let mut s = ServerState::default();
        let first = s.add_toplevel(None);
        let second = s.add_toplevel(None);
        assert_ne!(first, second);
        assert_eq!(s.windows.len(), 2);
        let wa = &s.windows[&first].canvas;
        let wb = &s.windows[&second].canvas;
        assert!(wb.pos.x > wa.pos.x && wb.pos.y > wa.pos.y);
    }

    #[test]
    fn topmost_pick_and_raise() {
        let mut s = ServerState::default();
        let a = s.add_toplevel(None);
        let b = s.add_toplevel(None);
        // Stack both on the same spot: newest (b) starts on top.
        {
            let r = CanvasWindow::new(a, 0.0, 0.0, 100.0, 100.0);
            s.windows.get_mut(&a).unwrap().canvas = r;
            let r = CanvasWindow::new(b, 0.0, 0.0, 100.0, 100.0);
            s.windows.get_mut(&b).unwrap().canvas = r;
        }
        assert_eq!(s.topmost_at(Vec2::new(10.0, 10.0)), Some(b));
        // Click-focus raises a to the front; empty space hits nothing.
        s.raise(a);
        assert_eq!(s.topmost_at(Vec2::new(10.0, 10.0)), Some(a));
        assert_eq!(s.topmost_at(Vec2::new(500.0, 500.0)), None);
        // Raising an unknown id is a no-op (no panic, no reorder).
        let before = s.topmost_at(Vec2::new(10.0, 10.0));
        s.raise(9999);
        assert_eq!(s.topmost_at(Vec2::new(10.0, 10.0)), before);
    }

    #[test]
    fn fullscreen_fills_view_and_restores() {
        let mut s = ServerState::default();
        let id = s.add_toplevel(None);
        // Pretend the client buffer is 486x318 at cascade position.
        {
            let win = s.windows.get_mut(&id).unwrap();
            win.canvas = CanvasWindow::new(id, 40.0, 40.0, 486.0, 318.0);
        }
        let view = CanvasWindow::new(u64::MAX, 0.0, 0.0, 1280.0, 800.0);
        assert!(s.toggle_fullscreen(id, &view));
        let full = s.windows[&id].canvas;
        // True fullscreen: the rect IS the view, no letterbox.
        assert_eq!(full.pos, Vec2::new(0.0, 0.0));
        assert_eq!(full.size, Vec2::new(1280.0, 800.0));
        assert!(!s.toggle_fullscreen(id, &view));
        let back = s.windows[&id].canvas;
        assert_eq!(back.pos, Vec2::new(40.0, 40.0));
        assert_eq!(back.size, Vec2::new(486.0, 318.0));
    }
}

/// In-process seat proof: a real client connects over the socket, maps a
/// toplevel, then the server focuses it and sends key + pointer events.
/// No X, no winit, no xdotool: pump and dispatch interleave on one thread.
#[cfg(test)]
mod seat_tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    use wayland_client::protocol::wl_compositor::WlCompositor as ClientCompositor;
    use wayland_client::protocol::wl_keyboard::Event as ClientKeyEvent;
    use wayland_client::protocol::wl_pointer::Event as ClientPointerEvent;
    use wayland_client::protocol::wl_seat::WlSeat as ClientSeat;
    use wayland_client::{Display as ClientDisplay, GlobalManager};
    use wayland_protocols::xdg_shell::client::{xdg_surface, xdg_wm_base};

    fn pump_both(
        server: &mut Server,
        state: &mut ServerState,
        display: &ClientDisplay,
        queue: &mut wayland_client::EventQueue,
        rounds: usize,
    ) {
        use std::io::ErrorKind;
        for _ in 0..rounds {
            if let Err(e) = display.flush() {
                assert_eq!(e.kind(), ErrorKind::WouldBlock, "client flush: {:?}", e);
            }
            server.pump(state);
            if let Some(guard) = queue.prepare_read() {
                if let Err(e) = guard.read_events() {
                    assert_eq!(e.kind(), ErrorKind::WouldBlock, "client read: {:?}", e);
                }
            }
            queue
                .dispatch_pending(&mut (), |_, _, _| {})
                .expect("client dispatch");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn seat_forwards_key_and_pointer_to_focused_window() {
        std::env::set_var("XDG_RUNTIME_DIR", std::env::temp_dir());
        let (mut server, socket) = Server::new().expect("server");
        let socket_name = socket.to_str().expect("socket name").to_owned();
        let mut state = ServerState::default();

        let display = ClientDisplay::connect_to_name(socket_name).expect("client connect");
        let mut queue = display.create_event_queue();
        let attached = (*display).clone().attach(queue.token());
        let globals = GlobalManager::new(&attached);
        pump_both(&mut server, &mut state, &display, &mut queue, 30);

        let compositor = globals
            .instantiate_exact::<ClientCompositor>(4)
            .expect("wl_compositor");
        let seat = globals.instantiate_exact::<ClientSeat>(5).expect("wl_seat");
        let events: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let kb_events = events.clone();
        seat.get_keyboard().quick_assign(move |_, event, _| match event {
            ClientKeyEvent::Enter { .. } => kb_events.borrow_mut().push("kb-enter".into()),
            ClientKeyEvent::Key { key, .. } => {
                kb_events.borrow_mut().push(format!("key {}", key))
            }
            _ => {}
        });
        let pt_events = events.clone();
        seat.get_pointer().quick_assign(move |_, event, _| match event {
            ClientPointerEvent::Enter { .. } => pt_events.borrow_mut().push("pt-enter".into()),
            ClientPointerEvent::Button { button, .. } => {
                pt_events.borrow_mut().push(format!("button {}", button))
            }
            _ => {}
        });
        pump_both(&mut server, &mut state, &display, &mut queue, 20);

        let wm_base = globals
            .instantiate_exact::<xdg_wm_base::XdgWmBase>(1)
            .expect("xdg_wm_base");
        wm_base.quick_assign(|wm_base, event, _| match event {
            xdg_wm_base::Event::Ping { serial } => wm_base.pong(serial),
            _ => {}
        });
        let surface = compositor.create_surface();
        let xdg_surface = wm_base.get_xdg_surface(&surface);
        xdg_surface.quick_assign(|xdg_surface, event, mut data| match event {
            xdg_surface::Event::Configure { serial } => {
                xdg_surface.ack_configure(serial);
                if let Some(done) = data.get::<bool>() {
                    *done = true;
                }
            }
            _ => {}
        });
        let _toplevel = xdg_surface.get_toplevel();
        surface.commit();
        pump_both(&mut server, &mut state, &display, &mut queue, 40);
        assert_eq!(state.windows.len(), 1, "client toplevel becomes a window");

        let id = *state.windows.keys().next().unwrap();
        let surf = state.windows[&id].surface.clone().expect("surface");
        server.focus_keyboard(None);
        server.focus_keyboard(Some(&surf));
        server.input_key(16, true);
        server.input_key(16, false);
        let local = (10, 10);
        server.input_motion((0.0, 0.0), Some((surf, local)));
        server.input_button(0x110, true);
        server.input_button(0x110, false);
        pump_both(&mut server, &mut state, &display, &mut queue, 40);

        let got = events.borrow().join("\n");
        assert!(got.contains("kb-enter"), "keyboard enter:\n{}", got);
        assert!(got.contains("key 16"), "key event:\n{}", got);
        assert!(got.contains("pt-enter"), "pointer enter:\n{}", got);
        assert!(got.contains("button 272"), "button event:\n{}", got);
    }
}
