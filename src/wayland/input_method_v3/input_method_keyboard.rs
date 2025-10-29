use crate::input::keyboard::{KeyboardHandle, KnownKbds, WlKeyboardApi};
use crate::input::SeatHandler;
use crate::utils::Serial;
use std::collections::VecDeque;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex};
use super::InputMethodManagerState;
use tracing::{error, warn};
use wl_input_method::input_method::v1::server::xx_input_method_keyboard_v1::{self, FilterAction, XxInputMethodKeyboardV1};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, Resource, WEnum};
use wayland_server::{protocol::{wl_keyboard::{KeyState, WlKeyboard}, wl_surface::WlSurface}};

/// A reification of wl_keyboard events, just to be able to shift them in time.
/// Not using the event from wayland libraries directly because they'd need to be translated into a pure Rust call anyway before sending.
#[derive(Debug)]
pub(crate) enum KeyboardEvent {
    Keymap{
        format: wl_keyboard::KeymapFormat,
        fd: OwnedFd,
        size: u32,
    },
    Enter {
        serial: u32,
        surface: WlSurface,
        keys: Vec<u8>,
    },
    Leave {
        serial: u32,
        surface: WlSurface,
    },
    Key {
        serial: u32,
        time: u32,
        key: u32,
        state: KeyState,
    },
    Modifiers {
        serial: u32,
        mods_depressed: u32,
        mods_latched: u32,
        mods_locked: u32,
        group: u32,
    },
    RepeatInfo {
        rate: i32,
        delay: i32,
    },
}

impl KeyboardEvent {
    fn serial(&self) -> Option<u32> {
        match self {
            Self::Keymap{..} | Self::RepeatInfo {..} => None,
            Self::Enter { serial, .. }
            | Self::Leave { serial, .. }
            | Self::Key { serial, .. }
            | Self::Modifiers { serial, .. } => Some(*serial),
        }
    }
    
    fn describe(&self) -> &str {
        match self {
            Self::Keymap { .. } => "keymap",
            Self::Enter { .. } => "enter",
            Self::Leave { .. } => "leave",
            Self::Key { .. } => "key",
            Self::Modifiers { .. } => "modifiers",
            Self::RepeatInfo { .. } => "repeat_info",
        }
    }

    fn apply<T: WlKeyboardApi + ?Sized>(&self, k: &T) {
        // Using a reference here rather than an owned copy because the event must be applied N times, so cloning is unavoidable. 
        // Cloning the entire object where it contains an OwnedFd inside is a bit complex, so instead just cloning keys inside the call.
        // This is only inefficient if it is ever called outside a loop, once per event.
        match self {
            KeyboardEvent::Keymap { format, fd, size }=> {
                k.keymap(*format, fd.as_fd(), *size)
            },
            KeyboardEvent::Enter { serial, surface, keys } => {
                k.enter(*serial, surface, keys.clone())
            },
            KeyboardEvent::Leave { serial, surface } => {
                k.leave(*serial, surface)
            },
            KeyboardEvent::Key { serial, time, key, state } =>{
                k.key(*serial, *time, *key, *state)
            },
            KeyboardEvent::Modifiers { serial, mods_depressed, mods_latched, mods_locked, group } => {
                k.modifiers(*serial, *mods_depressed, *mods_latched, *mods_locked, *group)
            },
            KeyboardEvent::RepeatInfo { rate, delay } => {
                k.repeat_info(*rate, *delay)
            },
        }
    }
    
    /// Returns `true` if the event is filterable by the input method
    fn is_filterable(&self) -> bool {
        match self {
            KeyboardEvent::Key { .. } => true,
            _ => false,
        }
    }
}

#[derive(Debug)]
pub(crate) enum EventState {
    /// Event is already sent but confirmation has not arrived yet.
    /// Only for events which must be passed through.
    Sent,
    /// Delayed until confirmation arrives
    Delaying,
}

/// Carries data assigned in a im_keyboard::bind request.
#[derive(Debug, Clone)]
pub(crate) struct BoundKeyboard {
    /// Keyboard provided by the input method client to sniff on target surface's events.
    pub im_keyboard: WlKeyboard,
    /// Input method extensions assigned to this keyboard instance
    pub im_filter: XxInputMethodKeyboardV1,
    /// Surface on the IM side which should be target of enter and leave
    pub im_surface: WlSurface,
}

/// Filters key events to arrive to text input
pub(crate) struct KeyFilter {
    pub keyboard: BoundKeyboard,
    /// Client keyboards to which events can be forwarded
    pub client_keyboards: std::sync::Weak<Mutex<Vec<wayland_server::Weak<wl_keyboard::WlKeyboard>>>>,
    /// Events waiting for filter decision from the input method client.
    /// This queue begins with 0 to N of sent events and contains delayed ones after that.
    pub events_to_filter: Arc<Mutex<VecDeque<(EventState, KeyboardEvent)>>>,
    /// Surface to which events should be sent after filtering
    pub focused_surface: WlSurface,
}

impl KeyFilter {
    /// Creates a new instance of key filter and initializes it.
    pub(crate) fn create(
        keyboard: &BoundKeyboard,
        client_keyboards: &Arc<Mutex<
            Vec<wayland_server::Weak<wl_keyboard::WlKeyboard>>
        >>,
        surface: &WlSurface,
    ) -> Self {
        let ret = KeyFilter {
            keyboard: keyboard.clone(),
            client_keyboards: Arc::downgrade(client_keyboards),
            events_to_filter: Arc::new(Mutex::new(VecDeque::new())),
            focused_surface: surface.clone(),
        };
        keyboard.im_filter.notify_version(ret.version());
        ret
    }
    
    fn push_event(&self, event: KeyboardEvent) {
        // TODO: unnecessary (?) Sync requirement causes the need to lock
        let mut events = self.events_to_filter.lock().unwrap();
        let queue_empty = events.iter().position(|e| match e {
            (EventState::Delaying, _) => true,
            _ => false,
        }).is_some();

        let state = if queue_empty && !event.is_filterable() {
            // There are no outstanding delayed events to block incoming events, so if this one doesn't need to wait for a filter decision, send it immediately.
            self.forward(&event);
            EventState::Sent
        } else {
            EventState::Delaying
        };
        events.push_front((state, event));
    }

    /// Applies event to the text input application, if event was not already applied.
    fn apply(&self, (state, event): (EventState, KeyboardEvent)) {
        match state {
            EventState::Sent => {},
            EventState::Delaying => self.forward(&event),
        }
    }
    
    fn forward(&self, event: &KeyboardEvent) {
        let keyboards = self.client_keyboards.upgrade().unwrap();
        KnownKbds::for_each_focused_kbd(
            &*keyboards.lock().unwrap(),
            &self.focused_surface,
            |k| event.apply(k)
        );
    }
}

use wayland_server::protocol::{wl_keyboard, wl_surface};

impl WlKeyboardApi for KeyFilter {
    fn keymap(
        &self,
        format: wl_keyboard::KeymapFormat,
        fd: ::std::os::unix::io::BorrowedFd<'_>,
        size: u32,
    ) {
        self.keyboard.im_keyboard.keymap(format, fd, size);
        // FIXME: not sure if this is safe. What if the original fd closes before the event is dropped?
        if let Ok(fd) = fd.try_clone_to_owned() {
            self.push_event(KeyboardEvent::Keymap{format, fd, size});
        } else {
            error!("Failed to clone keymap fd. This will likely crash the client.");
        }
    }
    fn enter(
        &self,
        serial: u32,
        surface: &wl_surface::WlSurface,
        keys: Vec<u8>,
    ) {
        self.keyboard.im_keyboard.enter(serial, &self.keyboard.im_surface, keys.clone());
        self.push_event(KeyboardEvent::Enter {
            serial,
            surface: surface.clone(),
            keys,
        });
    }
    fn leave(&self, serial: u32, surface: &wl_surface::WlSurface) {
        self.keyboard.im_keyboard.leave(serial, &self.keyboard.im_surface);
        self.push_event(KeyboardEvent::Leave {
            serial,
            surface: surface.clone(),
        });
    }
    fn key(&self, serial: u32, time: u32, key: u32, state: wl_keyboard::KeyState) {
        let im_state = if wayland_server::Resource::version(&self.keyboard.im_keyboard) < 10 {
           match state {
               wl_keyboard::KeyState::Repeated => wl_keyboard::KeyState::Pressed,
               other => other,
           }
        } else {
            state
        };
        self.keyboard.im_keyboard.key(serial, time, key, im_state);
        self.push_event(KeyboardEvent::Key { serial, time, key, state });
    }
    fn modifiers(
        &self,
        serial: u32,
        mods_depressed: u32,
        mods_latched: u32,
        mods_locked: u32,
        group: u32,
    ) {
        self.keyboard.im_keyboard.modifiers(serial, mods_depressed, mods_latched, mods_locked, group);
        self.push_event(KeyboardEvent::Modifiers { 
            serial,
            mods_depressed,
            mods_latched,
            mods_locked,
            group,
        });
    }
    fn repeat_info(&self, rate: i32, delay: i32) {
        self.keyboard.im_keyboard.repeat_info(rate, delay);
        self.push_event(KeyboardEvent::RepeatInfo {rate, delay });
    }
    fn version(&self) -> u32 {
        let mut v = None;
        let keyboards = self.client_keyboards.upgrade().unwrap();
        let keyboards = keyboards.lock().unwrap();
        
        // Hopefully there's only one keyboard registered at the focused surface.
        // If there are multple ones, they better share the version.
        KnownKbds::for_each_focused_kbd(
            &*keyboards,
            &self.focused_surface,
            |k| {v = Some(k.version());},
        );
        v.unwrap_or(Resource::version(&self.keyboard.im_keyboard))
    }
}

/// Accessible through XxInputMethodKeyboardV1 instance.
#[derive(Debug)]
pub struct KeyboardUserData<D: SeatHandler> {
    pub(crate) keyboard_handle: KeyboardHandle<D>,
}

impl<D> Dispatch<XxInputMethodKeyboardV1, KeyboardUserData<D>, D> for InputMethodManagerState
where
    D: Dispatch<XxInputMethodKeyboardV1, KeyboardUserData<D>>,
    D: SeatHandler,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        resource: &XxInputMethodKeyboardV1,
        request: <XxInputMethodKeyboardV1 as Resource>::Request,
        data: &KeyboardUserData<D>,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let known_kbds = &data.keyboard_handle.arc.known_kbds;
        let filter = known_kbds.interceptor.lock().unwrap();
        let Some(filter) = filter.as_ref()
        else {
            error!("IM has a bound keyboard, but no interceptor is registered");
            return;
        };
        let Some(filter) = (AsRef::<dyn WlKeyboardApi + Send + Sync>::as_ref(filter)
            as &dyn WlKeyboardApi)
            .downcast_ref::<KeyFilter>()
        else { 
            error!("The registered keyboard interceptor is not the IM one");
            return;
        };
        
        use xx_input_method_keyboard_v1::Request;
        match request {
            Request::Unbind => {
                for e in filter.events_to_filter.lock().unwrap().drain(..) {
                    filter.apply(e)
                }
            }
            Request::Filter { serial, action } => {
                /// Wayland enums are not exhaustive, so they require matching on `_`. We filter out unsupported actions early, so with an exhaustive enum we can let Rust find missing patterns in `match`es later.
                #[derive(Clone, Copy)]
                enum Action {
                    Passthrough,
                    Consume,
                }

                let action = match action {
                    WEnum::Value(FilterAction::Passthrough) => Action::Passthrough,
                    WEnum::Value(FilterAction::Consume) => Action::Consume,
                    WEnum::Value(unk) => {
                        error!("Unsupported action {unk:?}");
                        return;
                    },
                    WEnum::Unknown(unk) => {
                        error!("Unsupported action {unk}");
                        return;
                    },
                };
            
                let mut events = filter.events_to_filter.lock().unwrap();
                while let Some(e) = events.pop_back() {
                    let (action, stop) = if let Some(waiting_serial) = e.1.serial() {
                        if Serial(serial) > Serial(waiting_serial) {
                            resource.post_error(xx_input_method_keyboard_v1::Error::InvalidSerial, "Filter serial newer than awaited");
                            return;
                        };
                        (action, true)
                    } else {
                        // Events without a serial will not get a confirmation. Just pass them through and go to next event.
                        (Action::Passthrough, false)
                    };
                    match (action, e.1.is_filterable()) {
                        (Action::Consume, true) => {},
                        (Action::Consume, false) => {
                            resource.post_error(
                                xx_input_method_keyboard_v1::Error::InvalidFilterAction,
                                format!("Only key events may be filtered, but tried {}", e.1.describe())
                            );
                            return;
                        },
                        (Action::Passthrough, _) => {
                            filter.apply(e)
                        },
                    }
                    if stop {
                        return;
                    }
                }
                warn!("Maybe stale filter action {serial}: no event waiting")
            }
            _ => {}
        }
    }
}