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
    serial: u32,
    time: u32,
    key: u32,
    state: KeyState,
}

/// Seat `kbd_interceptor`: keys go to the IM + buffer; other events fan out to IM and client.
#[derive(Debug)]
struct FilterInterceptor {
    im_keyboard: WlKeyboard,
    im_surface: WlSurface,
    client_keyboards: Arc<Mutex<Vec<Weak<WlKeyboard>>>>,
    focused_surface: WlSurface,
    pending_events: Arc<Mutex<VecDeque<BufferedEvent>>>,
}

impl FilterInterceptor {
    fn for_each_client_kbd(&self, mut f: impl FnMut(&WlKeyboard)) {
        for kbd in &*self.client_keyboards.lock().unwrap() {
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
        self.for_each_client_kbd(|kbd| kbd.keymap(format, fd, size));
    }

    fn enter(&self, serial: u32, surface: &WlSurface, keys: Vec<u8>) {
        self.im_keyboard.enter(serial, &self.im_surface, keys.clone());
        self.for_each_client_kbd(|kbd| kbd.enter(serial, surface, keys.clone()));
    }

    fn leave(&self, serial: u32, surface: &WlSurface) {
        self.im_keyboard.leave(serial, &self.im_surface);
        self.for_each_client_kbd(|kbd| kbd.leave(serial, surface));
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
        self.for_each_client_kbd(|kbd| kbd.repeat_info(rate, delay));
    }

    fn protocol_version(&self) -> u32 {
        let mut v = None;
        self.for_each_client_kbd(|kbd| v = Some(kbd.version()));
        v.unwrap_or(Resource::version(&self.im_keyboard))
    }
}

/// User data for a bound `zwp_keyboard_filter_v1`.
#[derive(Debug)]
pub struct KeyboardFilterUserData<D: SeatHandler> {
    pub(crate) keyboard_handle: KeyboardHandle<D>,
    pub(crate) pending_events: Arc<Mutex<VecDeque<BufferedEvent>>>,
    pub(crate) focused_surface: Mutex<Option<WlSurface>>,
    pub(crate) manager_data: Arc<Mutex<KeyboardFilterManagerUserDataInner>>,
    pub(crate) bound_keyboard: WlKeyboard,
    pub(crate) bound_input_method: ZwpInputMethodV3,
    pub(crate) im_surface: WlSurface,
}

impl<D: SeatHandler + 'static> KeyboardFilterUserData<D> {
    /// Install interceptor for `focused_surface` (no-op if already active for it).
    pub(crate) fn activate_interceptor(&self, focused_surface: &WlSurface) {
        {
            let slot = self.keyboard_handle.arc.kbd_interceptor.lock().unwrap();
            if slot.is_some()
                && self
                    .focused_surface
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|s| s.id() == focused_surface.id())
            {
                return;
            }
        }

        *self.focused_surface.lock().unwrap() = Some(focused_surface.clone());
        *self.keyboard_handle.arc.kbd_interceptor.lock().unwrap() = Some(Box::new(FilterInterceptor {
            im_keyboard: self.bound_keyboard.clone(),
            im_surface: self.im_surface.clone(),
            client_keyboards: self.keyboard_handle.arc.known_kbds.clone(),
            focused_surface: focused_surface.clone(),
            pending_events: self.pending_events.clone(),
        }));
    }

    /// Remove interceptor and drop buffered keys.
    pub(crate) fn deactivate_interceptor(&self) {
        self.pending_events.lock().unwrap().clear();
        self.keyboard_handle.arc.clear_kbd_interceptor();
    }

    /// Forward buffered keys to the focused client (used on unbind / IM destroy).
    pub(crate) fn flush_pending_passthrough(&self) {
        let mut pending = self.pending_events.lock().unwrap();
        for event in pending.drain(..) {
            self.send_key_to_focused_client(&event);
        }
    }

    fn send_key_to_focused_client(&self, event: &BufferedEvent) {
        let Some(ref surface) = *self.focused_surface.lock().unwrap() else {
            return;
        };
        for kbd in &*self.keyboard_handle.arc.known_kbds.lock().unwrap() {
            let Ok(kbd) = kbd.upgrade() else {
                continue;
            };
            if kbd.id().same_client_as(&surface.id()) {
                kbd.key(event.serial, event.time, event.key, event.state);
            }
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
                let passthrough = match action {
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
                let Some(pos) = pending.iter().position(|e| e.serial == serial) else {
                    warn!("Filter response for unknown serial {serial}");
                    resource.post_error(
                        zwp_keyboard_filter_v1::Error::InvalidSerial,
                        format!("No pending event with serial {serial}"),
                    );
                    return;
                };
                let event = pending.remove(pos).unwrap();
                drop(pending);
                if passthrough {
                    self.send_key_to_focused_client(&event);
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
