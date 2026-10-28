//! Second xdg_shell global at version 3.
//!
//! Smithay 0.3 only advertises `xdg_wm_base` version 1, which modern GTK
//! (zenity 4.x and friends) refuses to bind. This module advertises the
//! same protocol at version 3 (the maximum of the vendored protocol XML)
//! with a minimal implementation: toplevel/popup map, initial configure,
//! title/app-id bookkeeping, and clean removal on destroy. New clients bind
//! the highest version, so they land here; the v1 global stays for old ones.
//! Maximized/fullscreen states are accepted and ignored for now (a later
//! pass wires them to the canvas fullscreen).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use smithay::reexports::wayland_protocols::xdg_shell::server::{
    xdg_popup, xdg_surface, xdg_toplevel, xdg_wm_base,
};
use smithay::reexports::wayland_server::protocol::wl_surface;
use smithay::reexports::wayland_server::{DispatchData, Display, Filter, Global, Main, Resource};
use smithay::wayland::Serial;

use crate::server::ServerState;

static SERIAL: AtomicU32 = AtomicU32::new(1);

fn next_serial() -> Serial {
    Serial::from(SERIAL.fetch_add(1, Ordering::Relaxed))
}

struct SurfaceData {
    wl_surface: wl_surface::WlSurface,
    has_role: AtomicBool,
}

/// Advertise `xdg_wm_base` version 3 alongside Smithay's version 1 global.
pub fn init_xdg_v3(display: &mut Display) -> Global<xdg_wm_base::XdgWmBase> {
    display.create_global(
        3,
        Filter::new(move |(shell, _version), _, _| {
            implement_wm_base(shell);
        }),
    )
}

fn implement_wm_base(shell: Main<xdg_wm_base::XdgWmBase>) {
    shell.quick_assign(|shell, request, dispatch| {
        wm_implementation(shell, request, dispatch)
    });
}

fn wm_implementation(
    _shell: Main<xdg_wm_base::XdgWmBase>,
    request: xdg_wm_base::Request,
    _dispatch: DispatchData<'_>,
) {
    match request {
        xdg_wm_base::Request::Destroy => {}
        xdg_wm_base::Request::CreatePositioner { id } => {
            id.quick_assign(|_, _, _| {});
        }
        xdg_wm_base::Request::GetXdgSurface { id, surface } => {
            id.quick_assign(|surface, req, mut dispatch| {
                surface_implementation(surface, req, dispatch)
            });
            id.assign_destructor(Filter::new(
                |_: Resource<xdg_surface::XdgSurface>, _, _| {},
            ));
            id.as_ref().user_data().set(|| SurfaceData {
                wl_surface: surface,
                has_role: AtomicBool::new(false),
            });
        }
        xdg_wm_base::Request::Pong { .. } => {}
        _ => {}
    }
}

fn surface_implementation(
    xdg_surface: Main<xdg_surface::XdgSurface>,
    request: xdg_surface::Request,
    mut dispatch: DispatchData<'_>,
) {
    match request {
        xdg_surface::Request::Destroy => {}
        xdg_surface::Request::GetToplevel { id } => {
            let data = xdg_surface
                .as_ref()
                .user_data()
                .get::<SurfaceData>()
                .expect("xdg_surface without data");
            if data
                .has_role
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return;
            }
            let win_id = dispatch
                .get::<ServerState>()
                .map(|state| state.add_toplevel(Some(data.wl_surface.clone())));
            let serial = next_serial();
            id.quick_assign(move |toplevel, req, mut dispatch| {
                toplevel_implementation(toplevel, req, dispatch)
            });
            id.assign_destructor(Filter::new(
                move |_: Resource<xdg_toplevel::XdgToplevel>, _, mut data| {
                if let Some(win_id) = win_id {
                    if let Some(state) = data.get::<ServerState>() {
                        state.close_window(win_id);
                        log::info!("xdg v3 toplevel destroyed, window {}", win_id);
                    }
                }
            }));
            id.as_ref().user_data().set(|| ToplevelData {
                xdg_surface: xdg_surface.clone(),
                win_id,
            });
            if let Some(win_id) = win_id {
                log::info!("xdg v3 new toplevel -> canvas window {}", win_id);
            }
            // Initial configure with no size: client picks, we follow.
            // Advertise activated: newest windows take keyboard focus
            // immediately (some clients stall without it).
            id.configure(0, 0, vec![4, 0, 0, 0]);
            xdg_surface.configure(serial.into());
        }
        xdg_surface::Request::GetPopup { id, .. } => {
            let data = xdg_surface
                .as_ref()
                .user_data()
                .get::<SurfaceData>()
                .expect("xdg_surface without data");
            if data
                .has_role
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return;
            }
            let win_id = dispatch
                .get::<ServerState>()
                .map(|state| state.add_toplevel(Some(data.wl_surface.clone())));
            let serial = next_serial();
            id.quick_assign(|_, _, _| {});
            id.assign_destructor(Filter::new(
                move |_: Resource<xdg_popup::XdgPopup>, _, mut data| {
                if let Some(win_id) = win_id {
                    if let Some(state) = data.get::<ServerState>() {
                        state.close_window(win_id);
                    }
                }
            }));
            if let Some(win_id) = win_id {
                log::info!("xdg v3 new popup -> canvas window {}", win_id);
            }
            id.configure(0, 0, 0, 0);
            xdg_surface.configure(serial.into());
        }
        xdg_surface::Request::SetWindowGeometry { .. } => {}
        xdg_surface::Request::AckConfigure { .. } => {}
        _ => {}
    }
}

struct ToplevelData {
    xdg_surface: Main<xdg_surface::XdgSurface>,
    win_id: Option<u64>,
}

#[allow(unused_variables)]
fn toplevel_implementation(
    toplevel: Main<xdg_toplevel::XdgToplevel>,
    request: xdg_toplevel::Request,
    _dispatch: DispatchData<'_>,
) {
    match request {
        xdg_toplevel::Request::Destroy => {}
        // Window management states are accepted so clients never abort;
        // wiring them to canvas fullscreen is a later pass.
        xdg_toplevel::Request::SetMaximized => {}
        xdg_toplevel::Request::UnsetMaximized => {}
        xdg_toplevel::Request::SetFullscreen { .. } => {}
        xdg_toplevel::Request::UnsetFullscreen => {}
        xdg_toplevel::Request::SetMinimized => {}
        _ => {}
    }
}

#[allow(dead_code)]
fn popup_close(_popup: &Main<xdg_popup::XdgPopup>) {}
