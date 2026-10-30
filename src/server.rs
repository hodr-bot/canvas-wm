//! Wayland frontend skeleton (feature `compositor` only).
//!
//! Display + socket + core globals (compositor, shm, xdg_shell). Every new
//! xdg-toplevel becomes a CanvasWindow with cascade placement on the shared
//! canvas. Client buffer rendering and seat input land next.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use smithay::backend::allocator::Format as DmaFormat;
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
use smithay::wayland::dmabuf::init_dmabuf_global;
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
    /// xdg popup overlays (tooltips, menus): rendered above everything,
    /// routed to the seat first — but NEVER canvas windows, so they can
    /// neither steal picks/focus nor be grabbed/moved/resized.
    pub popups: HashMap<u64, PopupWindow>,
    next_id: u64,
    /// Stacking high-water mark; each raise bumps it.
    z_top: u64,
    /// Every live wl_surface by its numeric id, XWayland's included.
    /// XWayland publishes each X window's surface id through the
    /// WL_SURFACE_ID client message; the backend resolves it here and
    /// renders the real surface zero-copy (no GetImage bridge).
    surface_ids: HashMap<u32, WlSurface>,
    /// Surfaces already announced to wl_outputs (enter sent once).
    entered_outputs: HashSet<u32>,
    /// X window ids whose canvas raise still needs its X restack mirror.
    /// XTEST button delivery goes to the X-topmost window at the pointer,
    /// so every canvas raise queues its xid here and the backend drains
    /// the queue into XRaiseWindow each frame. Hover focus never raises,
    /// so it never queues: hover focuses without restacking.
    restack_queue: Vec<u32>,
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

/// An xdg popup (tooltip, menu): a child overlay of its parent window.
/// Rendered above all windows at parent.pos + offset; the offset is set
/// from the cursor at first map (popups open at the pointer), the size is
/// learned from the first committed buffer. Never pickable as a window.
pub struct PopupWindow {
    pub id: u64,
    pub parent: u64,
    pub surface: Option<WlSurface>,
    pub offset: Option<Vec2>,
    pub size: Vec2,
    pub texture: Option<Gles2Texture>,
    pub last_buffer: Option<WlBuffer>,
}

impl PopupWindow {
    fn new(id: u64, parent: u64, surface: Option<WlSurface>) -> Self {
        Self {
            id,
            parent,
            surface,
            offset: None,
            size: Vec2::new(0.0, 0.0),
            texture: None,
            last_buffer: None,
        }
    }

    /// Canvas rect, once placed and sized.
    pub fn rect(&self, parent_pos: Vec2) -> Option<(Vec2, Vec2)> {
        match (self.offset, self.size) {
            (Some(off), s) if s.x > 0.0 && s.y > 0.0 => {
                Some((Vec2::new(parent_pos.x + off.x, parent_pos.y + off.y), s))
            }
            _ => None,
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
    /// Orphaned popups of the window die with it.
    pub fn close_window(&mut self, id: u64) -> Option<u32> {
        self.popups.retain(|_, p| p.parent != id);
        self.windows.remove(&id).and_then(|w| w.x11)
    }

    /// Register an xdg popup overlay for a parent window. Returns the
    /// popup id (None when the parent is unknown — the popup is dropped).
    pub fn add_popup(&mut self, parent: u64, surface: Option<WlSurface>) -> Option<u64> {
        if !self.windows.contains_key(&parent) {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.popups.insert(id, PopupWindow::new(id, parent, surface));
        Some(id)
    }

    /// Drop a popup overlay (unmapped/destroyed). Never touches windows.
    /// Returns the parent id so the caller can fold keyboard focus back
    /// to it — a seat left pointing at the dead popup surface reads as
    /// a frozen app (no more keys until the next click).
    pub fn remove_popup(&mut self, id: u64) -> Option<u64> {
        self.popups.remove(&id).map(|p| p.parent)
    }

    /// Topmost popup containing a canvas-plane point (newest wins).
    /// The seat consults popups BEFORE windows so menus and tooltips
    /// receive input; canvas picking (`topmost_at`) never sees them.
    pub fn popup_at(&self, p: crate::canvas::Vec2) -> Option<u64> {
        let mut best: Option<u64> = None;
        for (id, pop) in self.popups.iter() {
            let parent_pos = match self.windows.get(&pop.parent) {
                Some(w) => w.canvas.pos,
                None => continue,
            };
            if let Some((org, size)) = pop.rect(parent_pos) {
                if p.x >= org.x && p.x < org.x + size.x && p.y >= org.y && p.y < org.y + size.y
                {
                    if best.map(|b| *id > b).unwrap_or(true) {
                        best = Some(*id);
                    }
                }
            }
        }
        best
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

    /// Canvas ids back-to-front (painter's algorithm): the backend paints
    /// in this order so the visual top always matches the pick top
    /// (`topmost_at`). HashMap iteration is random; painting unsorted
    /// lets an occluded window cover the focused one while clicks
    /// resolve to the focused one.
    pub fn paint_order(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self.windows.keys().copied().collect();
        ids.sort_by_key(|id| self.windows.get(id).map(|c| c.z).unwrap_or(0));
        ids
    }

    /// Bring a window to the front (click-focus, grab).
    pub fn raise(&mut self, id: u64) {
        if self.windows.contains_key(&id) {
            self.z_top += 1;
            if let Some(w) = self.windows.get_mut(&id) {
                w.z = self.z_top;
                // Queue the X mirror: without it, X stacking drifts from
                // canvas stacking and clicks land on the wrong client
                // wherever X windows overlap in root space.
                if let Some(xid) = w.x11 {
                    self.restack_queue.push(xid);
                }
            }
        }
    }

    /// Drain queued X restacks (backend mirrors them into XRaiseWindow).
    pub fn take_restacks(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.restack_queue)
    }

    /// Window owning `surface`, if any (activation focus, layer panels).
    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<u64> {
        let id = surface.as_ref().id();
        self.windows
            .iter()
            .find(|(_, w)| {
                w.surface
                    .as_ref()
                    .map(|s| s.as_ref().id() == id)
                    .unwrap_or(false)
            })
            .map(|(wid, _)| *wid)
    }

    /// Check whether the one-shot output enter was already delivered for
    /// `surface`. The backend retries until `enter_output` reaches a bound
    /// `wl_output`, so a first frame that lands before the client binds
    /// the output does not wedge Qt clients forever.
    pub fn is_enter_sent(&self, surface: &WlSurface) -> bool {
        self.entered_outputs.contains(&surface.as_ref().id())
    }

    /// Mark the one-shot output enter for `surface` as delivered. True on
    /// first call per surface id; call only after `enter_output` reports
    /// at least one target. The backend sends `wl_surface.enter` then.
    pub fn claim_output_enter(&mut self, surface: &WlSurface) -> bool {
        self.entered_outputs.insert(surface.as_ref().id())
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
    /// GPU buffer sharing for GL clients (Kate, Alacritty). Without this
    /// global they cannot present a single frame; keep it alive forever.
    _dmabuf: Global<
        smithay::reexports::wayland_protocols::unstable::linux_dmabuf::v1::server::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
    >,
    /// Device handoff for Mesa hardware clients (`wl_drm` render node).
    /// Without it they allocate against fd -1 and freeze after mapping;
    /// keep it alive forever. None when no render node is openable.
    _drm: Option<Global<crate::drm::WlDrm>>,
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
    /// Decoration manager (client-side preferred, no SSD path).
    _decoration: Global<
        smithay::reexports::wayland_protocols::unstable::xdg_decoration::v1::server::zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
    >,
    /// Activation tokens + pending focus surfaces.
    activation_registry: crate::activation::SharedRegistry,
    _activation: Global<
        smithay::reexports::wayland_protocols::staging::xdg_activation::v1::server::xdg_activation_v1::XdgActivationV1,
    >,
    /// Primary (middle-click) selection registry.
    primary_registry: crate::primary::SharedRegistry,
    _primary: Global<
        smithay::reexports::wayland_protocols::unstable::primary_selection::v1::server::zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1,
    >,
    /// Last pressed pointer button, for drag-grab start data.
    last_press: Option<u32>,
    /// Last pointer location + focus, for drag-grab start data.
    last_pointer: (f64, f64),
    last_focus: Option<(WlSurface, (i32, i32))>,
    /// Last keyboard-focus surface id, to log real focus changes.
    last_kbd: Option<u32>,
    _seat: Seat,
    _seat_global: Global<smithay::reexports::wayland_server::protocol::wl_seat::WlSeat>,
    /// Single wl_output v4 (Smithay's advertises v3, which mako refuses).
    _output_global: Global<WlOutput>,
    /// Bound outputs, for surface enter broadcasts (Qt needs enter).
    output_registry: crate::output::SharedOutputs,
    keyboard: KeyboardHandle,
    pointer: PointerHandle,
    serial: u32,
    start: Instant,
}

/// First openable DRM render node (`/dev/dri/renderD*`), for the `wl_drm`
/// device handoff. Render nodes are world-openable and need no auth.
fn pick_render_node() -> Option<String> {
    let mut nodes: Vec<String> = std::fs::read_dir("/dev/dri")
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().to_string_lossy().into_owned())
        .filter(|path| path.contains("renderD"))
        .collect();
    nodes.sort();
    nodes.into_iter().find(|path| std::fs::File::open(path).is_ok())
}

impl Server {
    /// Formats come from the real renderer (queried before this call so
    /// the dmabuf global advertises exactly what the driver imports).
    /// Pass an empty vec in tests: the global is still announced.
    pub fn new(dmabuf_formats: Vec<DmaFormat>) -> io::Result<(Self, OsString)> {
        let display = Rc::new(RefCell::new(Display::new()));
        // Reserve serial 0: some toolkits (Qt) ignore a first configure
        // carrying serial 0 and wait forever for a real one.
        let _ = smithay::wayland::SERIAL_COUNTER.next_serial();
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
        // GPU buffers (dmabuf): GL toolkits need this to present. Accept
        // any format the renderer listed; the real driver check happens
        // at import time in the backend (clean warning, never a crash).
        let known = dmabuf_formats.clone();
        log::info!("dmabuf global: {} formats", known.len());
        let dmabuf = init_dmabuf_global(
            &mut display.borrow_mut(),
            dmabuf_formats,
            move |dmabuf, _| {
                use smithay::backend::allocator::Buffer as _;
                let code = smithay::backend::allocator::Buffer::format(dmabuf).code;
                let ok = known.iter().any(|f| f.code == code);
                if !ok {
                    log::warn!("dmabuf format {:?} not advertised, refused", code);
                }
                ok
            },
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
        // Decoration negotiation (always client-side) for GTK/Qt.
        let decoration = crate::deco::init_decoration(&mut display.borrow_mut());
        // Token-based activation: clients can request focus.
        let activation_registry = crate::activation::new_registry();
        let activation =
            crate::activation::init_activation(&mut display.borrow_mut(), activation_registry.clone());
        // Primary (middle-click) selection alongside the clipboard.
        let primary_registry = crate::primary::new_registry();
        let primary =
            crate::primary::init_manager(&mut display.borrow_mut(), primary_registry.clone());
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
        let (output_global, output_registry) = output_global;
        // Render-node handoff for Mesa hardware clients: without a device
        // they allocate against fd -1, fail EGL init and freeze after the
        // first map. Render nodes need no DRM auth, which a nested
        // compositor could not provide anyway.
        let drm = match pick_render_node() {
            Some(device) => {
                log::warn!("wl_drm device: {}", device);
                Some(crate::drm::init_drm_global(
                    &mut display.borrow_mut(),
                    device,
                ))
            }
            None => {
                log::warn!("wl_drm device: none openable, hardware GL clients will fail");
                None
            }
        };
        Ok((
            Self {
                display,
                _compositor: compositor,
                _subcompositor: subcompositor,
                _shm: shm,
                _dmabuf: dmabuf,
                _drm: drm,
                _shell_state: shell_state,
                _xdg: xdg,
                _zxdg: zxdg,
                _xdg_v3: xdg_v3,
                _data_manager: data_manager,
                _layer_shell: layer_shell,
                data_registry,
                _decoration: decoration,
                activation_registry,
                _activation: activation,
                primary_registry,
                _primary: primary,
                last_press: None,
                last_pointer: (0.0, 0.0),
                last_focus: None,
                last_kbd: None,
                _seat: seat,
                _seat_global: seat_global,
                _output_global: output_global,
                output_registry,
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
        self.drain_activation(state);
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
        let new = surface.map(|s| s.as_ref().id());
        if self.last_kbd != new {
            log::info!("kbd focus {:?} -> {:?}", self.last_kbd, new);
            self.last_kbd = new;
        }
        self.keyboard.set_focus(surface, serial);
        // Selection offers must follow keyboard focus for copy/paste.
        self.focus_data_device(surface);
    }

    /// Mirror keyboard focus into the registry so selection and DnD offers
    /// reach the right client. X11-backed windows clear it.
    pub fn focus_data_device(&self, surface: Option<&WlSurface>) {
        crate::data::set_focus_client(&self.data_registry, surface);
        crate::primary::set_focus_client(&self.primary_registry, surface);
    }

    /// Focus surfaces requested through `xdg_activation_v1`: raise the
    /// owning window and hand it keyboard focus.
    pub fn drain_activation(&mut self, state: &mut ServerState) {
        for surface in crate::activation::take_pending(&self.activation_registry) {
            if !surface.as_ref().is_alive() {
                continue;
            }
            if let Some(id) = state.window_for_surface(&surface) {
                state.raise(id);
                log::info!("activation: focused window {}", id);
            }
            self.focus_keyboard(Some(&surface));
        }
    }

    /// Broadcast `wl_surface.enter` for a newly seen surface. Toolkits
    /// such as Qt wait for enter before their first frame. Returns the
    /// number of outputs that got the event; zero means retry later.
    pub fn enter_output(&self, surface: &WlSurface) -> usize {
        crate::output::enter_all(&self.output_registry, surface)
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

    /// Reconfigure a natively-fullscreened window on the Smithay v1 path:
    /// real client resize + fullscreen state so the buffer follows instead
    /// of stretching (the "F zooms the app" look). False when the surface
    /// is not a tracked v1 toplevel (v3 clients use xdg_v3::configure).
    pub fn configure_native_stable(
        &self,
        surface: &WlSurface,
        w: i32,
        h: i32,
        fullscreen: bool,
    ) -> bool {
        use smithay::reexports::wayland_protocols::xdg_shell::server::xdg_toplevel::State;
        let shell = self._shell_state.lock().unwrap();
        for tl in shell.toplevel_surfaces() {
            if tl.get_surface() == Some(surface) {
                let _ = tl.with_pending_state(|s| {
                    s.size = Some((w, h).into());
                    if fullscreen {
                        s.states.set(State::Fullscreen);
                    } else {
                        s.states.unset(State::Fullscreen);
                    }
                });
                tl.send_configure();
                log::info!(
                    "xdg v1 configure {}x{} fullscreen={}",
                    w,
                    h,
                    fullscreen
                );
                return true;
            }
        }
        false
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
    fn popups_never_become_windows() {
        use crate::canvas::Vec2;
        let mut s = ServerState::default();
        // Unknown parent: popup dropped, no window created.
        assert_eq!(s.add_popup(999, None), None);
        assert!(s.windows.is_empty());
        let w = s.add_toplevel(None);
        let wpos = s.windows[&w].canvas.pos;
        let p = s.add_popup(w, None).expect("popup");
        // Unplaced/unsized: invisible to the seat, and never a window.
        assert_eq!(s.popup_at(wpos), None);
        assert!(s.topmost_at(wpos) == Some(w));
        // Placed + sized: seat hit, still not a window.
        s.popups.get_mut(&p).unwrap().offset = Some(Vec2::new(10.0, 10.0));
        s.popups.get_mut(&p).unwrap().size = Vec2::new(100.0, 40.0);
        let inside = Vec2::new(wpos.x + 50.0, wpos.y + 20.0);
        assert_eq!(s.popup_at(inside), Some(p));
        assert_eq!(s.topmost_at(inside), Some(w));
        // Dismissed: gone from the seat, and the parent id comes back
        // for the keyboard-focus fallback.
        assert_eq!(s.remove_popup(p), Some(w));
        assert_eq!(s.popup_at(inside), None);
        assert_eq!(s.remove_popup(p), None);
        // Parent closed: orphan popups die with it.
        let p2 = s.add_popup(w, None).expect("popup");
        s.popups.get_mut(&p2).unwrap().offset = Some(Vec2::new(0.0, 0.0));
        s.popups.get_mut(&p2).unwrap().size = Vec2::new(10.0, 10.0);
        s.close_window(w);
        assert!(s.popups.is_empty());
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
    fn raise_queues_x_restack_for_x11_windows() {
        let mut s = ServerState::default();
        let id = s.add_toplevel(None);
        // Map-time raise has no X window yet: nothing queued.
        assert!(s.take_restacks().is_empty());
        s.windows.get_mut(&id).unwrap().x11 = Some(42);
        s.raise(id);
        assert_eq!(s.take_restacks(), vec![42]);
        // Drain is one-shot; unknown ids queue nothing.
        assert!(s.take_restacks().is_empty());
        s.raise(9999);
        assert!(s.take_restacks().is_empty());
    }

    #[test]
    fn paint_order_puts_raised_window_last() {
        let mut s = ServerState::default();
        let a = s.add_toplevel(None);
        let b = s.add_toplevel(None);
        // Newest (b) paints last; after raising a, a paints last, so the
        // visual top always matches the pick top.
        assert_eq!(s.paint_order().last(), Some(&b));
        s.raise(a);
        assert_eq!(s.paint_order().last(), Some(&a));
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
        let (mut server, socket) = Server::new(vec![]).expect("server");
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
        let cfg_events = events.clone();
        let xdg_surface = wm_base.get_xdg_surface(&surface);
        xdg_surface.quick_assign(move |xdg_surface, event, mut data| match event {
            xdg_surface::Event::Configure { serial } => {
                xdg_surface.ack_configure(serial);
                cfg_events.borrow_mut().push("configure".into());
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
        server.input_motion((0.0, 0.0), Some((surf.clone(), local)));
        server.input_button(0x110, true);
        server.input_button(0x110, false);
        pump_both(&mut server, &mut state, &display, &mut queue, 40);

        let got = events.borrow().join("\n");
        assert!(got.contains("kb-enter"), "keyboard enter:\n{}", got);
        assert!(got.contains("key 16"), "key event:\n{}", got);
        assert!(got.contains("pt-enter"), "pointer enter:\n{}", got);
        assert!(got.contains("button 272"), "button event:\n{}", got);
        // Fullscreen configure reaches the native client (real resize,
        // not a stretch): the toplevel is tracked and a new configure
        // lands after the call.
        let cfgs = || {
            events
                .borrow()
                .iter()
                .filter(|e| e.as_str() == "configure")
                .count()
        };
        let before = cfgs();
        assert!(
            server.configure_native_stable(&surf, 800, 600, true),
            "v1 toplevel tracked"
        );
        pump_both(&mut server, &mut state, &display, &mut queue, 40);
        assert!(
            cfgs() > before,
            "client received the fullscreen configure"
        );
    }
}
