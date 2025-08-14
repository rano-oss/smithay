use crate::input::keyboard::{KeyboardHandle, KnownKbds, WlKeyboardApi};
use crate::input::SeatHandler;
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
pub enum KeyboardEvent {
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
}

/// Stores data related to filtering key events arriving to text input
pub(crate) struct KeyFilter {
    /// Keyboard provided by the input method client to sniff on target surface's events.
    pub keyboard: WlKeyboard,
    /// Input method keyboard extensions
    pub im_keyboard: XxInputMethodKeyboardV1,
    /// Events waiting for filter decision from the input method client
    pub events_to_filter: Arc<Mutex<VecDeque<KeyboardEvent>>>,
    /// Surface to which events should be sent after filtering
    pub focused_surface: Arc<Mutex<Option<WlSurface>>>,
    /// Surface on the IM side which should be target of enter and leave
    pub im_surface: WlSurface,
}
impl KeyFilter {
    fn push_event(&self, event: KeyboardEvent) {
        // TODO: unnecessary (?) Sync requirement causes the need to lock
        let mut events = self.events_to_filter.lock().unwrap();
        events.push_front(event);
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
        self.keyboard.keymap(format, fd, size);
        // FIXME: not sure if this is safe. What if the original fd closes before the event is dropped?
        if let Ok(fd) = fd.try_clone_to_owned() {
            self.push_event(KeyboardEvent::Keymap{format, fd, size});
        } else {
            error!("Failed to clone keymap fd. This will likely crash the client.");
        }
        dbg!("keymap", format);
    }
    fn enter(
        &self,
        serial: u32,
        surface: &wl_surface::WlSurface,
        keys: Vec<u8>,
    ) {
        let mut target = self.focused_surface.lock().unwrap();
        *target = Some(surface.clone());
        self.keyboard.enter(serial, &self.im_surface, keys.clone());
        //let no_surface = wayland_server::Resource
        dbg!("enter", &keys);
        self.push_event(KeyboardEvent::Enter {
            serial,
            surface: surface.clone(),
            keys,
        });
    }
    fn leave(&self, serial: u32, surface: &wl_surface::WlSurface) {
        let mut target = self.focused_surface.lock().unwrap();
        let target = target.take();
        if target.as_ref() != Some(surface) {
            warn!("Received leave with an unfocused surface");
        }
        self.keyboard.leave(serial, &self.im_surface);
        self.push_event(KeyboardEvent::Leave {
            serial,
            surface: surface.clone(),
        });
        dbg!("leave");
    }
    fn key(&self, serial: u32, time: u32, key: u32, state: wl_keyboard::KeyState) {
        self.keyboard.key(serial, time, key, state);
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
        self.keyboard.modifiers(serial, mods_depressed, mods_latched, mods_locked, group);
        self.push_event(KeyboardEvent::Modifiers { 
            serial,
            mods_depressed,
            mods_latched,
            mods_locked,
            group,
        });
    }
    fn repeat_info(&self, rate: i32, delay: i32) {
        self.keyboard.repeat_info(rate, delay);
        self.push_event(KeyboardEvent::RepeatInfo {rate, delay });
    }
    fn version(&self) -> u32 {
        Resource::version(&self.keyboard)
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
        use xx_input_method_keyboard_v1::Request;
        match request {
            Request::Unbind => {
                data.keyboard_handle.with_keyboards_mut(|known_kbds| {
                    let Some(filter) = known_kbds.interceptor.as_mut()
                    else {
                        resource.post_error(xx_input_method_keyboard_v1::Error::NotBound, "No keyboard has been bound");
                        return;
                    };
                    let Some(filter) = (AsRef::<dyn WlKeyboardApi + Send + Sync>::as_ref(filter)
                        as &dyn WlKeyboardApi)
                        .downcast_ref::<KeyFilter>()
                    else { 
                        error!("The registered keyboard interceptor is not the IM one");
                        return;
                    };
                    let target = filter.focused_surface.lock().unwrap();
                    if let Some(surface) = target.as_ref() {
                        for e in filter.events_to_filter.lock().unwrap().drain(..) {
                            KnownKbds::for_each_focused_kbd(
                                &known_kbds.keyboards,
                                surface,
                                |k| e.apply(k)
                            );
                        }
                    } else {
                        error!("Bound keyboard still has some events but no client surface is in focus")
                    }
                    // FIXME: remove kbd
                    //data.keyboard_handle.
                })
            }
            Request::Filter { serial, action } => {
                dbg!(serial, action);
                /// Wayland enums are not exhaustive, so they require matching on `_`. We filter out unsupported actions early, so with an exhaustive enum we can let Rust find missing patterns in `match`es later.
                #[derive(Clone, Copy)]
                enum Action {
                    Passthrough,
                    Consume,
                }
                // FIXME: events coming without serial must be processed immediately if no queue
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

                data.keyboard_handle.with_keyboards_mut(|known_kbds| {
                    let Some(filter) = known_kbds.interceptor.as_mut()
                    else {
                        resource.post_error(xx_input_method_keyboard_v1::Error::NotBound, "No keyboard has been bound");
                        return;
                    };
                    let Some(filter) = (AsRef::<dyn WlKeyboardApi + Send + Sync>::as_ref(filter)
                        as &dyn WlKeyboardApi)
                        .downcast_ref::<KeyFilter>()
                    else { 
                        error!("The registered keyboard interceptor is not the IM one");
                        return;
                    };
                
                    let mut events = filter.events_to_filter.lock().unwrap();
                    while let Some(e) = events.pop_back() {
                        let (action, stop) = if let Some(waiting_serial) = e.serial() {
                            if serial != waiting_serial {
                                resource.post_error(xx_input_method_keyboard_v1::Error::InvalidSerial, "Next event's serial doesn't match request");
                                return;
                            };
                            (action, true)
                        } else {
                            // Events without a serial will not get a confirmation. Just pass them through and go to next event.
                            (Action::Passthrough, false)
                        };
                        match (action, &e) {
                            (Action::Consume, KeyboardEvent::Key{..}) => {},
                            (Action::Consume, KeyboardEvent::Keymap { .. })
                            | (Action::Consume, KeyboardEvent::Enter { .. })
                            | (Action::Consume, KeyboardEvent::Leave { .. })
                            | (Action::Consume, KeyboardEvent::Modifiers { .. })
                            | (Action::Consume, KeyboardEvent::RepeatInfo { .. }) => {
                                resource.post_error(
                                    xx_input_method_keyboard_v1::Error::InvalidSerial,
                                    format!("Only key events may be consumed, but requested to consume {}", e.describe())
                                );
                                return
                            },
                            (Action::Passthrough, e) => {
                                let target = filter.focused_surface.lock().unwrap();
                                if let Some(surface) = target.as_ref() {
                                    KnownKbds::for_each_focused_kbd(
                                        &known_kbds.keyboards,
                                        surface,
                                        |k| e.apply(k),
                                    );
                                } else {
                                    warn!("key event without a focused surface");
                                }
                            },
                        }
                        if stop {
                            return;
                        }
                    }
                    resource.post_error(xx_input_method_keyboard_v1::Error::InvalidSerial, "No event is waiting for confirmation");
                })
            }
            _ => {}
        }
    }
}