use std::{
    collections::{HashSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use tracing::{error, warn};
use wayland_protocols_experimental::{
    input_method::v1::server::xx_input_method_v1::XxInputMethodV1,
    keyboard_filter::v3::server::xx_keyboard_filter_v1::{self, FilterAction, XxKeyboardFilterV1},
};
use wayland_server::{
    Client, DataInit, DisplayHandle, Resource, WEnum,
    protocol::{
        wl_keyboard::{KeyState, WlKeyboard},
        wl_surface::WlSurface,
    },
};

use crate::input::{SeatHandler, keyboard::KeyboardHandle};
use crate::utils::SERIAL_COUNTER;
use crate::wayland::{
    Dispatch2,
    input_method::InputMethodV3UserData,
    seat::keyboard::{repeat_key_states, serialize_pressed_keys},
};

#[derive(Debug)]
pub(crate) struct BufferedEvent {
    serial: u32,
    time: u32,
    key: u32,
    state: KeyState,
    keyboards: Vec<WlKeyboard>,
}

#[derive(Clone, Debug)]
pub(crate) struct KbdFilter {
    pub(crate) im_keyboard: WlKeyboard,
    pending: Arc<Mutex<VecDeque<BufferedEvent>>>,
}

impl KbdFilter {
    pub(crate) fn key(&self, serial: u32, time: u32, key: u32, state: KeyState, keyboards: Vec<WlKeyboard>) {
        // Filter keycodes are Linux/evdev (wkb LEFT_SHIFT/RIGHT_SHIFT), not xkb.
        if key == 42 || key == 54 {
            tracing::warn!(serial, time, key, ?state, pending = self.pending.lock().unwrap().len(), "kbd-filter shift -> IM");
        }
        self.im_keyboard.key(serial, time, key, state);
        self.pending.lock().unwrap().push_back(BufferedEvent {
            serial,
            time,
            key,
            state,
            keyboards,
        });
    }
}

/// User data for `xx_keyboard_filter_v1`.
#[derive(Debug)]
pub struct KeyboardFilterUserData<D: SeatHandler> {
    pub(crate) keyboard_handle: KeyboardHandle<D>,
    pub(crate) pending_events: Arc<Mutex<VecDeque<BufferedEvent>>>,
    pub(crate) bound_keyboards: Arc<Mutex<HashSet<WlKeyboard>>>,
    pub(crate) bound_keyboard: WlKeyboard,
    pub(crate) bound_input_method: XxInputMethodV1,
    pub(crate) im_surface: WlSurface,
    pub(crate) attached: AtomicBool,
}

impl<D: SeatHandler + 'static> KeyboardFilterUserData<D> {
    pub(crate) fn activate(&self) {
        let staged = self
            .keyboard_handle
            .arc
            .client_input
            .lock()
            .unwrap()
            .pending_filter_keys
            .take();
        let (keys, mods) = match staged {
            Some(keys) => (keys, None),
            None => {
                let g = self.keyboard_handle.arc.internal.lock().unwrap();
                let m = g.mods_state.serialized;
                (
                    serialize_pressed_keys(g.forwarded_pressed_keys.iter().copied()),
                    Some([m.depressed, m.latched, m.locked, m.layout_effective]),
                )
            }
        };
        self.keyboard_handle.update_client_input(|input| {
            if input.filter.as_ref().is_some_and(|f| f.im_keyboard == self.bound_keyboard) {
                return false;
            }
            let serial = SERIAL_COUNTER.next_serial().into();
            self.bound_keyboard.enter(serial, &self.im_surface, keys);
            if let Some([d, l, k, g]) = mods {
                self.bound_keyboard.modifiers(serial, d, l, k, g);
            }
            input.filter = Some(KbdFilter {
                im_keyboard: self.bound_keyboard.clone(),
                pending: self.pending_events.clone(),
            });
            true
        });
    }

    pub(crate) fn deactivate(&self, flush: bool) {
        if flush {
            for event in self.pending_events.lock().unwrap().drain(..) {
                forward_key(&event);
            }
        }
        self.keyboard_handle.update_client_input(|input| {
            if input.filter.as_ref().is_none_or(|f| f.im_keyboard != self.bound_keyboard) {
                return false;
            }
            input.filter = None;
            self.bound_keyboard
                .leave(SERIAL_COUNTER.next_serial().into(), &self.im_surface);
            true
        });
    }

    pub(crate) fn detach(&self, resource: &XxKeyboardFilterV1) {
        if !self.attached.swap(false, Ordering::AcqRel) {
            return;
        }
        self.deactivate(true);
        self.bound_keyboards.lock().unwrap().remove(&self.bound_keyboard);
        let im = self
            .bound_input_method
            .data::<InputMethodV3UserData<D>>()
            .unwrap();
        let mut slot = im.keyboard_filter.lock().unwrap();
        if slot.as_ref() == Some(resource) {
            *slot = None;
        }
    }
}

fn forward_key(event: &BufferedEvent) {
    for kbd in &event.keyboards {
        match event.state {
            KeyState::Repeated => {
                for &s in repeat_key_states(kbd) {
                    kbd.key(event.serial, event.time, event.key, s);
                }
            }
            s => kbd.key(event.serial, event.time, event.key, s),
        }
    }
}

impl<D: SeatHandler + 'static> Dispatch2<XxKeyboardFilterV1, D> for KeyboardFilterUserData<D> {
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
            Request::Unbind => self.detach(resource),
            Request::Filter { serial, action } => {
                if !self.attached.load(Ordering::Acquire) {
                    return;
                }
                let passthrough = match action {
                    WEnum::Value(FilterAction::Passthrough) => true,
                    WEnum::Value(FilterAction::Consume) => false,
                    other => {
                        error!("Unsupported filter action {other:?}");
                        return;
                    }
                };
                let mut pending = self.pending_events.lock().unwrap();
                // Protocol: only the oldest pending serial is valid. Unknown serials
                // may raise invalid_serial; other invalid serials (e.g. out-of-order)
                // must be ignored so filtering stays strictly in arrival order.
                match pending.front() {
                    Some(e) if e.serial == serial => {}
                    Some(_) if pending.iter().any(|e| e.serial == serial) => {
                        warn!("Ignoring out-of-order filter response for serial {serial}");
                        return;
                    }
                    _ => {
                        warn!("Filter response for unknown serial {serial}");
                        resource.post_error(
                            xx_keyboard_filter_v1::Error::InvalidSerial,
                            format!("No pending event with serial {serial}"),
                        );
                        return;
                    }
                }
                let event = pending.pop_front().unwrap();
                drop(pending);
                if passthrough {
                    forward_key(&event);
                }
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        &self,
        _state: &mut D,
        _client: wayland_server::backend::ClientId,
        resource: &XxKeyboardFilterV1,
    ) {
        self.detach(resource);
    }
}
