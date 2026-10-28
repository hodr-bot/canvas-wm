//! Single `wl_output` global at version 4 (Smithay 0.3 hardcodes v3,
//! which modern clients such as mako refuse: they bind version 4).
//!
//! One viewport, 1280x800@60, scale 1, identity transform. XWayland sizes
//! its screen from this; native clients use it for geometry/DPI.

use smithay::reexports::wayland_server::protocol::wl_output::{
    self, Mode, Subpixel, Transform, WlOutput,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Display, Filter, Global, Main};
use std::{cell::RefCell, rc::Rc};

/// Logical viewport every client sees.
pub const VIEW_W: i32 = 1280;
pub const VIEW_H: i32 = 800;

/// Bound outputs, for surface enter/leave broadcasts.
pub type SharedOutputs = Rc<RefCell<Vec<Main<WlOutput>>>>;

pub fn new_outputs() -> SharedOutputs {
    Rc::new(RefCell::new(Vec::new()))
}

/// Advertise `wl_output` version 4 (1280x800, scale 1).
pub fn init_output(display: &mut Display) -> (Global<WlOutput>, SharedOutputs) {
    let outputs = new_outputs();
    let live = outputs.clone();
    let global = display.create_global(
        4,
        Filter::new(move |(output, _version): (Main<WlOutput>, _), _, _| {
            live.borrow_mut().push(output.clone());
            implement_output(output)
        }),
    );
    (global, outputs)
}

fn implement_output(output: Main<WlOutput>) {
    output.quick_assign(|_, request, _| {
        // Release (v3+) needs no bookkeeping: the runtime destroys the object.
        let _ = request;
    });
    output.as_ref().user_data().set(|| ());
    // Geometry, mode, scale, then name/description (v4) and done.
    // A single current+preferred mode: 1280x800@60Hz.
    output.geometry(
        0,
        0,
        0,
        0,
        Subpixel::Unknown,
        "canvas-wm".into(),
        "canvas".into(),
        Transform::Normal,
    );
    output.mode(
        Mode::Current | Mode::Preferred,
        VIEW_W,
        VIEW_H,
        60000,
    );
    if output.as_ref().version() >= 2 {
        output.scale(1);
    }
    if output.as_ref().version() >= 4 {
        output.name("WL-1".into());
        output.description("canvas-wm canvas".into());
    }
    output.done();
}

/// Tell every bound output that `surface` entered it. Toolkits such as
/// Qt gate their first frame on `wl_surface.enter`; without it they map
/// and stay blank forever.
pub fn enter_all(outputs: &SharedOutputs, surface: &WlSurface) {
    outputs.borrow_mut().retain(|o| o.as_ref().is_alive());
    let owner = surface.as_ref().client();
    for output in outputs.borrow().iter() {
        // enter must reference same-client objects: foreign outputs
        // kill the receiver's connection on use.
        let same = match (&output.as_ref().client(), &owner) {
            (Some(a), Some(b)) => a.equals(b),
            _ => false,
        };
        if same {
            surface.enter(output);
        }
    }
}
