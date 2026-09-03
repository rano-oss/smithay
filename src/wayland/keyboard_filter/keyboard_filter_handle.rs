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
    SeatHandler,
    keyboard::{KeyboardHandle, WlKeyboardApi},
};
use crate::wayland::{Dispatch2, input_method::InputMethodV3UserData};

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

impl FilterInterceptor {
    fn for_each_client_kbd(&self, mut f: impl FnMut(&WlKeyboard)) {
        let known_kbds = &self.client_keyboards;
        for kbd in &*known_kbds.lock().unwrap() {
            let Ok(kbd) = kbd.upgrade() else {
                continue;
            };

            if kbd.id().same_client_as(&self.focused_surface.id()) {
                f(&kbd);
            }
        }
    }
}

impl WlKeyboardApi for FilterInterceptor {
    fn keymap(
        &self,
        format: wayland_server::protocol::wl_keyboard::KeymapFormat,
        fd: std::os::unix::io::BorrowedFd<'_>,
        size: u32,
    ) {
        self.im_keyboard.keymap(format, fd, size);
        self.for_each_client_kbd(|kbd| {
            kbd.keymap(format, fd, size);
        });
    }

    fn enter(&self, serial: u32, surface: &WlSurface, keys: Vec<u8>) {
        self.im_keyboard.enter(serial, &self.im_surface, keys.clone());
        self.for_each_client_kbd(|kbd| {
            kbd.enter(serial, surface, keys.clone());
        });
    }

    fn leave(&self, serial: u32, surface: &WlSurface) {
        self.im_keyboard.leave(serial, &self.im_surface);
        self.for_each_client_kbd(|kbd| {
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
        self.for_each_client_kbd(|kbd| {
            kbd.modifiers(serial, mods_depressed, mods_latched, mods_locked, group);
        });
    }

    fn repeat_info(&self, rate: i32, delay: i32) {
        self.im_keyboard.repeat_info(rate, delay);
        self.for_each_client_kbd(|kbd| {
            kbd.repeat_info(rate, delay);
        });
    }

    fn protocol_version(&self) -> u32 {
        let mut v = None;
        self.for_each_client_kbd(|kbd| {
            v = Some(kbd.version());
        });
        v.unwrap_or(Resource::version(&self.im_keyboard))
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
    pub(crate) im_surface: WlSurface,
}

impl<D: SeatHandler + 'static> KeyboardFilterUserData<D> {
    /// Activate keyboard interception. Events will be forwarded to the IM keyboard
    /// and buffered for filter decisions.
    ///
    /// `focused_surface` is the surface currently receiving text input (i.e. the app's surface).
    /// Passthrough events will be forwarded to client keyboards focused on this surface.
    pub(crate) fn activate_interceptor(&self, focused_surface: &WlSurface) {
        *self.focused_surface.lock().unwrap() = Some(focused_surface.clone());

        let interceptor = FilterInterceptor {
            im_keyboard: self.bound_keyboard.clone(),
            im_surface: self.im_surface.clone(),
            client_keyboards: self.keyboard_handle.arc.known_kbds.clone(),
            focused_surface: focused_surface.clone(),
            pending_events: self.pending_events.clone(),
        };

        let mut slot = self.keyboard_handle.arc.kbd_interceptor.lock().unwrap();
        *slot = Some(Box::new(interceptor));
    }

    /// Deactivate keyboard interception and drop buffered events.
    pub(crate) fn deactivate_interceptor(&self) {
        self.pending_events.lock().unwrap().clear();
        self.keyboard_handle.arc.clear_kbd_interceptor();
    }

    fn flush_pending_passthrough(&self) {
        let mut pending = self.pending_events.lock().unwrap();
        if let Some(ref surface) = *self.focused_surface.lock().unwrap() {
            for event in pending.drain(..) {
                let known_kbds = &self.keyboard_handle.arc.known_kbds;
                for kbd in &*known_kbds.lock().unwrap() {
                    let Ok(kbd) = kbd.upgrade() else {
                        continue;
                    };

                    if kbd.id().same_client_as(&surface.id()) {
                        kbd.key(event.serial, event.time, event.key, event.state);
                    }
                }
            }
        } else {
            pending.clear();
        }
    }

    fn detach(&self) {
        self.deactivate_interceptor();
        let mut mgr = self.manager_data.lock().unwrap();
        mgr.bound_keyboards.remove(&self.bound_keyboard);
        mgr.bound_ims.remove(&self.bound_input_method);
        *self
            .bound_input_method
            .data::<InputMethodV3UserData<D>>()
            .unwrap()
            .keyboard_filter
            .lock()
            .unwrap() = None;
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
                            let known_kbds = &self.keyboard_handle.arc.known_kbds;
                            for kbd in &*known_kbds.lock().unwrap() {
                                let Ok(kbd) = kbd.upgrade() else {
                                    continue;
                                };

                                if kbd.id().same_client_as(&surface.id()) {
                                    kbd.key(event.serial, event.time, event.key, event.state);
                                }
                            }
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
