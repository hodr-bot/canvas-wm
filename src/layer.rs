//! Minimal `zwlr_layer_shell_v1` (version 4) for panels and notifications.
//!
//! Only what notification daemons (mako) and simple panels need: layer
//! surface mapping with size/anchor/margins bookkeeping, an initial
//! configure, and clean removal on destroy. Surfaces land as view-locked
//! canvas windows: they follow the buffer size like normal windows but
//! are positioned per-frame from the anchor (they stick to the viewport,
//! never join the canvas layout, hit-testing or keyboard focus).
//! Exclusive zones are accepted and ignored (single panel set for now).

use std::sync::atomic::{AtomicU32, Ordering};

use smithay::reexports::wayland_protocols::wlr::unstable::layer_shell::v1::server::{
    zwlr_layer_shell_v1, zwlr_layer_surface_v1,
};
use smithay::reexports::wayland_server::protocol::wl_surface;
use smithay::reexports::wayland_server::{DispatchData, Display, Filter, Global, Main};
use smithay::wayland::Serial;

use crate::server::ServerState;

static SERIAL: AtomicU32 = AtomicU32::new(1);

fn next_serial() -> Serial {
    Serial::from(SERIAL.fetch_add(1, Ordering::Relaxed))
}

/// Anchor bits from the protocol XML (top=1 bottom=2 left=4 right=8).
pub const ANCHOR_TOP: u32 = 1;
pub const ANCHOR_BOTTOM: u32 = 2;
pub const ANCHOR_LEFT: u32 = 4;
pub const ANCHOR_RIGHT: u32 = 8;

/// View-space lock for a panel: recomputed every frame from the viewport.
#[derive(Clone, Copy, Debug)]
pub struct ViewLock {
    pub anchor: u32,
    pub margins: [i32; 4],
    pub req: (u32, u32),
}

/// Advertise `zwlr_layer_shell_v1` version 4.
pub fn init_layer_shell(display: &mut Display) -> Global<zwlr_layer_shell_v1::ZwlrLayerShellV1> {
    display.create_global(
        4,
        Filter::new(move |(shell, _version), _, _| {
            implement_shell(shell);
        }),
    )
}

fn implement_shell(shell: Main<zwlr_layer_shell_v1::ZwlrLayerShellV1>) {
    shell.quick_assign(|shell, request, dispatch| shell_implementation(shell, request, dispatch));
}

fn shell_implementation(
    _shell: Main<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    request: zwlr_layer_shell_v1::Request,
    _dispatch: DispatchData<'_>,
) {
    match request {
        zwlr_layer_shell_v1::Request::GetLayerSurface {
            id,
            surface,
            layer,
            namespace,
            ..
        } => {
            id.quick_assign(move |panel, req, mut dispatch| {
                panel_implementation(panel, req, dispatch)
            });
            id.assign_destructor(Filter::new(
                |panel: smithay::reexports::wayland_server::Resource<
                    zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
                >,
                 _,
                 mut data| {
                    let win_id = panel
                        .user_data()
                        .get::<PanelData>()
                        .and_then(|panel| panel.win_id.get());
                    if let Some(win_id) = win_id {
                        if let Some(state) = data.get::<ServerState>() {
                            state.close_window(win_id);
                            log::info!("layer panel destroyed, window {}", win_id);
                        }
                    }
                },
            ));
            log::info!(
                "layer shell surface for '{}' layer {}",
                namespace,
                layer.to_raw()
            );
            id.as_ref().user_data().set(|| PanelData {
                wl_surface: surface,
                layer: layer.to_raw(),
                namespace,
                win_id: std::cell::Cell::new(None),
                req: std::cell::Cell::new((0, 0)),
                anchor: std::cell::Cell::new(ANCHOR_TOP | ANCHOR_RIGHT),
                margins: std::cell::Cell::new([0, 0, 0, 0]),
            });
            // Initial configure with no size: client picks, we follow.
            id.configure(next_serial().into(), 0, 0);
        }
        zwlr_layer_shell_v1::Request::Destroy => {}
        _ => {}
    }
}

struct PanelData {
    wl_surface: wl_surface::WlSurface,
    #[allow(dead_code)]
    layer: u32,
    #[allow(dead_code)]
    namespace: String,
    win_id: std::cell::Cell<Option<u64>>,
    req: std::cell::Cell<(u32, u32)>,
    anchor: std::cell::Cell<u32>,
    margins: std::cell::Cell<[i32; 4]>,
}

fn panel_implementation(
    panel: Main<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    request: zwlr_layer_surface_v1::Request,
    mut dispatch: DispatchData<'_>,
) {
    let data = match panel.as_ref().user_data().get::<PanelData>() {
        Some(data) => data,
        None => return,
    };
    match request {
        zwlr_layer_surface_v1::Request::SetSize { width, height } => {
            data.req.set((width, height));
        }
        zwlr_layer_surface_v1::Request::SetAnchor { anchor } => {
            data.anchor.set(anchor.to_raw());
        }
        zwlr_layer_surface_v1::Request::SetExclusiveZone { .. } => {}
        zwlr_layer_surface_v1::Request::SetMargin {
            top,
            right,
            bottom,
            left,
        } => {
            data.margins.set([top, right, bottom, left]);
        }
        zwlr_layer_surface_v1::Request::SetKeyboardInteractivity { .. } => {}
        zwlr_layer_surface_v1::Request::GetPopup { .. } => {}
        zwlr_layer_surface_v1::Request::AckConfigure { .. } => {
            // First ack: map the panel now that the client accepted us.
            if data.win_id.get().is_none() {
                let lock = ViewLock {
                    anchor: data.anchor.get(),
                    margins: data.margins.get(),
                    req: data.req.get(),
                };
                let win_id = dispatch.get::<ServerState>().map(|state| {
                    state.add_layer_panel(Some(data.wl_surface.clone()), lock)
                });
                data.win_id.set(win_id);
                if let Some(win_id) = win_id {
                    log::info!("layer panel mapped -> canvas window {}", win_id);
                }
                // Re-configure with the client size: a (0,0) configure
                // leaves clients like mako waiting for usable space.
                let (w, h) = data.req.get();
                if w != 0 && h != 0 {
                    panel.configure(next_serial().into(), w, h);
                }
            }
        }
        zwlr_layer_surface_v1::Request::Destroy => {}
        _ => {}
    }
}
