use std::{
    collections::VecDeque,
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

/// Seat-level routing installed while a keyboard filter is active: key events go to the
/// IM keyboard and are buffered until the IM answers; other events reach both.
#[derive(Debug)]
pub(crate) struct KbdFilter {
    pub(crate) im_keyboard: WlKeyboard,
    pending: Arc<Mutex<VecDeque<BufferedEvent>>>,
}

impl KbdFilter {
    pub(crate) fn key(&self, serial: u32, time: u32, key: u32, state: KeyState, keyboards: Vec<WlKeyboard>) {
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

/// User data for a bound `xx_keyboard_filter_v1`.
#[derive(Debug)]
pub struct KeyboardFilterUserData<D: SeatHandler> {
    pub(crate) keyboard_handle: KeyboardHandle<D>,
    pub(crate) pending_events: Arc<Mutex<VecDeque<BufferedEvent>>>,
    pub(crate) bound_keyboards: Arc<Mutex<std::collections::HashSet<WlKeyboard>>>,
    pub(crate) bound_keyboard: WlKeyboard,
    pub(crate) bound_input_method: XxInputMethodV1,
    pub(crate) im_surface: WlSurface,
    pub(crate) attached: AtomicBool,
}

impl<D: SeatHandler + 'static> KeyboardFilterUserData<D> {
    /// Route the seat's key events through this filter.
    pub(crate) fn activate(&self) {
        // Prefer keys staged by focus enter (avoids re-locking `internal`); else snapshot.
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
            if input
                .filter
                .as_ref()
                .is_some_and(|f| Arc::ptr_eq(&f.pending, &self.pending_events))
            {
                return false;
            }
            let serial = SERIAL_COUNTER.next_serial().into();
            self.bound_keyboard.enter(serial, &self.im_surface, keys);
            if let Some([depressed, latched, locked, group]) = mods {
                self.bound_keyboard
                    .modifiers(serial, depressed, latched, locked, group);
            }
            input.filter = Some(KbdFilter {
                im_keyboard: self.bound_keyboard.clone(),
                pending: self.pending_events.clone(),
            });
            true
        });
    }

    /// Stop routing through this filter, forwarding buffered keys when `flush` is true.
    ///
    /// Otherwise the queue is preserved so replies issued after deactivation remain valid.
    pub(crate) fn deactivate(&self, flush: bool) {
        if flush {
            for event in self.pending_events.lock().unwrap().drain(..) {
                Self::send_key_to_focused_client(&event);
            }
        }
        self.keyboard_handle.update_client_input(|input| {
            let installed = input
                .filter
                .as_ref()
                .is_some_and(|f| Arc::ptr_eq(&f.pending, &self.pending_events));
            if installed {
                input.filter = None;
                self.bound_keyboard
                    .leave(SERIAL_COUNTER.next_serial().into(), &self.im_surface);
            }
            installed
        });
    }

    fn send_key_to_focused_client(event: &BufferedEvent) {
        for kbd in &event.keyboards {
            match event.state {
                KeyState::Pressed | KeyState::Released => {
                    kbd.key(event.serial, event.time, event.key, event.state);
                }
                KeyState::Repeated => {
                    for &state in repeat_key_states(kbd) {
                        kbd.key(event.serial, event.time, event.key, state);
                    }
                }
                _ => kbd.key(event.serial, event.time, event.key, event.state),
            }
        }
    }

    pub(crate) fn detach(&self, resource: &XxKeyboardFilterV1) {
        if !self.attached.swap(false, Ordering::AcqRel) {
            return;
        }
        self.deactivate(true);
        self.bound_keyboards.lock().unwrap().remove(&self.bound_keyboard);
        let im_data = self
            .bound_input_method
            .data::<InputMethodV3UserData<D>>()
            .unwrap();
        let mut filter = im_data.keyboard_filter.lock().unwrap();
        if filter.as_ref() == Some(resource) {
            *filter = None;
        }
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
                self.detach(resource);
            }
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
                let Some(front) = pending.front() else {
                    warn!("Filter response for unknown serial {serial}");
                    resource.post_error(
                        xx_keyboard_filter_v1::Error::InvalidSerial,
                        format!("No pending event with serial {serial}"),
                    );
                    return;
                };
                if front.serial != serial {
                    if pending.iter().any(|event| event.serial == serial) {
                        warn!("Ignoring out-of-order filter response for serial {serial}");
                        return;
                    }
                    warn!("Filter response for unknown serial {serial}");
                    resource.post_error(
                        xx_keyboard_filter_v1::Error::InvalidSerial,
                        format!("No pending event with serial {serial}"),
                    );
                    return;
                }
                let event = pending.pop_front().unwrap();
                drop(pending);
                if passthrough {
                    Self::send_key_to_focused_client(&event);
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
