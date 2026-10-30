//! Legacy `wl_drm` device handoff (version 2).
//!
//! Smithay 0.3 only speaks `zwp_linux_dmabuf` v3: formats, but no feedback
//! main-device, and no `wl_drm`. Mesa hardware clients on Wayland need a DRM
//! device fd to allocate their buffers; with neither handoff they end up
//! with fd -1, die in EGL init (`MESA-LOADER: failed to retrieve device
//! information`) and freeze right after mapping — while software-GL clients
//! (shm path, no device needed) look fine. This global closes the gap: on
//! bind it sends the render node plus the formats we import through dmabuf.
//! Render nodes need no DRM authentication, which a nested compositor could
//! not provide anyway; `authenticate` is answered unconditionally and the
//! dead GEM-flink requests are ignored (allocation goes through dmabuf).

use smithay::reexports::wayland_server::{Display, Filter, Global, Main};
use std::os::unix::io::FromRawFd;

// Same shim layout as wayland-protocols' `wayland_protocol!` server module:
// the scanner output expects these names in scope via `super::`.
mod generated {
    #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
    #![allow(non_upper_case_globals, non_snake_case, unused_imports)]
    #![allow(missing_docs, clippy::all)]

    pub mod server {
        //! Server-side API of wl_drm
        pub(crate) use wayland_server::{AnonymousObject, Main, Resource, ResourceMap};
        pub(crate) use wayland_commons::map::{Object, ObjectMetadata};
        pub(crate) use wayland_commons::{Interface, MessageGroup};
        pub(crate) use wayland_commons::wire::{Argument, ArgumentType, Message, MessageDesc};
        pub(crate) use wayland_commons::smallvec;
        pub(crate) use wayland_server::protocol::wl_buffer;
        pub(crate) use wayland_server::sys;
        include!(concat!(env!("OUT_DIR"), "/drm_server_api.rs"));
    }
}

pub use generated::server::wl_drm::{Request as DrmRequest, WlDrm};

/// 32-bit XR24 fourcc: the format every Mesa client speaks.
const FORMAT_XRGB8888: u32 = 0x3432_5258;
/// 32-bit AR24 fourcc.
const FORMAT_ARGB8888: u32 = 0x3432_5241;
/// Capability bit: PRIME buffer sharing works.
const CAP_PRIME: u32 = 1;

/// Announce `device` on `wl_drm` v2. Returns the global; the caller keeps it
/// alive by holding the return value in server state.
pub fn init_drm_global(display: &mut Display, device: String) -> Global<WlDrm> {
    display.create_global(
        2,
        Filter::new(
            move |(drm, version): (Main<WlDrm>, u32), _, _| {
                drm.quick_assign(move |drm, req, _| match req {
                    // Render-node clients never send this; answer anyway so
                    // a card-node client degrades instead of hanging.
                    DrmRequest::Authenticate { .. } => drm.authenticated(),
                    // GEM-flink and prime buffers cannot be imported by this
                    // compositor (allocation goes through linux-dmabuf):
                    // implement the id so the client is not killed for an
                    // unimplemented object, then refuse per protocol so it
                    // can fall back instead of hanging on a dead buffer.
                    DrmRequest::CreateBuffer { id, .. }
                    | DrmRequest::CreatePlanarBuffer { id, .. } => {
                        id.quick_assign(|_, _, _| {});
                        drm.invalid_format();
                    }
                    DrmRequest::CreatePrimeBuffer { id, fd, .. } => {
                        // Wayland passes ownership of the fd: close it.
                        let _ = unsafe {
                            std::os::unix::io::OwnedFd::from_raw_fd(fd)
                        };
                        id.quick_assign(|_, _, _| {});
                        drm.invalid_format();
                    }
                    _ => {}
                });
                drm.device(device.clone());
                drm.format(FORMAT_XRGB8888);
                drm.format(FORMAT_ARGB8888);
                if version >= 2 {
                    drm.capabilities(CAP_PRIME);
                }
            },
        ),
    )
}
