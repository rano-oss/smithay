//! Filtering key presses
//!
//! This Wayland protocol allows an application to register itself as a filter for key presses destined for another application.
//!
//! The current implementation supports filtering for input method purposes.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use tracing::{error, warn};

use wayland_server::WEnum;
use wayland_server::{
    backend::GlobalId,
    protocol::{wl_keyboard::WlKeyboard, wl_surface::WlSurface},
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, Weak,
};
use wl_input_method::{
    input_method::v1::server::xx_input_method_v1::XxInputMethodV1,
    keyboard_filter::v1::server::{
        xx_keyboard_filter_manager_v1::{self, XxKeyboardFilterManagerV1},
        xx_keyboard_filter_v1::{self, FilterAction, XxKeyboardFilterV1},
    },
};

use crate::{
    input::{keyboard::KeyboardHandle, Seat, SeatHandler},
    utils::{Serial, SERIAL_COUNTER},
    wayland::{
        input_method_v3::InputMethodUserData,
        keyboard_filter::queue::Action,
        seat::{KeyboardUserData, WaylandFocus},
    },
};

mod fake_seat;
mod grab;
mod queue;

pub(crate) use queue::DispatchQueue;

const MANAGER_VERSION: u32 = 1;

/// Data associated with the global.
#[allow(missing_debug_implementations)]
pub struct KeyboardFilterManagerGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

/// Data accesible from XxKeyboardFilterManagerV1
#[derive(Debug)]
pub struct KeyboardFilterManagerUserData {
    inner: Arc<Mutex<KeyboardFilterManagerUserDataInner>>,
}

#[derive(Debug, Default)]
struct KeyboardFilterManagerUserDataInner {
    bound_keyboards: HashSet<WlKeyboard>,
    bound_ims: HashSet<XxInputMethodV1>,
}

/// State of the protocol
#[derive(Debug)]
pub struct KeyboardFilterManagerState {
    global: GlobalId,
}

impl KeyboardFilterManagerState {
    /// Initialize a manager global.
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<XxKeyboardFilterManagerV1, KeyboardFilterManagerGlobalData>,
        D: Dispatch<XxKeyboardFilterManagerV1, KeyboardFilterManagerUserData>,
        D: SeatHandler,
        D: 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let data = KeyboardFilterManagerGlobalData {
            filter: Box::new(filter),
        };
        let global = display.create_global::<D, XxKeyboardFilterManagerV1, _>(MANAGER_VERSION, data);

        Self { global }
    }

    /// Get the id of manager global
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

impl<D> GlobalDispatch<XxKeyboardFilterManagerV1, KeyboardFilterManagerGlobalData, D>
    for KeyboardFilterManagerState
where
    D: GlobalDispatch<XxKeyboardFilterManagerV1, KeyboardFilterManagerGlobalData>,
    D: Dispatch<XxKeyboardFilterManagerV1, KeyboardFilterManagerUserData>,
    D: 'static,
{
    fn bind(
        _: &mut D,
        _: &DisplayHandle,
        _: &Client,
        resource: New<XxKeyboardFilterManagerV1>,
        _: &KeyboardFilterManagerGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(
            resource,
            KeyboardFilterManagerUserData {
                inner: Arc::new(Mutex::new(Default::default())),
            },
        );
    }

    fn can_view(client: Client, global_data: &KeyboardFilterManagerGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

fn register_kbd<D>(
    keyboard_handle: &KeyboardHandle<D>,
    new_keyboard: &WlKeyboard,
    surface_target: Option<&WlSurface>,
) where
    D: SeatHandler + 'static,
    <D as SeatHandler>::KeyboardFocus: WaylandFocus,
{
    keyboard_handle.register_kbd(new_keyboard, surface_target)
}

impl<D> Dispatch<XxKeyboardFilterManagerV1, KeyboardFilterManagerUserData, D> for KeyboardFilterManagerState
where
    D: Dispatch<XxKeyboardFilterManagerV1, KeyboardFilterManagerUserData>,
    D: Dispatch<XxKeyboardFilterV1, KeyboardFilterUserData<D>>,
    D: SeatHandler,
    <D as SeatHandler>::KeyboardFocus: WaylandFocus,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        resource: &XxKeyboardFilterManagerV1,
        request: xx_keyboard_filter_manager_v1::Request,
        data: &KeyboardFilterManagerUserData,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            xx_keyboard_filter_manager_v1::Request::BindToInputMethod {
                keyboard,
                input_method,
                surface,
                extensions,
            } => {
                dbg!("Binding stub");
                {
                    let bind = data.inner.lock().unwrap();
                    if bind.bound_keyboards.contains(&keyboard) {
                        resource.post_error(
                            xx_keyboard_filter_manager_v1::Error::AlreadyBound,
                            format!("WlKeyboard {keyboard:?} already bound"),
                        );
                        return;
                    };
                    if bind.bound_ims.contains(&input_method) {
                        resource.post_error(
                            xx_keyboard_filter_manager_v1::Error::AlreadyBound,
                            format!("Input method {input_method:?} already bound"),
                        );
                        return;
                    };
                }

                let imdata = input_method.data::<InputMethodUserData<D>>().unwrap();

                let keyboard_data = keyboard.data::<KeyboardUserData<D>>().unwrap();

                let keyboard_filter = data_init.init::<XxKeyboardFilterV1, _>(
                    extensions,
                    KeyboardFilterUserData {
                        keyboard_handle: keyboard_data
                            .handle
                            .as_ref()
                            .expect("Seat doesn't support keyboard")
                            .clone(),
                        manager_data: data.inner.clone(),
                        queue_slot: Default::default(),
                        bound_keyboard: keyboard.clone(),
                        bound_input_method: input_method.clone(),
                    },
                );

                {
                    let mut im_filter = imdata.keyboard_filter.lock().unwrap();
                    *im_filter = Some(Filter {
                        intercept_keyboard: keyboard.clone(),
                        intercept_surface: surface,
                        keyboard_filter,
                        register_kbd,
                    });
                }

                {
                    let mut bind = data.inner.lock().unwrap();
                    bind.bound_keyboards.insert(keyboard);
                    bind.bound_ims.insert(input_method);
                }

                // FIXME: if IM already active, register the filter as keyboard interceptor
            }
            xx_keyboard_filter_manager_v1::Request::Destroy => {
                panic!("what to do?")
            }
            _ => {}
        }
    }
}

#[allow(missing_docs)] // TODO
#[macro_export]
macro_rules! delegate_keyboard_filter_manager_v1 {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        $crate::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::reexports::wayland_protocols_experimental::keyboard_filter::v1::server::xx_keyboard_filter_manager_v1::XxKeyboardFilterManagerV1:
            $crate::wayland::keyboard_filter::KeyboardFilterManagerGlobalData
        ] => $crate::wayland::keyboard_filter::KeyboardFilterManagerState);
        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::reexports::wayland_protocols_experimental::keyboard_filter::v1::server::xx_keyboard_filter_manager_v1::XxKeyboardFilterManagerV1: $crate::wayland::keyboard_filter::KeyboardFilterManagerUserData
        ] => $crate::wayland::keyboard_filter::KeyboardFilterManagerState);
        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::reexports::wayland_protocols_experimental::keyboard_filter::v1::server::xx_keyboard_filter_v1::XxKeyboardFilterV1: $crate::wayland::keyboard_filter::KeyboardFilterUserData<Self>
        ] => $crate::wayland::keyboard_filter::KeyboardFilterManagerState);
    }
}

/// Handle for the input method to use
#[derive(Debug, Clone)]
pub(crate) struct Filter<D>
where
    D: SeatHandler,
{
    intercept_keyboard: WlKeyboard,
    intercept_surface: WlSurface,
    /// Needed to access queue slot. KeyboardGrab doesn't have access to this struct directly, and it shouldn't. This is exclusively to help input method register a new grab and is kept simple and Arc-free.
    keyboard_filter: XxKeyboardFilterV1,
    register_kbd: fn(&KeyboardHandle<D>, &WlKeyboard, Option<&WlSurface>),
}

impl<D> Filter<D>
where
    D: SeatHandler + 'static,
{
    pub fn activate_grab(
        &self,
        state: &mut D,
        seat: &Seat<D>,
        //keyboards: &Arc<Mutex<Vec<Weak<WlKeyboard>>>>,
        //surface: &WlSurface,
    ) {
        let queue = DispatchQueue::new(); //keyboards, surface);
        {
            let keyboard_handle = seat.get_keyboard().unwrap().clone();
            let keymap_file = keyboard_handle.arc.keymap.clone();

            keyboard_handle.set_grab(
                state,
                grab::KeyboardFilterGrab::new(
                    self.register_kbd,
                    self.intercept_keyboard.clone(),
                    self.intercept_surface.clone(),
                    keymap_file,
                    queue.clone(),
                ),
                // WARNING: no idea what the serial is for
                SERIAL_COUNTER.next_serial(),
            );
        }
        let filter_data = self.keyboard_filter.data::<KeyboardFilterUserData<D>>().unwrap();
        *filter_data.queue_slot.lock().unwrap() = Some(queue);
    }
}

/*
    /// Keyboard provided by the filter client to sniff on target surface's events.
    pub im_keyboard: WlKeyboard,
    /// Keyboard filter assigned to this keyboard instance
    pub filter: XxKeyboardFilterV1,
    /// Surface on the filterer side which should be target of enter and leave
    pub im_surface: WlSurface,
}*/

/// Accessible through XxKeyboardFilterV1 instance.
#[derive(Debug)]
pub struct KeyboardFilterUserData<D: SeatHandler> {
    /// Keyboard to which events after filtering get directed
    keyboard_handle: KeyboardHandle<D>,
    queue_slot: Arc<Mutex<Option<DispatchQueue>>>,
    manager_data: Arc<Mutex<KeyboardFilterManagerUserDataInner>>,
    bound_keyboard: WlKeyboard,
    bound_input_method: XxInputMethodV1,
}

impl<D> Dispatch<XxKeyboardFilterV1, KeyboardFilterUserData<D>, D> for KeyboardFilterManagerState
where
    D: Dispatch<XxKeyboardFilterV1, KeyboardFilterUserData<D>>,
    D: SeatHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &XxKeyboardFilterV1,
        request: <XxKeyboardFilterV1 as Resource>::Request,
        data: &KeyboardFilterUserData<D>,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let queue = data.queue_slot.lock().unwrap();
        use xx_keyboard_filter_v1::Request;
        match request {
            Request::Unbind => {
                if let Some(queue) = queue.as_ref() {
                    for e in queue.events().lock().unwrap().drain(..) {
                        queue.apply(e, Action::Passthrough, &data.keyboard_handle, state);
                    }
                }
                let mut mgr = data.manager_data.lock().unwrap();
                mgr.bound_keyboards.remove(&data.bound_keyboard);
                mgr.bound_ims.remove(&data.bound_input_method);
            }
            Request::Filter { serial, action } => {
                let Some(queue) = queue.as_ref() else {
                    resource.post_error(
                        xx_keyboard_filter_v1::Error::InvalidSerial,
                        "No outstanding events to filter",
                    );
                    return;
                };

                let action = match action {
                    WEnum::Value(FilterAction::Passthrough) => Action::Passthrough,
                    WEnum::Value(FilterAction::Consume) => Action::Consume,
                    WEnum::Value(unk) => {
                        error!("Unsupported action {unk:?}");
                        return;
                    }
                    WEnum::Unknown(unk) => {
                        error!("Unsupported action {unk}");
                        return;
                    }
                };

                let mut events = queue.events().lock().unwrap();
                while let Some(e) = events.pop_back() {
                    let (action, stop) = {
                        if Serial(serial) > e.1.serial {
                            resource.post_error(
                                xx_keyboard_filter_v1::Error::InvalidSerial,
                                "Filter serial newer than awaited",
                            );
                            return;
                        };
                        (action, true)
                    };
                    //if e.1.is_filterable() {
                    queue.apply(e, action, &data.keyboard_handle, state);
                    if stop {
                        return;
                    }
                }
                warn!("Maybe stale filter action {serial}: no event waiting")
            }
            _ => {}
        }
    }
    fn destroyed(
        state: &mut D,
        _client: wayland_server::backend::ClientId,
        _resource: &XxKeyboardFilterV1,
        data: &KeyboardFilterUserData<D>,
    ) {
        {
            let queue = data.queue_slot.lock().unwrap();
            if let Some(queue) = queue.as_ref() {
                for e in queue.events().lock().unwrap().drain(..) {
                    queue.apply(e, Action::Passthrough, &data.keyboard_handle, state)
                }
            }
        }
        // FIXME: this might unset another grab.
        data.keyboard_handle.unset_grab(state);
        let mut mgr = data.manager_data.lock().unwrap();
        mgr.bound_keyboards.remove(&data.bound_keyboard);
        mgr.bound_ims.remove(&data.bound_input_method);
    }
}

#[cfg(test)]
mod test {
    use crate::input::Seat;

    use super::*;
    use wl_input_method::keyboard_filter::v1::server as protocol;

    struct Handler {}

    impl SeatHandler for Handler {
        type KeyboardFocus = WlSurface;
        type PointerFocus = WlSurface;
        type TouchFocus = WlSurface;
        fn seat_state(&mut self) -> &mut crate::input::SeatState<Self> {
            unreachable!("Test code not meant to be executed");
        }
        fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&Self::KeyboardFocus>) {}
        fn cursor_image(&mut self, _seat: &Seat<Self>, _image: crate::input::pointer::CursorImageStatus) {}
        fn led_state_changed(&mut self, _seat: &Seat<Self>, _led_state: crate::input::keyboard::LedState) {}
    }

    delegate_keyboard_filter_manager_v1!(Handler);

    fn assert_is_manager_delegate<T>()
    where
        T: wayland_server::Dispatch<
            protocol::xx_keyboard_filter_manager_v1::XxKeyboardFilterManagerV1,
            KeyboardFilterManagerUserData,
        >,
    {
    }

    fn assert_is_delegate<T>()
    where
        T: SeatHandler,
        T: wayland_server::Dispatch<
            protocol::xx_keyboard_filter_v1::XxKeyboardFilterV1,
            KeyboardFilterUserData<T>,
        >,
    {
    }

    #[test]
    fn test_valid_assignment() {
        assert_is_manager_delegate::<Handler>();
        assert_is_delegate::<Handler>();
    }
}
