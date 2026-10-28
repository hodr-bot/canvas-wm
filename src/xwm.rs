//! X11 window manager over XWayland (feature `compositor` only).
//!
//! Smithay 0.3 only spawns the XWayland process; window management is ours.
//! Xwm owns the privileged X11 connection (x11rb), acting as WM
//! (SubstructureRedirect on the root; MapRequest/Configure/Destroy), and
//! resolves each X window to its Wayland surface through the WL_SURFACE_ID
//! client message Xwayland delivers to the root. Pixels then flow zero-copy:
//! Xwayland commits its own buffers, the backend renders them like any
//! client surface. XTEST routing for key/button/motion when an X-backed
//! window is focused.
//!
//! Position on the canvas comes from our own cascade, never from X geometry.

use std::collections::{HashMap, VecDeque};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux,
    EventMask, ImageOrder, InputFocus, MapState, StackMode, WindowClass, BUTTON_PRESS_EVENT,
    BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT,
};
use x11rb::protocol::composite::{self, Redirect};
use x11rb::protocol::xtest;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::COPY_FROM_PARENT;

/// Tracked X window: geometry in X root space (for XTEST motion mapping).
/// `surface_id` is Xwayland's Wayland surface id for this window, from the
/// WL_SURFACE_ID client message; None until the message arrives.
struct XWin {
    x: i16,
    y: i16,
    w: u16,
    h: u16,
    mapped: bool,
    surface_id: Option<u32>,
}

/// Short name for an X11 event (temporary E2E probe).
fn event_name(e: &Event) -> &'static str {
    match e {
        Event::MapRequest(_) => "MapRequest",
        Event::MapNotify(_) => "MapNotify",
        Event::UnmapNotify(_) => "UnmapNotify",
        Event::ConfigureRequest(_) => "ConfigureRequest",
        Event::ConfigureNotify(_) => "ConfigureNotify",
        Event::ClientMessage(_) => "ClientMessage",
        Event::CreateNotify(_) => "CreateNotify",
        Event::DestroyNotify(_) => "DestroyNotify",
        Event::ReparentNotify(_) => "ReparentNotify",
        Event::PropertyNotify(_) => "PropertyNotify",
        Event::DamageNotify(_) => "DamageNotify",
        Event::Error(_) => "Error",
        _ => "other",
    }
}

/// X11 window manager: SubstructureRedirect WM + WL_SURFACE_ID resolution.
pub struct Xwm {
    conn: RustConnection,
    root: u32,
    wins: HashMap<u32, XWin>,
    surface_id_atom: u32,
    /// Fresh (xid, wayland surface id) pairs for the backend to link.
    pub surface_links: VecDeque<(u32, u32)>,
    /// Destroyed windows as (xid, surface id or None) for canvas cleanup.
    pub dead: Vec<(u32, Option<u32>)>,
}

impl Xwm {
    /// The X11 stream arrives via XWayland Ready; nothing Wayland-side is
    /// needed here anymore (Xwayland owns its surfaces).
    pub fn new(x11: std::os::unix::net::UnixStream) -> Result<Self, String> {
        x11.set_nonblocking(true)
            .map_err(|e| format!("x11 nonblock: {}", e))?;
        let stream = x11rb::rust_connection::DefaultStream::from_unix_stream(x11)
            .map_err(|e| format!("x11 stream: {}", e))?
            .0;
        let conn =
            RustConnection::connect_to_stream(stream, 0).map_err(|e| format!("x11 connect: {}", e))?;
        let (root, lsb_first) = {
            let setup = conn.setup();
            let screen = setup.roots.first().ok_or("x11: no screen 0")?;
            (screen.root, setup.image_byte_order == ImageOrder::LSB_FIRST)
        };
        if !lsb_first {
            return Err("x11: MSBFirst image order unsupported".into());
        }
        // Become the window manager.
        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new()
                .event_mask(EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY),
        )
        .map_err(|e| format!("x11 wm select: {}", e))?;
        // Composite-redirect the root's children: Xwayland only wraps a
        // toplevel (wl_surface + WL_SURFACE_ID) once it is redirected.
        match composite::redirect_subwindows(&conn, root, Redirect::MANUAL) {
            Ok(_) => log::info!("X composite redirect manual on root"),
            Err(e) => log::warn!("X composite redirect failed: {}", e),
        }
        // Claim WM_S0: Xwayland only adopts its -listenfd X listeners after
        // the window manager owns the WM_S<n> selection (xwayland.c
        // wm_selection_callback -> listen_on_fds). Without this it stays deaf
        // to X clients even though it signalled ready.
        let wm_s0 = conn
            .intern_atom(false, b"WM_S0")
            .map_err(|e| format!("x11 wm atom: {}", e))?
            .reply()
            .map_err(|e| format!("x11 wm atom reply: {}", e))?
            .atom;
        let holder = conn
            .generate_id()
            .map_err(|e| format!("x11 holder id: {}", e))?;
        conn.create_window(
            0,
            holder,
            root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            COPY_FROM_PARENT,
            &CreateWindowAux {
                override_redirect: Some(1),
                ..Default::default()
            },
        )
        .map_err(|e| format!("x11 holder window: {}", e))?;
        conn.set_selection_owner(holder, wm_s0, 0u32)
            .map_err(|e| format!("x11 wm own: {}", e))?;
        conn.flush().map_err(|e| format!("x11 wm flush: {}", e))?;
        log::info!("claimed WM_S0 on holder window {}", holder);
        let surface_id_atom = conn
            .intern_atom(false, b"WL_SURFACE_ID")
            .map_err(|e| format!("x11 surface atom: {}", e))?
            .reply()
            .map_err(|e| format!("x11 surface atom reply: {}", e))?
            .atom;
        let mut xwm = Self {
            conn,
            root,
            wins: HashMap::new(),
            surface_id_atom,
            surface_links: VecDeque::new(),
            dead: Vec::new(),
        };
        xwm.intern_children()?;
        Ok(xwm)
    }

    /// Manage already-mapped top-level windows (clients started early).
    fn intern_children(&mut self) -> Result<(), String> {
        let tree = self
            .conn
            .query_tree(self.root)
            .map_err(|e| format!("x11 query_tree: {}", e))?
            .reply()
            .map_err(|e| format!("x11 tree reply: {}", e))?;
        for child in tree.children {
            if child == self.root {
                continue;
            }
            let attrs = self
                .conn
                .get_window_attributes(child)
                .map_err(|e| format!("x11 attrs: {}", e))?
                .reply()
                .map_err(|e| format!("x11 attrs reply: {}", e))?;
            let geom = self
                .conn
                .get_geometry(child)
                .map_err(|e| format!("x11 geom: {}", e))?
                .reply()
                .map_err(|e| format!("x11 geom reply: {}", e))?;
            if attrs.map_state == MapState::VIEWABLE
                && !attrs.override_redirect
                && geom.width > 0
                && geom.height > 0
            {
                self.manage(child, geom.x, geom.y, geom.width, geom.height);
            }
        }
        Ok(())
    }

    fn manage(&mut self, xid: u32, x: i16, y: i16, w: u16, h: u16) {
        if self.wins.contains_key(&xid) {
            return;
        }
        let _ = self.conn.map_window(xid);
        let _ = self.conn.flush();
        self.wins.insert(
            xid,
            XWin {
                x,
                y,
                w,
                h,
                mapped: true,
                surface_id: None,
            },
        );
        log::info!("X window {} managed ({}x{} at {},{})", xid, w, h, x, y);
    }

    /// Kill an X client (close button). The compositor drops its canvas
    /// window; XWayland unmaps the surface on DestroyNotify.
    pub fn kill(&self, xid: u32) {
        if let Err(e) = self.conn.kill_client(xid) {
            log::warn!("kill client {} failed: {}", xid, e);
        }
        let _ = self.conn.flush();
    }

    /// Resize an X window (fullscreen on/off): the client gets a real
    /// ConfigureNotify and recommits at the new size, so the buffer
    /// follows instead of stretching.
    pub fn configure(&self, xid: u32, w: u32, h: u32) {
        let aux = ConfigureWindowAux::new().width(w).height(h);
        if let Err(e) = self.conn.configure_window(xid, &aux) {
            log::warn!("configure {} to {}x{} failed: {}", xid, w, h, e);
        }
        let _ = self.conn.flush();
    }

    /// Drain X11 events. Call every frame.
    pub fn poll(&mut self) {
        loop {
            let event = match self.conn.poll_for_event() {
                Ok(Some(e)) => {
                    log::info!("X event: {}", event_name(&e));
                    e
                }
                Ok(None) => break,
                Err(e) => {
                    log::warn!("X poll: {}", e);
                    break;
                }
            };
            match event {
                Event::ClientMessage(e) => {
                    log::info!(
                        "X clientmessage window={} type={} data32={:?}",
                        e.window,
                        e.type_,
                        e.data.as_data32()
                    );
                    // WL_SURFACE_ID from Xwayland: the Wayland surface id
                    // for this X window. Queue it for the backend to link.
                    if e.type_ == self.surface_id_atom {
                        let sid = e.data.as_data32()[0];
                        if let Some(win) = self.wins.get_mut(&e.window) {
                            win.surface_id = Some(sid);
                        }
                        self.surface_links.push_back((e.window, sid));
                        log::info!("X window {} -> wayland surface {}", e.window, sid);
                    }
                }
                Event::MapRequest(e) => {
                    let geom = self
                        .conn
                        .get_geometry(e.window)
                        .ok()
                        .and_then(|c| c.reply().ok());
                    let attrs = self
                        .conn
                        .get_window_attributes(e.window)
                        .ok()
                        .and_then(|c| c.reply().ok());
                    if let (Some(g), Some(a)) = (geom, attrs) {
                        if !a.override_redirect && g.width > 0 && g.height > 0 {
                            self.manage(e.window, g.x, g.y, g.width, g.height);
                        }
                    }
                }
                Event::ConfigureRequest(e) => {
                    // Apply position + size; sibling/stack order unmanaged (V1).
                    let aux = ConfigureWindowAux::new()
                        .x(e.x as i32)
                        .y(e.y as i32)
                        .width(e.width as u32)
                        .height(e.height as u32);
                    let _ = self.conn.configure_window(e.window, &aux);
                    if let Some(win) = self.wins.get_mut(&e.window) {
                        win.x = e.x;
                        win.y = e.y;
                        win.w = e.width;
                        win.h = e.height;
                    }
                    let _ = self.conn.flush();
                }
                Event::UnmapNotify(e) => {
                    if let Some(win) = self.wins.get_mut(&e.window) {
                        win.mapped = false;
                    }
                }
                Event::DestroyNotify(e) => {
                    let sid = self.wins.remove(&e.window).and_then(|w| w.surface_id);
                    self.dead.push((e.window, sid));
                }
                _ => {}
            }
        }
        let _ = self.conn.flush();
    }

    /// Give X input focus to a managed window.
    pub fn focus(&self, xid: u32) {
        if !self.wins.contains_key(&xid) {
            return;
        }
        let _ = self
            .conn
            .set_input_focus(InputFocus::POINTER_ROOT, xid, x11rb::CURRENT_TIME);
        let _ = self.conn.flush();
    }

    /// X keycode from evdev (X adds 8). None when unmappable.
    fn xkey(evdev: u32) -> Option<u8> {
        evdev.checked_add(8)?.try_into().ok()
    }

    pub fn xtest_key(&self, xid: u32, evdev: u32, pressed: bool) {
        if !self.wins.contains_key(&xid) {
            return;
        }
        let code = match Self::xkey(evdev) {
            Some(c) => c,
            None => return,
        };
        let ty = if pressed {
            KEY_PRESS_EVENT
        } else {
            KEY_RELEASE_EVENT
        };
        let _ = xtest::fake_input(
            &self.conn,
            ty,
            code,
            x11rb::CURRENT_TIME,
            self.root,
            0,
            0,
            0,
        );
        let _ = self.conn.flush();
    }

    /// Mirror a canvas raise into X stacking order (XRaiseWindow). Without
    /// this, X stacking drifts from canvas stacking and XTEST/wl_pointer
    /// button delivery (which targets the X-topmost window at the pointer)
    /// lands on the wrong client wherever X windows overlap in root space
    /// (Xwayland stacks them all at 0,0 by default).
    pub fn restack_above(&self, xid: u32) {
        if !self.wins.contains_key(&xid) {
            return;
        }
        let _ = self.conn.configure_window(
            xid,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        );
        let _ = self.conn.flush();
    }

    /// Left button only in V1 (X button 1).
    pub fn xtest_button(&self, xid: u32, evdev_code: u32, pressed: bool) {
        if !self.wins.contains_key(&xid) {
            return;
        }
        let ty = match (evdev_code, pressed) {
            (0x110, true) => BUTTON_PRESS_EVENT,
            (0x110, false) => BUTTON_RELEASE_EVENT,
            _ => return,
        };
        let _ = xtest::fake_input(
            &self.conn,
            ty,
            1,
            x11rb::CURRENT_TIME,
            self.root,
            0,
            0,
            0,
        );
        let _ = self.conn.flush();
    }

    /// Move the X pointer to window-local coords (root = local + X origin).
    pub fn xtest_motion(&self, xid: u32, local_x: i32, local_y: i32) {
        let win = match self.wins.get(&xid) {
            Some(w) => w,
            None => return,
        };
        let _ = xtest::fake_input(
            &self.conn,
            MOTION_NOTIFY_EVENT,
            0,
            x11rb::CURRENT_TIME,
            self.root,
            (win.x as i32 + local_x) as i16,
            (win.y as i32 + local_y) as i16,
            0,
        );
        let _ = self.conn.flush();
    }
}
