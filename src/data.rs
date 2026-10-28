//! Our own `wl_data_device_manager` (version 3).
//!
//! Smithay 0.3 hides its drag grab (`DnDGrab`) and its device registry
//! (`SeatData`) from downstream crates, so client-initiated drag-and-drop
//! cannot be wired through its public API. This module implements the
//! manager directly: clipboard selection with byte relay, plus a canvas
//! pointer grab that delivers enter/motion/leave/drop to drop targets.
//! Clients only ever see standard protocol, so foot/GTK/Qt behave.

use std::{cell::RefCell, ops::Deref as _, rc::Rc};

use smithay::{
    reexports::wayland_server::{
        protocol::{
            wl_data_device::{self, WlDataDevice},
            wl_data_device_manager::{self, DndAction},
            wl_data_offer::{self, WlDataOffer},
            wl_data_source::{self, WlDataSource},
            wl_pointer,
            wl_surface::WlSurface,
        },
        Client, Display, Filter, Global, Main,
    },
    utils::{Logical, Point},
    wayland::{
        data_device::default_action_chooser,
        seat::{AxisFrame, GrabStartData, PointerGrab, PointerInnerHandle, Seat},
        Serial,
    },
};

/// A bound data-device object, paired with its owning client.
struct DeviceEntry {
    client: Client,
    device: WlDataDevice,
}

/// Source advertisement (mime types + offered DnD actions).
#[derive(Clone)]
struct SourceMeta {
    mime_types: Vec<String>,
    dnd_actions: DndAction,
    /// Retained offer objects: dropping a Main destroys it client-side.
    offers: Vec<Main<WlDataOffer>>,
}

impl Default for SourceMeta {
    fn default() -> Self {
        Self {
            mime_types: Vec::new(),
            dnd_actions: DndAction::empty(),
            offers: Vec::new(),
        }
    }
}

/// In-flight client drag, shared between the main loop and the grab.
struct Drag {
    source: Option<WlDataSource>,
    origin: WlSurface,
    icon: Option<WlSurface>,
    focus: Option<WlSurface>,
    offer: Option<WlDataOffer>,
    offer_data: Option<Rc<RefCell<OfferData>>>,
    finished: bool,
}

/// Seed posted by `start_drag`, consumed by the main loop to install the grab.
pub struct DragSeed {
    pub source: Option<WlDataSource>,
    pub origin: WlSurface,
    pub icon: Option<WlSurface>,
}

/// Shared registry: devices, selection, focused client, pending/in-flight drag.
#[derive(Default)]
pub struct Registry {
    devices: Vec<DeviceEntry>,
    selection: Option<WlDataSource>,
    focus_client: Option<Client>,
    pub pending_drag: Option<DragSeed>,
    drag: Option<Drag>,
}

pub type SharedRegistry = Rc<RefCell<Registry>>;

pub fn new_registry() -> SharedRegistry {
    Rc::new(RefCell::new(Registry::default()))
}

/// Advertise `wl_data_device_manager` version 3.
pub fn init_manager(
    display: &mut Display,
    registry: SharedRegistry,
) -> Global<wl_data_device_manager::WlDataDeviceManager> {
    display.create_global(
        3,
        Filter::new(
            move |(manager, _version): (
                Main<wl_data_device_manager::WlDataDeviceManager>,
                u32,
            ),
                  _,
                  _| {
                let registry = registry.clone();
            manager.quick_assign(move |_manager, request, _dispatch| {
                use wl_data_device_manager::Request;
                match request {
                    Request::CreateDataSource { id } => {
                        implement_source(id);
                    }
                    Request::GetDataDevice { id, .. } => {
                        implement_device(id, registry.clone());
                    }
                    _ => {}
                }
            });
            },
        ),
    )
}

fn implement_source(id: Main<WlDataSource>) {
    id.quick_assign(|source, request, _data| match request {
        wl_data_source::Request::Offer { mime_type } => {
            if let Some(meta) = source.as_ref().user_data().get::<RefCell<SourceMeta>>() {
                meta.borrow_mut().mime_types.push(mime_type);
            }
        }
        wl_data_source::Request::SetActions { dnd_actions } => {
            if let Some(meta) = source.as_ref().user_data().get::<RefCell<SourceMeta>>() {
                meta.borrow_mut().dnd_actions = dnd_actions;
            }
        }
        wl_data_source::Request::Destroy => {}
        _ => {}
    });
    id.as_ref()
        .user_data()
        .set(|| RefCell::new(SourceMeta::default()));
}

fn implement_device(id: Main<WlDataDevice>, registry: SharedRegistry) {
    let client = id.as_ref().client();
    let client_in = client.clone();
    let registry_in = registry.clone();
    id.quick_assign(move |device, request, _data| {
        let mut reg = registry_in.borrow_mut();
        let client = client_in.clone();
        match request {
            wl_data_device::Request::StartDrag {
                source,
                origin,
                icon,
                ..
            } => {
                // Lenient serial check: any press serial is accepted for now.
                reg.pending_drag = Some(DragSeed {
                    source,
                    origin,
                    icon,
                });
                log::info!("data-device: client drag requested");
            }
            wl_data_device::Request::SetSelection { source, .. } => {
                // Only the focused client may set the selection.
                let allowed = match (&reg.focus_client, &client) {
                    (Some(f), Some(o)) => f.equals(o),
                    _ => false,
                };
                if !allowed {
                    log::info!(
                        "data-device: selection denied (focused={}, setter={})",
                        reg.focus_client.is_some(),
                        client.is_some()
                    );
                    return;
                }
                log::info!("data-device: selection accepted");
                reg.selection = source.clone();
                // Do NOT broadcast back to the setter's own devices:
                // helpers like wl-copy treat an unexpected selection
                // event as "selection lost" and exit, killing the source.
                // Waiters get the selection via late-bind or focus
                // re-advertise instead.
            }
            wl_data_device::Request::Release => {
                reg.devices.retain(|e| {
                    e.device.as_ref().is_alive() && !e.device.as_ref().equals(device.as_ref())
                });
            }
            _ => {}
        }
    });
    if let Some(client) = client {
        // Late binder: hand over a live selection unconditionally.
        // Copy/paste helpers (wl-paste) connect after the selection was
        // set and own no surface, so focus-gating receivers would starve
        // them; the gate stays on the setter side (SetSelection).
        // NOTE: the new device is NOT in the registry yet, so offer
        // directly on it instead of iterating the registry.
        let selection = registry.borrow().selection.clone();
        log::info!(
            "data-device: new device (selection={})",
            selection.is_some()
        );
        if let Some(source) = selection {
            log::info!(
                "data-device: late offer (source alive={})",
                source.as_ref().is_alive()
            );
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

/// Offer `source` to one data-device object.
fn offer_selection(source: &WlDataSource, client: &Client, device: &WlDataDevice) {
    let mime_types = source
        .as_ref()
        .user_data()
        .get::<RefCell<SourceMeta>>()
        .map(|m| m.borrow().mime_types.clone())
        .unwrap_or_default();
    let offer: Main<WlDataOffer> = client
        .create_resource::<WlDataOffer>(device.as_ref().version())
        .unwrap();
    let offer_res = offer.deref().clone();
    let source_clone = source.clone();
    offer.quick_assign(move |_offer, req, _| {
        if let wl_data_offer::Request::Receive { mime_type, fd } = req {
            log::info!("data-device: offer receive mime={}", mime_type);
            source_clone.send(mime_type, fd);
            // Do NOT close fd: wayland-server flushes the sendmsg
            // asynchronously, after this handler returns. Closing here
            // would invalidate the fd before it is passed on (one leaked
            // fd per paste, acceptable for a clipboard path).
        }
    });
    device.data_offer(&offer_res);
    for mime in &mime_types {
        offer_res.offer(mime.clone());
    }
    device.selection(Some(&offer_res));
    // Retain the offer Main in the source: dropping it would destroy the
    // offer object client-side before the receiver sends `receive`.
    if let Some(meta) = source
        .as_ref()
        .user_data()
        .get::<RefCell<SourceMeta>>()
    {
        meta.borrow_mut().offers.push(offer);
    }
}

/// Send the current selection to every device of `client`.
fn advertise_selection(reg: &Registry, source: &WlDataSource, client: &Client) {
    let devices: Vec<WlDataDevice> = reg
        .devices
        .iter()
        .filter(|e| e.client.equals(client))
        .map(|e| e.device.clone())
        .collect();
    for device in &devices {
        offer_selection(source, client, device);
    }
}
/// Mirror keyboard focus into the registry so selection and DnD offers
/// reach the right client. X11-backed windows clear it.
/// Re-advertises a live selection to the newly focused client, so binders
/// that arrived before the selection (or never bound a surface) still get it.
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

/// Install a pending client drag as a pointer grab. Called from the main
/// loop after pump; needs the seat for the pointer handle.
pub fn install_pending_drag(
    registry: &SharedRegistry,
    seat: &Seat,
    serial: Serial,
    button: u32,
    location: Point<f64, Logical>,
    focus: Option<(WlSurface, Point<i32, Logical>)>,
) {
    let seed = registry.borrow_mut().pending_drag.take();
    let Some(seed) = seed else {
        return;
    };
    let start_data = GrabStartData {
        focus: focus.clone(),
        button,
        location,
    };
    let origin = seed.origin.clone();
    registry.borrow_mut().drag = Some(Drag {
        source: seed.source,
        origin,
        icon: seed.icon,
        focus: None,
        offer: None,
        offer_data: None,
        finished: false,
    });
    if let Some(pointer) = seat.get_pointer() {
        let grab = CanvasDndGrab {
            registry: registry.clone(),
            start_data,
        };
        pointer.set_grab(grab, serial);
        log::info!("data-device: drag grab installed");
    }
}

/// True once the in-flight drag has completed (drop or cancel).
pub fn drag_finished(registry: &SharedRegistry) -> bool {
    registry
        .borrow()
        .drag
        .as_ref()
        .map(|d| d.finished)
        .unwrap_or(false)
}

/// Clear a finished drag from the registry.
pub fn clear_drag(registry: &SharedRegistry) {
    registry.borrow_mut().drag = None;
}

fn close_fd(fd: i32) {
    use std::os::unix::io::FromRawFd as _;
    drop(unsafe { std::fs::File::from_raw_fd(fd) });
}

/// Pointer grab delivering DnD target events to canvas windows.
struct CanvasDndGrab {
    registry: SharedRegistry,
    start_data: GrabStartData,
}

impl PointerGrab for CanvasDndGrab {
    fn motion(
        &mut self,
        _handle: &mut PointerInnerHandle<'_>,
        location: Point<f64, Logical>,
        focus: Option<(WlSurface, Point<i32, Logical>)>,
        serial: Serial,
        time: u32,
    ) {
        let focus_surface = focus.as_ref().map(|(s, _)| s.clone());
        // Has the target changed since last motion?
        let changed = {
            let reg = self.registry.borrow();
            match (reg.drag.as_ref(), &focus_surface) {
                (Some(drag), Some(s)) => drag.focus.as_ref() != Some(s),
                (Some(drag), None) => drag.focus.is_some(),
                (None, _) => return,
            }
        };
        if changed {
            // Leave the old target when the drag carries a payload or the
            // old target belongs to the origin client.
            let old = {
                let mut reg = self.registry.borrow_mut();
                reg.drag.as_mut().and_then(|d| d.focus.take())
            };
            if let Some(old) = old {
                let (has_source, same_origin) = {
                    let reg = self.registry.borrow();
                    let drag = reg.drag.as_ref().expect("drag vanished");
                    (
                        drag.source.is_some(),
                        old.as_ref().same_client_as(&drag.origin.as_ref()),
                    )
                };
                if has_source || same_origin {
                    let reg = self.registry.borrow();
                    for entry in reg
                        .devices
                        .iter()
                        .filter(|e| old.as_ref().same_client_as(&e.device.as_ref()))
                    {
                        entry.device.leave();
                    }
                    let mut reg = self.registry.borrow_mut();
                    let drag = reg.drag.as_mut().expect("drag vanished");
                    drag.offer = None;
                    if let Some(data) = drag.offer_data.take() {
                        data.borrow_mut().active = false;
                    }
                }
            }
        }
        let Some((surface, surface_loc)) = focus else {
            return;
        };
        let (x, y): (f64, f64) = (location - surface_loc.to_f64()).into();
        let client = match surface.as_ref().client() {
            Some(c) => c,
            None => return,
        };
        let entering = {
            let reg = self.registry.borrow();
            reg.drag
                .as_ref()
                .map(|d| d.focus.is_none())
                .unwrap_or(false)
        };
        if entering {
            let has_source = {
                let reg = self.registry.borrow();
                reg.drag.as_ref().map(|d| d.source.is_some()).unwrap_or(false)
            };
            if has_source {
                // Offer the payload to every device of the target client.
                let (targets, mime_types, actions, source) = {
                    let reg = self.registry.borrow();
                    let drag = reg.drag.as_ref().expect("drag vanished");
                    let source = drag.source.clone().expect("source vanished");
                    let targets: Vec<WlDataDevice> = reg
                        .devices
                        .iter()
                        .filter(|e| e.client.equals(&client))
                        .map(|e| e.device.clone())
                        .collect();
                    let (mime_types, actions) = source
                        .as_ref()
                        .user_data()
                        .get::<RefCell<SourceMeta>>()
                        .map(|m| {
                            let m = m.borrow();
                            (m.mime_types.clone(), m.dnd_actions)
                        })
                        .unwrap_or((Vec::new(), DndAction::empty()));
                    (targets, mime_types, actions, source)
                };
                let offer_data = Rc::new(RefCell::new(OfferData {
                    active: true,
                    dropped: false,
                    accepted: true,
                    chosen_action: DndAction::empty(),
                }));
                let mut offer_res_opt = None;
                for device in &targets {
                    let action_choice = Rc::new(RefCell::new(
                        |a: DndAction, p: DndAction| default_action_chooser(a, p),
                    ));
                    let offer: Main<WlDataOffer> = client
                        .create_resource::<WlDataOffer>(device.as_ref().version())
                        .unwrap();
                    let offer_res = offer.deref().clone();
                    implement_offer(offer, source.clone(), offer_data.clone(), action_choice);
                    device.data_offer(&offer_res);
                    for mime in &mime_types {
                        offer_res.offer(mime.clone());
                    }
                    offer_res.source_actions(actions);
                    device.enter(serial.into(), &surface, x, y, Some(&offer_res));
                    offer_res_opt = Some(offer_res);
                }
                let mut reg = self.registry.borrow_mut();
                let drag = reg.drag.as_mut().expect("drag vanished");
                drag.offer = offer_res_opt;
                drag.offer_data = Some(offer_data);
                drag.focus = Some(surface);
            } else {
                let same_origin = {
                    let reg = self.registry.borrow();
                    reg.drag.as_ref().map(|d| {
                        surface.as_ref().same_client_as(&d.origin.as_ref())
                    }).unwrap_or(false)
                };
                if same_origin {
                    let reg = self.registry.borrow();
                    for entry in reg
                        .devices
                        .iter()
                        .filter(|e| surface.as_ref().same_client_as(&e.device.as_ref()))
                    {
                        entry.device.enter(serial.into(), &surface, x, y, None);
                    }
                    let mut reg = self.registry.borrow_mut();
                    reg.drag.as_mut().expect("drag vanished").focus = Some(surface);
                }
            }
        } else {
            let deliver = {
                let reg = self.registry.borrow();
                reg.drag.as_ref().map(|d| {
                    d.source.is_some()
                        || surface.as_ref().same_client_as(&d.origin.as_ref())
                }).unwrap_or(false)
            };
            if deliver {
                let reg = self.registry.borrow();
                for entry in reg
                    .devices
                    .iter()
                    .filter(|e| surface.as_ref().same_client_as(&e.device.as_ref()))
                {
                    entry.device.motion(time, x, y);
                }
            }
        }
    }

    fn button(
        &mut self,
        handle: &mut PointerInnerHandle<'_>,
        _button: u32,
        _state: wl_pointer::ButtonState,
        serial: Serial,
        time: u32,
    ) {
        if handle.current_pressed().is_empty() {
            // All buttons released: complete the drop.
            let (validated, has_source) = {
                let reg = self.registry.borrow();
                let drag = match reg.drag.as_ref() {
                    Some(d) => d,
                    None => return,
                };
                let validated = drag
                    .offer_data
                    .as_ref()
                    .map(|d| {
                        let d = d.borrow();
                        d.accepted && !d.chosen_action.is_empty()
                    })
                    .unwrap_or(false);
                (validated, drag.source.is_some())
            };
            let (focus, origin, source) = {
                let reg = self.registry.borrow();
                let drag = match reg.drag.as_ref() {
                    Some(d) => d,
                    None => return,
                };
                (drag.focus.clone(), drag.origin.clone(), drag.source.clone())
            };
            if let Some(ref surface) = focus {
                let same_origin = surface.as_ref().same_client_as(&origin.as_ref());
                if has_source || same_origin {
                    let reg = self.registry.borrow();
                    for entry in reg
                        .devices
                        .iter()
                        .filter(|e| surface.as_ref().same_client_as(&e.device.as_ref()))
                    {
                        if validated {
                            entry.device.drop();
                        } else {
                            entry.device.leave();
                        }
                    }
                }
            }
            {
                let mut reg = self.registry.borrow_mut();
                let drag = reg.drag.as_mut().expect("drag vanished");
                if let Some(ref offer_data) = drag.offer_data {
                    let mut data = offer_data.borrow_mut();
                    if validated {
                        data.dropped = true;
                    } else {
                        data.active = false;
                    }
                }
                if let Some(ref source) = source {
                    source.dnd_drop_performed();
                    if !validated {
                        source.cancelled();
                    }
                }
                drag.finished = true;
                drag.icon = None;
            }
            log::info!("data-device: drop completed (accepted={})", validated);
            handle.unset_grab(serial, time);
        }
    }

    fn axis(&mut self, handle: &mut PointerInnerHandle<'_>, details: AxisFrame) {
        handle.axis(details);
    }

    fn start_data(&self) -> &GrabStartData {
        &self.start_data
    }
}

struct OfferData {
    active: bool,
    dropped: bool,
    accepted: bool,
    chosen_action: DndAction,
}

fn implement_offer(
    offer: Main<WlDataOffer>,
    source: WlDataSource,
    offer_data: Rc<RefCell<OfferData>>,
    action_choice: Rc<RefCell<dyn FnMut(DndAction, DndAction) -> DndAction>>,
) {
    offer.quick_assign(move |offer, req, _| {
        let mut data = offer_data.borrow_mut();
        match req {
            wl_data_offer::Request::Accept { mime_type, .. } => {
                if let Some(mtype) = mime_type {
                    data.accepted = source
                        .as_ref()
                        .user_data()
                        .get::<RefCell<SourceMeta>>()
                        .map(|m| m.borrow().mime_types.contains(&mtype))
                        .unwrap_or(false);
                } else {
                    data.accepted = false;
                }
            }
            wl_data_offer::Request::Receive { mime_type, fd } => {
                let valid = source
                    .as_ref()
                    .user_data()
                    .get::<RefCell<SourceMeta>>()
                    .map(|m| m.borrow().mime_types.contains(&mime_type))
                    .unwrap_or(false)
                    && source.as_ref().is_alive()
                    && data.active;
                if valid {
                    source.send(mime_type, fd);
                }
                close_fd(fd);
            }
            wl_data_offer::Request::Destroy => {}
            wl_data_offer::Request::Finish => {
                if !data.active || !data.accepted || !data.dropped || data.chosen_action.is_empty()
                {
                    offer.as_ref().post_error(
                        wl_data_offer::Error::InvalidFinish as u32,
                        "Cannot finish an invalid data offer.".into(),
                    );
                }
                source.dnd_finished();
                data.active = false;
            }
            wl_data_offer::Request::SetActions {
                dnd_actions,
                preferred_action,
            } => {
                if ![DndAction::Move, DndAction::Copy, DndAction::Ask].contains(&preferred_action)
                {
                    offer.as_ref().post_error(
                        wl_data_offer::Error::InvalidAction as u32,
                        "Invalid preferred action.".into(),
                    );
                }
                let source_actions = source
                    .as_ref()
                    .user_data()
                    .get::<RefCell<SourceMeta>>()
                    .map(|m| m.borrow().dnd_actions)
                    .unwrap_or_else(DndAction::empty);
                let possible = source_actions & dnd_actions;
                data.chosen_action = (&mut *action_choice.borrow_mut())(possible, preferred_action);
                offer.action(data.chosen_action);
                source.action(data.chosen_action);
            }
            _ => {}
        }
    });
}
