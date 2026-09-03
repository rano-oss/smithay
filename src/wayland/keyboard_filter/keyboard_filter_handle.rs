use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use tracing::{error, warn};

use wayland_protocols::wp::{
    input_method::zv3::server::zwp_input_method_v3::ZwpInputMethodV3,
    keyboard_filter::zv1::server::zwp_keyboard_filter_v1::{self, FilterAction, ZwpKeyboardFilterV1},
};
use wayland_server::WEnum;
use wayland_server::{
    Client, DataInit, DisplayHandle, Resource, Weak,
    protocol::{
        wl_keyboard::{KeyState, WlKeyboard},
        wl_surface::WlSurface,
    },
};

use crate::input::{
    Seat, SeatHandler,
    keyboard::{KeyboardHandle, WlKeyboardApi},
};
use crate::wayland::{Dispatch2, seat::keyboard::for_each_focused_kbd_resource};

use super::KeyboardFilterManagerUserDataInner;

#[derive(Debug)]
pub(crate) struct BufferedEvent {
    pub(crate) serial: u32,
    pub(crate) time: u32,
    pub(crate) key: u32,
    pub(crate) state: KeyState,
}

/// The interceptor installed in `KeyboardHandle::kbd_interceptor`.
///
/// It forwards all events to both:
/// - The IM client's keyboard (so the IM sees the events)
/// - A buffer (for key events only, awaiting filter decisions)
///
/// Non-key events (enter, leave, modifiers, keymap, repeat_info) are also
/// forwarded to the real client keyboards immediately.
#[derive(Debug)]
struct FilterInterceptor {
    /// IM client's keyboard to forward events to
    im_keyboard: WlKeyboard,
    /// IM client's surface for enter/leave
    im_surface: WlSurface,
    /// Real client keyboards to forward filtered events to
    client_keyboards: Arc<Mutex<Vec<Weak<WlKeyboard>>>>,
    /// Surface the client keyboards are focused on
    focused_surface: WlSurface,
    /// Key events buffered waiting for filter decision
    pending_events: Arc<Mutex<VecDeque<BufferedEvent>>>,
}

impl WlKeyboardApi for FilterInterceptor {
    fn keymap(
        &self,
        format: wayland_server::protocol::wl_keyboard::KeymapFormat,
        fd: std::os::unix::io::BorrowedFd<'_>,
        size: u32,
    ) {
        self.im_keyboard.keymap(format, fd, size);
        for_each_focused_kbd_resource(&self.client_keyboards, &self.focused_surface, |kbd| {
            kbd.keymap(format, fd, size);
        });
    }

    fn enter(&self, serial: u32, surface: &WlSurface, keys: Vec<u8>) {
        self.im_keyboard.enter(serial, &self.im_surface, keys.clone());
        for_each_focused_kbd_resource(&self.client_keyboards, &self.focused_surface, |kbd| {
            kbd.enter(serial, surface, keys.clone());
        });
    }

    fn leave(&self, serial: u32, surface: &WlSurface) {
        self.im_keyboard.leave(serial, &self.im_surface);
        for_each_focused_kbd_resource(&self.client_keyboards, &self.focused_surface, |kbd| {
            kbd.leave(serial, surface);
        });
    }

    fn key(&self, serial: u32, time: u32, key: u32, state: KeyState) {
        self.im_keyboard.key(serial, time, key, state);
        self.pending_events.lock().unwrap().push_front(BufferedEvent {
            serial,
            time,
            key,
            state,
        });
    }

    fn modifiers(&self, serial: u32, mods_depressed: u32, mods_latched: u32, mods_locked: u32, group: u32) {
        self.im_keyboard
            .modifiers(serial, mods_depressed, mods_latched, mods_locked, group);
        for_each_focused_kbd_resource(&self.client_keyboards, &self.focused_surface, |kbd| {
            kbd.modifiers(serial, mods_depressed, mods_latched, mods_locked, group);
        });
    }

    fn repeat_info(&self, rate: i32, delay: i32) {
        self.im_keyboard.repeat_info(rate, delay);
        for_each_focused_kbd_resource(&self.client_keyboards, &self.focused_surface, |kbd| {
            kbd.repeat_info(rate, delay);
        });
    }

    fn version(&self) -> u32 {
        let mut v = None;
        for_each_focused_kbd_resource(&self.client_keyboards, &self.focused_surface, |kbd| {
            v = Some(kbd.version());
        });
        v.unwrap_or(Resource::version(&self.im_keyboard))
    }
}

/// Handle stored in the input method, used to activate/deactivate the interceptor.
#[derive(Debug, Clone)]
pub(crate) struct Filter {
    pub(crate) keyboard_filter: ZwpKeyboardFilterV1,
    pub(crate) pending_events: Arc<Mutex<VecDeque<BufferedEvent>>>,
    pub(crate) focused_surface: Arc<Mutex<Option<WlSurface>>>,
}

impl Filter {
    /// Activate keyboard interception. Events will be forwarded to the IM keyboard
    /// and buffered for filter decisions.
    ///
    /// `focused_surface` is the surface currently receiving text input (i.e. the app's surface).
    /// Passthrough events will be forwarded to client keyboards focused on this surface.
    pub fn activate_interceptor<D: SeatHandler + 'static>(
        &self,
        seat: &Seat<D>,
        focused_surface: &WlSurface,
    ) {
        let keyboard_handle = seat.get_keyboard().unwrap();
        let filter_data = self.keyboard_filter.data::<KeyboardFilterUserData<D>>().unwrap();

        *self.focused_surface.lock().unwrap() = Some(focused_surface.clone());

        let interceptor = FilterInterceptor {
            im_keyboard: filter_data.im_keyboard.clone(),
            im_surface: filter_data.im_surface.clone(),
            client_keyboards: keyboard_handle.arc.known_kbds.clone(),
            focused_surface: focused_surface.clone(),
            pending_events: self.pending_events.clone(),
        };

        let mut slot = keyboard_handle.arc.kbd_interceptor.lock().unwrap();
        *slot = Some(Box::new(interceptor));
    }

    /// Deactivate keyboard interception and drop buffered events.
    pub fn deactivate_interceptor<D: SeatHandler + 'static>(&self, seat: &Seat<D>) {
        let keyboard_handle = seat.get_keyboard().unwrap();
        let mut pending = self.pending_events.lock().unwrap();
        pending.clear();
        keyboard_handle.arc.clear_kbd_interceptor();
    }
}

/// Data accessible from the ZwpKeyboardFilterV1 object.
#[derive(Debug)]
pub struct KeyboardFilterUserData<D: SeatHandler> {
    pub(crate) keyboard_handle: KeyboardHandle<D>,
    pub(crate) pending_events: Arc<Mutex<VecDeque<BufferedEvent>>>,
    pub(crate) focused_surface: Arc<Mutex<Option<WlSurface>>>,
    pub(crate) manager_data: Arc<Mutex<KeyboardFilterManagerUserDataInner>>,
    pub(crate) bound_keyboard: WlKeyboard,
    pub(crate) bound_input_method: ZwpInputMethodV3,
    pub(crate) im_keyboard: WlKeyboard,
    pub(crate) im_surface: WlSurface,
}

impl<D: SeatHandler> KeyboardFilterUserData<D> {
    fn flush_pending_passthrough(&self) {
        let mut pending = self.pending_events.lock().unwrap();
        if let Some(ref surface) = *self.focused_surface.lock().unwrap() {
            for event in pending.drain(..) {
                for_each_focused_kbd_resource(&self.keyboard_handle.arc.known_kbds, surface, |kbd| {
                    kbd.key(event.serial, event.time, event.key, event.state);
                });
            }
        } else {
            pending.clear();
        }
    }

    fn detach(&self) {
        self.keyboard_handle.arc.clear_kbd_interceptor();
        let mut mgr = self.manager_data.lock().unwrap();
        mgr.bound_keyboards.remove(&self.bound_keyboard);
        mgr.bound_ims.remove(&self.bound_input_method);
    }
}

impl<D> Dispatch2<ZwpKeyboardFilterV1, D> for KeyboardFilterUserData<D>
where
    D: SeatHandler,
    D: 'static,
{
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        resource: &ZwpKeyboardFilterV1,
        request: <ZwpKeyboardFilterV1 as Resource>::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        use zwp_keyboard_filter_v1::Request;
        match request {
            Request::Unbind => {
                self.flush_pending_passthrough();
                self.detach();
            }
            Request::Filter { serial, action } => {
                let action = match action {
                    WEnum::Value(FilterAction::Passthrough) => true,
                    WEnum::Value(FilterAction::Consume) => false,
                    WEnum::Value(unk) => {
                        error!("Unsupported filter action {unk:?}");
                        return;
                    }
                    WEnum::Unknown(unk) => {
                        error!("Unsupported filter action {unk}");
                        return;
                    }
                };

                let mut pending = self.pending_events.lock().unwrap();
                // Find the event matching this serial (events are in reverse order, newest first)
                if let Some(pos) = pending.iter().position(|e| e.serial == serial) {
                    let event = pending.remove(pos).unwrap();
                    if action {
                        // Passthrough: forward to real client
                        let focused = self.focused_surface.lock().unwrap();
                        if let Some(ref surface) = *focused {
                            for_each_focused_kbd_resource(
                                &self.keyboard_handle.arc.known_kbds,
                                surface,
                                |kbd| {
                                    kbd.key(event.serial, event.time, event.key, event.state);
                                },
                            );
                        } else {
                            tracing::warn!("Passthrough failed: no focused_surface!");
                        }
                    }
                    // Consume: just drop the event
                } else {
                    warn!("Filter response for unknown serial {serial}");
                    resource.post_error(
                        zwp_keyboard_filter_v1::Error::InvalidSerial,
                        format!("No pending event with serial {serial}"),
                    );
                }
            }
            _ => {}
        }
    }

    fn destroyed(
        &self,
        _state: &mut D,
        _client: wayland_server::backend::ClientId,
        _resource: &ZwpKeyboardFilterV1,
    ) {
        self.flush_pending_passthrough();
        self.detach();
    }
}
