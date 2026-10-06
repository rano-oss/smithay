use std::{
    collections::{HashSet, VecDeque},
    sync::{Arc, Mutex},
};

use tracing::{error, warn};

use wayland_protocols_experimental::{
    input_method::v1::server::xx_input_method_v1::XxInputMethodV1,
    keyboard_filter::v3::server::xx_keyboard_filter_v1::{self, FilterAction, XxKeyboardFilterV1},
};
use wayland_server::WEnum;
use wayland_server::{
    Client, DataInit, DisplayHandle, Resource,
    protocol::{
        wl_keyboard::{KeyState, WlKeyboard},
        wl_surface::WlSurface,
    },
};

use crate::input::{SeatHandler, keyboard::KeyboardHandle};
use crate::utils::SERIAL_COUNTER;
use crate::wayland::{Dispatch2, input_method::InputMethodV3UserData, seat::keyboard::repeat_key_states};

#[derive(Debug)]
pub(crate) struct BufferedEvent {
    serial: u32,
    time: u32,
    key: u32,
    state: KeyState,
}

/// Seat-level routing installed while a keyboard filter is active: key events go to the
/// IM keyboard and are buffered until the IM answers; other events reach both.
#[derive(Debug)]
pub(crate) struct KbdFilter {
    pub(crate) im_keyboard: WlKeyboard,
    pending: Arc<Mutex<VecDeque<BufferedEvent>>>,
}

impl KbdFilter {
    pub(crate) fn key(&self, serial: u32, time: u32, key: u32, state: KeyState) {
        self.im_keyboard.key(serial, time, key, state);
        self.pending.lock().unwrap().push_back(BufferedEvent {
            serial,
            time,
            key,
            state,
        });
    }
}

/// User data for a bound `xx_keyboard_filter_v1`.
#[derive(Debug)]
pub struct KeyboardFilterUserData<D: SeatHandler> {
    pub(crate) keyboard_handle: KeyboardHandle<D>,
    pub(crate) pending_events: Arc<Mutex<VecDeque<BufferedEvent>>>,
    pub(crate) focused_surface: Mutex<Option<WlSurface>>,
    pub(crate) bound_keyboards: Arc<Mutex<HashSet<WlKeyboard>>>,
    pub(crate) bound_keyboard: WlKeyboard,
    pub(crate) bound_input_method: XxInputMethodV1,
    pub(crate) im_surface: WlSurface,
    /// Keys whose press was forwarded to the focused client (so Repeated is valid).
    pub(crate) client_held_keys: Mutex<HashSet<u32>>,
}

impl<D: SeatHandler + 'static> KeyboardFilterUserData<D> {
    /// Route the seat's key events through this filter while `focused_surface` has focus.
    pub(crate) fn activate(&self, focused_surface: &WlSurface) {
        *self.focused_surface.lock().unwrap() = Some(focused_surface.clone());
        let mut filter = self.keyboard_handle.arc.kbd_filter.lock().unwrap();
        if self.is_installed(&filter) {
            return;
        }
        // Pressed keys are unknown here: the seat's state may be locked by the focus change
        // that triggered this activation.
        let serial = SERIAL_COUNTER.next_serial().into();
        self.bound_keyboard.enter(serial, &self.im_surface, Vec::new());
        *filter = Some(KbdFilter {
            im_keyboard: self.bound_keyboard.clone(),
            pending: self.pending_events.clone(),
        });
    }

    fn is_installed(&self, filter: &Option<KbdFilter>) -> bool {
        filter
            .as_ref()
            .is_some_and(|f| Arc::ptr_eq(&f.pending, &self.pending_events))
    }

    /// Stop routing through this filter, forwarding (`flush`) or dropping buffered keys.
    pub(crate) fn deactivate(&self, flush: bool) {
        let pending = std::mem::take(&mut *self.pending_events.lock().unwrap());
        if flush {
            pending
                .iter()
                .for_each(|event| self.send_key_to_focused_client(event));
        }
        self.client_held_keys.lock().unwrap().clear();
        let mut filter = self.keyboard_handle.arc.kbd_filter.lock().unwrap();
        if self.is_installed(&filter) {
            *filter = None;
            self.bound_keyboard
                .leave(SERIAL_COUNTER.next_serial().into(), &self.im_surface);
        }
    }

    fn send_key_to_focused_client(&self, event: &BufferedEvent) {
        let Some(ref surface) = *self.focused_surface.lock().unwrap() else {
            return;
        };
        let mut held = self.client_held_keys.lock().unwrap();
        for kbd in &*self.keyboard_handle.arc.known_kbds.lock().unwrap() {
            let Ok(kbd) = kbd.upgrade() else {
                continue;
            };
            if !kbd.id().same_client_as(&surface.id()) {
                continue;
            }
            match event.state {
                KeyState::Pressed => {
                    held.insert(event.key);
                    kbd.key(event.serial, event.time, event.key, KeyState::Pressed);
                }
                KeyState::Released => {
                    if held.remove(&event.key) {
                        kbd.key(event.serial, event.time, event.key, KeyState::Released);
                    }
                }
                KeyState::Repeated => {
                    for &state in repeat_key_states(&kbd) {
                        kbd.key(event.serial, event.time, event.key, state);
                    }
                }
                _ => kbd.key(event.serial, event.time, event.key, event.state),
            }
        }
    }

    pub(crate) fn detach(&self) {
        self.deactivate(true);
        self.bound_keyboards.lock().unwrap().remove(&self.bound_keyboard);
        let im_data = self
            .bound_input_method
            .data::<InputMethodV3UserData<D>>()
            .unwrap();
        *im_data.keyboard_filter.lock().unwrap() = None;
    }
}

impl<D> Dispatch2<XxKeyboardFilterV1, D> for KeyboardFilterUserData<D>
where
    D: SeatHandler,
    D: 'static,
{
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        resource: &XxKeyboardFilterV1,
        request: <XxKeyboardFilterV1 as Resource>::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        use xx_keyboard_filter_v1::Request;
        match request {
            Request::Unbind => {
                self.detach();
            }
            Request::Filter { serial, action } => {
                let passthrough = match action {
                    WEnum::Value(FilterAction::Passthrough) => true,
                    WEnum::Value(FilterAction::Consume) => false,
                    other => {
                        error!("Unsupported filter action {other:?}");
                        return;
                    }
                };
                let mut pending = self.pending_events.lock().unwrap();
                let Some(pos) = pending.iter().position(|e| e.serial == serial) else {
                    warn!("Filter response for unknown serial {serial}");
                    resource.post_error(
                        xx_keyboard_filter_v1::Error::InvalidSerial,
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
            _ => unreachable!(),
        }
    }

    fn destroyed(
        &self,
        _state: &mut D,
        _client: wayland_server::backend::ClientId,
        _resource: &XxKeyboardFilterV1,
    ) {
        self.detach();
    }
}
