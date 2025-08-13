use std::{
    collections::VecDeque, fmt, sync::{Arc, Mutex}
};

use tracing::error;

use wayland_client::WEnum;
use wl_input_method::input_method::v1::server::{
    xx_input_method_v1::{self, KeyboardConsumeAction, XxInputMethodV1},
    xx_input_popup_surface_v2::XxInputPopupSurfaceV2,
};
use wayland_server::{backend::ClientId, protocol::{wl_keyboard::{KeyState, WlKeyboard}, wl_surface::WlSurface}};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, Resource};

use crate::{
    input::{keyboard::{KeyboardHandle, WlKeyboardApi}, SeatHandler}, utils::{Logical, Rectangle}, wayland::{compositor, seat::WaylandFocus, text_input::TextInputHandle}
};

use super::{
    input_method_popup_surface::{ImPopupLocation, PopupParent, PopupSurface}, positioner::{PositionerState, PositionerUserData}, InputMethodHandler, InputMethodManagerState, InputMethodPopupSurfaceUserData, INPUT_POPUP_SURFACE_ROLE
};

/// Slot for an optional input method
#[derive(Default, Debug)]
pub(crate) struct MaybeInstance {
    /// Optional input method
    pub instance: Option<InputMethod>,
}

/// Contains input method state
#[derive(Debug)]
pub(crate) struct InputMethod {
    pub object: XxInputMethodV1,
    pub serial: u32,
    pub active: bool,
    pub popup_handles: Vec<PopupSurface>,
    /// Relative to surface on which input method is enabled
    pub cursor_rectangle: Rectangle<i32, Logical>,
}

impl InputMethod {
    /// Send the done incrementing the serial.
    pub(crate) fn done(&mut self) {
        self.object.done();
        self.serial += 1;
    }
}

/// Handle to a possible input method instance.
#[derive(Default, Debug, Clone)]
pub struct InputMethodHandle {
    // TODO: why does this need to be shared?
    pub(crate) inner: Arc<Mutex<MaybeInstance>>,
}

impl InputMethodHandle {
    /// Assigns a new instance
    pub(super) fn add_instance(&self, instance: &XxInputMethodV1) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(instance) = inner.instance.as_mut() {
            instance.serial = 0;
            instance.object.unavailable();
        } else {
            inner.instance = Some(InputMethod {
                object: instance.clone(),
                serial: 0,
                active: false,
                popup_handles: vec![],
                cursor_rectangle: Rectangle::default(),
            });
        }
    }

    /// Whether there's an active instance of input-method.
    pub(crate) fn has_instance(&self) -> bool {
        self.inner.lock().unwrap().instance.is_some()
    }

    /// Callback function to access the input method object
    pub(crate) fn with_instance<F>(&self, f: F)
    where
        F: FnOnce(&mut InputMethod),
    {
        let mut inner = self.inner.lock().unwrap();
        if let Some(instance) = inner.instance.as_mut() {
            f(instance);
        }
    }

    pub(crate) fn set_cursor_rectangle<D: SeatHandler + 'static>(
        &self,
        state: &mut D,
        cursor: Rectangle<i32, Logical>,
    ) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(ref mut inner) = &mut inner.instance {
            let data = inner.object.data::<InputMethodUserData<D>>().unwrap();
            inner.cursor_rectangle = cursor;
            for popup_surface in &mut inner.popup_handles {
                let popup_geometry = (data.popup_geometry)(
                    state,
                    &popup_surface.get_parent().surface,
                    &cursor,
                    &popup_surface.positioner(),
                );

                let anchor = cursor; // FIXME: choose the anchor which the positioner wants

                popup_surface.set_position(ImPopupLocation {
                    anchor,
                    geometry: popup_geometry,
                });

                // TODO: send now or on .done?
                (data.popup_repositioned)(state, popup_surface.clone());
            }
        }
    }

    pub(crate) fn done(&self) {
        let mut inner = self.inner.lock().unwrap();

        if let Some(ref mut inner) = &mut inner.instance {
            for popup_surface in &mut inner.popup_handles {
                popup_surface.send_pending_configure();
            }
            inner.done();
        }
    }

    /// Activate input method on the given surface.
    pub(crate) fn activate_input_method<D: SeatHandler + 'static>(&self, _: &mut D, _surface: &WlSurface) {
        self.with_instance(|im| {
            im.object.activate();
            im.active = true;
        });
    }

    /// Deactivate the active input method.
    ///
    /// This includes a complete sequence including .done.
    pub(crate) fn deactivate_input_method<D: SeatHandler + 'static>(&self, state: &mut D) {
        self.with_instance(|im| {
            im.object.deactivate();
            im.done();
            im.active = false;
            let data = im.object.data::<InputMethodUserData<D>>().unwrap();
            for popup in im.popup_handles.drain(..) {
                (data.dismiss_popup)(state, popup.clone());
            }
        });
    }
}

/// A reification of wl_keyboard events, just to be able to shift them in time.
/// Not using the event from wayland libraries directly because they'd need to be translated into a pure Rust call anyway before sending.
enum KeyboardEvent {
    Keymap, // FIXME
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
            Self::Keymap | Self::RepeatInfo {..} => None,
            Self::Enter { serial, .. }
            | Self::Leave { serial, .. }
            | Self::Key { serial, .. }
            | Self::Modifiers { serial, .. } => Some(*serial),
        }
    }
    
    fn describe(&self) -> &str {
        match self {
            Self::Keymap => "keymap",
            Self::Enter { .. } => "enter",
            Self::Leave { .. } => "leave",
            Self::Key { .. } => "key",
            Self::Modifiers { .. } => "modifiers",
            Self::RepeatInfo { .. } => "repeat_info",
        }
    }
}

/// Stores data related to filtering key events arriving to text input
pub(crate) struct KeyFilter {
    /// Keyboard provided by the input method client to sniff on target surface's events.
    keyboard: WlKeyboard,
    /// Events waiting for filter decision from the input method client
    events_to_filter: Arc<Mutex<VecDeque<KeyboardEvent>>>,
    /// Surface to which events should be sent
    focused_surface: Option<WlSurface>,
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
        // FIXME: save keymap for replaying
        dbg!("keymap", format);
    }
    fn enter(
        &self,
        serial: u32,
        surface: &wl_surface::WlSurface,
        keys: Vec<u8>,
    ) {
        self.keyboard.enter(serial, surface, keys.clone());
                dbg!("enter", &keys);
        self.push_event(KeyboardEvent::Enter {
            serial,
            surface: surface.clone(),
            keys,
        });
    }
    fn leave(&self, serial: u32, surface: &wl_surface::WlSurface) {
        self.keyboard.leave(serial, surface);
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

/// User data of XxInputMethodV1 object
#[derive(Clone)]
pub struct InputMethodUserData<D: SeatHandler> {
    pub(super) handle: InputMethodHandle,
    pub(crate) text_input_handle: TextInputHandle,
    /// Handle to main keyboard for registering sub-keyboards
    pub(crate) keyboard_handle: KeyboardHandle<D>,
    /// Filtering key events before they reach text input
    pub(crate) key_filter: Arc<Mutex<Option<KeyFilter>>>,
    /// This is just a copy from Input MethodHandler. It's here in order to break the requirement for D: InputMethodHandler on functions that call dismiss_popup. That means other modules don't have to explicitly put D: InputMethodHandler when they call something that ends up calling this.
    /// (Not sure what the purpose of that is, but it seems consistent...)
    pub(crate) popup_geometry:
        fn(&D, &WlSurface, &Rectangle<i32, Logical>, &PositionerState) -> Rectangle<i32, Logical>,
    pub(crate) popup_repositioned: fn(&mut D, PopupSurface),
    pub(crate) dismiss_popup: fn(&mut D, PopupSurface),
}

impl<D: SeatHandler> fmt::Debug for InputMethodUserData<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputMethodUserData")
            .field("handle", &self.handle)
            .field("text_input_handle", &self.text_input_handle)
            .finish()
    }
}

impl<D> Dispatch<XxInputMethodV1, InputMethodUserData<D>, D> for InputMethodManagerState
where
    D: Dispatch<XxInputMethodV1, InputMethodUserData<D>>,
    D: Dispatch<XxInputPopupSurfaceV2, InputMethodPopupSurfaceUserData>,
    D: SeatHandler,
    D: InputMethodHandler,
    <D as SeatHandler>::KeyboardFocus: WaylandFocus,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        im: &XxInputMethodV1,
        request: xx_input_method_v1::Request,
        data: &InputMethodUserData<D>,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        use xx_input_method_v1::Request;
        match request {
            Request::CommitString { text } => {
                data.text_input_handle.with_active_text_input(|ti, _surface| {
                    ti.commit_string(Some(text.clone()));
                });
            }
            Request::SetPreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                data.text_input_handle.with_active_text_input(|ti, _surface| {
                    ti.preedit_string(Some(text.clone()), cursor_begin, cursor_end);
                });
            }
            Request::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                data.text_input_handle.with_active_text_input(|ti, _surface| {
                    ti.delete_surrounding_text(before_length, after_length);
                });
            }
            Request::Commit { serial } => {
                let current_serial = data
                    .handle
                    .inner
                    .lock()
                    .unwrap()
                    .instance
                    .as_ref()
                    .map(|i| i.serial)
                    .unwrap_or(0);

                data.text_input_handle.done(serial != current_serial);
            }
            Request::GetInputPopupSurface {
                id,
                surface,
                positioner,
            } => {
                let mut input_method = data.handle.inner.lock().unwrap();
                if let Some(instance) = &mut input_method.instance {
                    if instance.active {
                        if compositor::give_role(&surface, INPUT_POPUP_SURFACE_ROLE).is_err()
                            && compositor::get_role(&surface) != Some(INPUT_POPUP_SURFACE_ROLE)
                        {
                            im.post_error(
                                xx_input_method_v1::Error::SurfaceHasRole,
                                "Surface already has a role.",
                            );
                            return;
                        }

                        let parent_surface = match data.text_input_handle.focus().clone() {
                            Some(parent) => parent,
                            None => {
                                im.post_error(
                                    xx_input_method_v1::Error::Inactive,
                                    "Popup may only be created on an active input method (no surface in text input focus).",
                                );
                                return;
                            }
                        };

                        let location = state.parent_geometry(&parent_surface);
                        let parent = PopupParent {
                            surface: parent_surface,
                            location,
                        };

                        let positioner_data = *positioner
                            .data::<PositionerUserData>()
                            .unwrap()
                            .inner
                            .lock()
                            .unwrap();

                        let geometry = state.popup_geometry(
                            &parent.surface,
                            &instance.cursor_rectangle,
                            &positioner_data,
                        );

                        // TODO: feed the popup with the anchor chosen by the positioner
                        let popup = PopupSurface::new(
                            |data| data_init.init(id, data),
                            im.clone(),
                            parent,
                            surface,
                            instance.cursor_rectangle,
                            geometry,
                            positioner_data,
                        );
                        instance.popup_handles.push(popup.clone());
                        state.new_popup(popup);
                    } else {
                        im.post_error(
                            xx_input_method_v1::Error::Inactive,
                            "Popup may only be created on an active input method.",
                        );
                    }
                }
            }
            Request::KeyboardBind { keyboard } => {
                let mut key_filter = data.keyboard_handle.with_interceptor(|key_filter| {
                    if key_filter.is_some() {
                        im.post_error(xx_input_method_v1::Error::KeyboardAlreadyBound, "A keyboard was already bound");
                    } else {
                        *key_filter = Some(Box::new(KeyFilter {
                            keyboard,
                            events_to_filter: Arc::new(Mutex::new(VecDeque::new())),
                            focused_surface: None,
                        }));
                    }
                });
            },
            Request::KeyboardUnbind => {
                let mut key_filter = data.key_filter.lock().unwrap();
                if let Some(filter) = key_filter.as_mut() {
                    if let Some(surface) = &filter.focused_surface {
                        for e in filter.events_to_filter.lock().unwrap().drain(..) {
                            let keyboards = data.keyboard_handle.arc.known_kbds.lock().unwrap();
                            keyboards.for_each_focused(surface, |k| {
                                match &e {
                                    KeyboardEvent::Keymap => {},
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
                            });
                        }
                    } else {
                        error!("Bound keyboard still has some events but no client surface is in focus")
                    }
                    // FIXME: remove kbd
                    //data.keyboard_handle.
                } else {
                    im.post_error(xx_input_method_v1::Error::KeyboardNotBound, "No keyboard has been bound");
                }
            }
            Request::KeyboardConsume { serial, action } => {
                dbg!(serial, action);
                /// Wayland enums are not exhaustive, so they require matching on `_`. We filter out unsupported actions early, so with an exhaustive enum we can let Rust find missing patterns in `match`es later.
                #[derive(Clone, Copy)]
                enum Action {
                    Passthrough,
                    Consume,
                }
                // FIXME: events coming without serial must be processed immediately if no queue
                let action = match action {
                    WEnum::Value(KeyboardConsumeAction::Passthrough) => Action::Passthrough,
                    WEnum::Value(KeyboardConsumeAction::Consume) => Action::Consume,
                    WEnum::Value(unk) => {
                        error!("Unsupported action {unk:?}");
                        return;
                    },
                    WEnum::Unknown(unk) => {
                        error!("Unsupported action {unk}");
                        return;
                    },
                };

                let mut key_filter = data.key_filter.lock().unwrap();
                if let Some(filter) = key_filter.as_mut() {
                    let mut events = filter.events_to_filter.lock().unwrap();
                    while let Some(e) = events.pop_back() {
                        let (action, stop) = if let Some(waiting_serial) = e.serial() {
                            if serial != waiting_serial {
                                im.post_error(xx_input_method_v1::Error::InvalidSerial, "Next event's serial doesn't match request");
                                return;
                            };
                            (action, true)
                        } else {
                            // Events without a serial will not get a confirmation. Just pass them through and go to next event.
                            (Action::Passthrough, false)
                        };
                        match (action, &e) {
                            (Action::Consume, KeyboardEvent::Key{..}) => {},
                            (Action::Consume, KeyboardEvent::Keymap)
                            | (Action::Consume, KeyboardEvent::Enter { .. })
                            | (Action::Consume, KeyboardEvent::Leave { .. })
                            | (Action::Consume, KeyboardEvent::Modifiers { .. })
                            | (Action::Consume, KeyboardEvent::RepeatInfo { .. }) => {
                                im.post_error(
                                    xx_input_method_v1::Error::InvalidSerial,
                                    format!("Only key events may be consumed, but requested to consume {}", e.describe())
                                );
                                return
                            },
                            (Action::Passthrough, e) => {
                                // TODO: forward
                            },
                        }
                        if stop {
                            return;
                        }
                    }
                    im.post_error(xx_input_method_v1::Error::InvalidSerial, "No event is waiting for confirmation");
                } else {
                    im.post_error(xx_input_method_v1::Error::KeyboardNotBound, "No keyboard has been bound");
                }
            }
            Request::Destroy => {
                // Nothing to do
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        _state: &mut D,
        _client: ClientId,
        _input_method: &XxInputMethodV1,
        data: &InputMethodUserData<D>,
    ) {
        data.handle.inner.lock().unwrap().instance = None;
        data.text_input_handle.leave();
    }
}
