//! `zwp_primary_selection_device_manager_v1`: middle-click selection.
//!
//! Same shape as the clipboard side of `data.rs`, without drag-and-drop:
//! one source per selection, offers to the focused client plus late
//! binders, byte relay on `receive`. GTK and Qt pick this up for primary
//! (select-to-copy, middle-click-to-paste) with no client changes.

use std::{cell::RefCell, ops::Deref as _, rc::Rc};

use smithay::reexports::wayland_server::{
    protocol::wl_surface::WlSurface,
    Client, Display, Filter, Global, Main,
};
use smithay::reexports::wayland_protocols::unstable::primary_selection::v1::server::{
    zwp_primary_selection_device_manager_v1, zwp_primary_selection_device_v1,
    zwp_primary_selection_offer_v1, zwp_primary_selection_source_v1,
};
use zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1;
use zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1;

/// A bound primary device, paired with its owning client.
struct DeviceEntry {
    client: Client,
    device: ZwpPrimarySelectionDeviceV1,
}

/// Shared registry: devices, current source, focused client.
#[derive(Default)]
pub struct Registry {
    devices: Vec<DeviceEntry>,
    selection: Option<ZwpPrimarySelectionSourceV1>,
    focus_client: Option<Client>,
}

pub type SharedRegistry = Rc<RefCell<Registry>>;

pub fn new_registry() -> SharedRegistry {
    Rc::new(RefCell::new(Registry::default()))
}

/// Advertise the primary selection manager (version 1).
pub fn init_manager(
    display: &mut Display,
    registry: SharedRegistry,
) -> Global<zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1> {
    display.create_global(
        1,
        Filter::new(
            move |(manager, _version): (
                Main<zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1>,
                u32,
            ),
                  _,
                  _| {
                let registry = registry.clone();
                manager.quick_assign(move |_manager, request, _dispatch| {
                    use zwp_primary_selection_device_manager_v1::Request;
                    match request {
                        Request::CreateSource { id } => {
                            implement_source(id);
                        }
                        Request::GetDevice { id, .. } => {
                            implement_device(id, registry.clone());
                        }
                        Request::Destroy => {}
                        _ => {}
                    }
                });
            },
        ),
    )
}

#[derive(Default)]
struct SourceData {
    mime_types: Vec<String>,
    /// Retained offers: dropping a Main destroys it client-side before
    /// the receiver sends `receive`.
    offers: Vec<
        smithay::reexports::wayland_server::Main<
            zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1,
        >,
    >,
}

fn implement_source(id: Main<ZwpPrimarySelectionSourceV1>) {
    id.quick_assign(|source, request, _| {
        if let zwp_primary_selection_source_v1::Request::Offer { mime_type } = request {
            if let Some(data) = source.as_ref().user_data().get::<RefCell<SourceData>>() {
                data.borrow_mut().mime_types.push(mime_type);
            }
        }
    });
    id.as_ref()
        .user_data()
        .set(|| RefCell::new(SourceData::default()));
}

fn implement_device(id: Main<ZwpPrimarySelectionDeviceV1>, registry: SharedRegistry) {
    let client = id.as_ref().client();
    let client_in = client.clone();
    let registry_in = registry.clone();
    id.quick_assign(move |device, request, _| {
        let mut reg = registry_in.borrow_mut();
        let client = client_in.clone();
        match request {
            zwp_primary_selection_device_v1::Request::SetSelection { source, .. } => {
                // Only the focused client may set the selection.
                let allowed = match (&reg.focus_client, &client) {
                    (Some(f), Some(o)) => f.equals(o),
                    _ => false,
                };
                if !allowed {
                    log::info!("primary: selection denied");
                    return;
                }
                log::info!("primary: selection accepted");
                reg.selection = source.clone();
            }
            zwp_primary_selection_device_v1::Request::Destroy => {
                reg.devices.retain(|e| {
                    e.device.as_ref().is_alive() && !e.device.as_ref().equals(device.as_ref())
                });
            }
            _ => {}
        }
    });
    if let Some(client) = client {
        // Late binder: hand over a live selection unconditionally, same
        // reasoning as the clipboard path in `data.rs`.
        let selection = registry.borrow().selection.clone();
        if let Some(source) = selection {
            if source.as_ref().is_alive() {
                offer_selection(&source, &client, &id.deref().clone());
            }
        }
        registry.borrow_mut().devices.push(DeviceEntry {
            client,
            device: id.deref().clone(),
        });
    }
}

/// Offer `source` to one primary device object.
fn offer_selection(
    source: &ZwpPrimarySelectionSourceV1,
    client: &Client,
    device: &ZwpPrimarySelectionDeviceV1,
) {
    use zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1;
    let mime_types = source
        .as_ref()
        .user_data()
        .get::<RefCell<SourceData>>()
        .map(|m| m.borrow().mime_types.clone())
        .unwrap_or_default();
    let offer: Main<ZwpPrimarySelectionOfferV1> = client
        .create_resource::<ZwpPrimarySelectionOfferV1>(device.as_ref().version())
        .unwrap();
    let offer_res = offer.deref().clone();
    let source_clone = source.clone();
    offer.quick_assign(move |_offer, req, _| {
        if let zwp_primary_selection_offer_v1::Request::Receive { mime_type, fd } = req {
            log::info!("primary: offer receive mime={}", mime_type);
            source_clone.send(mime_type, fd);
            // fd stays open: wayland-server passes it on after return.
        }
    });
    device.data_offer(&offer_res);
    for mime in &mime_types {
        offer_res.offer(mime.clone());
    }
    device.selection(Some(&offer_res));
    // Retain the offer Main in the source: dropping it would destroy the
    // offer object client-side before the receiver sends `receive`.
    if let Some(data) = source
        .as_ref()
        .user_data()
        .get::<RefCell<SourceData>>()
    {
        data.borrow_mut().offers.push(offer);
    }
}

/// Send the current selection to every device of `client`.
fn advertise_selection(reg: &Registry, source: &ZwpPrimarySelectionSourceV1, client: &Client) {
    let devices: Vec<ZwpPrimarySelectionDeviceV1> = reg
        .devices
        .iter()
        .filter(|e| e.client.equals(client))
        .map(|e| e.device.clone())
        .collect();
    for device in &devices {
        offer_selection(source, client, device);
    }
}

/// Mirror keyboard focus into the registry. Re-advertises a live
/// selection to the newly focused client.
pub fn set_focus_client(registry: &SharedRegistry, surface: Option<&WlSurface>) {
    let client = surface.and_then(|s| s.as_ref().client());
    let mut reg = registry.borrow_mut();
    reg.focus_client = client.clone();
    if let (Some(client), Some(source)) = (client, reg.selection.clone()) {
        if source.as_ref().is_alive() {
            advertise_selection(&reg, &source, &client);
        }
    }
}
