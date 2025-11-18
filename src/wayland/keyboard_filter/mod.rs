//! Filtering key presses
//!
//! This Wayland protocol allows an application to register itself as a filter for key presses destined for another application.
//!
//! The current implementation supports filtering for input method purposes.

use std::{collections::HashSet, sync::{Arc, Mutex}};

use tracing::{warn, error, debug};

use wayland_client::WEnum;
use wayland_server::{backend::GlobalId, protocol::{wl_keyboard::WlKeyboard, wl_surface::WlSurface}, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};
use wl_input_method::{input_method::v1::server::xx_input_method_v1::XxInputMethodV1, keyboard_filter::v1::server::{xx_keyboard_filter_manager_v1::{self, XxKeyboardFilterManagerV1}, xx_keyboard_filter_v1::{self, FilterAction, XxKeyboardFilterV1}}};

use crate::{input::{keyboard::KeyboardHandle, SeatHandler}, utils::Serial, wayland::input_method_v3::InputMethodUserData};

mod queue;

pub(crate) use queue::DispatchQueue;

const MANAGER_VERSION: u32 = 1;

/// Data associated with the global.
#[allow(missing_debug_implementations)]
pub struct KeyboardFilterManagerGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

/// Data accesible from XxKeyboardFilterManagerV1
#[derive(Debug, Default)]
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
        //D: Dispatch<XxInputMethodV1, InputMethodUserData<D>>,
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


impl<D> GlobalDispatch<XxKeyboardFilterManagerV1, KeyboardFilterManagerGlobalData, D> for KeyboardFilterManagerState
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
        data_init.init(resource, KeyboardFilterManagerUserData::default());
    }

    fn can_view(client: Client, global_data: &KeyboardFilterManagerGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<XxKeyboardFilterManagerV1, KeyboardFilterManagerUserData, D> for KeyboardFilterManagerState
where
    D: Dispatch<XxKeyboardFilterManagerV1, KeyboardFilterManagerUserData>,
    D: Dispatch<XxKeyboardFilterV1, KeyboardFilterUserData<D>>,
    D: SeatHandler,
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
            xx_keyboard_filter_manager_v1::Request::BindToInputMethod { keyboard, input_method, surface, extensions } => {
                {
                    let bind = data.inner.lock().unwrap();
                    if bind.bound_keyboards.contains(&keyboard) {
                        resource.post_error(
                            xx_keyboard_filter_manager_v1::Error::AlreadyBound,
                            format!("WlKeyboard {keyboard:?} already bound"),
                        );
                        return
                    };
                    if bind.bound_ims.contains(&input_method) {
                        resource.post_error(
                            xx_keyboard_filter_manager_v1::Error::AlreadyBound,
                            format!("Input method {input_method:?} already bound"),
                        );
                        return;
                    };
                }
                let filter = data_init.init::<XxKeyboardFilterV1, _>(
                    extensions,
                    KeyboardFilterUserData {
                        manager_data: data.inner.clone(),
                      //  keyboard_handle: data.keyboard_handle.clone(),
                        keyboard_handle: None,
                        bound_keyboard: keyboard.clone(),
                        bound_input_method: input_method.clone(),
                    },
                );
                {
                    let mut bind = data.inner.lock().unwrap();
                    bind.bound_keyboards.insert(keyboard);
                    bind.bound_ims.insert(input_method);
                }
                
/*                let mut input_method = input_method.data::<InputMethodUserData<D>>().unwrap();
                    instance.bound_keyboard = Some(Filter {
                        im_keyboard: keyboard,
                        filter,
                        im_surface: surface,
                    });*/
                // FIXME: if IM already active, register the filter as keyboard interceptor
            },
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
            $crate::reexports::wayland_protocols_experimental::keyboard_filter::v1::server::xx_keyboard_filter_manager_v1::XxKeyboardFilterManagerV1: KeyboardFilterManagerUserData
        ] => $crate::wayland::keyboard_filter::KeyboardFilterManagerState);
        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::reexports::wayland_protocols_experimental::keyboard_filter::v1::server::xx_keyboard_filter_v1::XxKeyboardFilterV1: $crate::wayland::keyboard_filter::KeyboardFilterUserData<Self>
        ] => $crate::wayland::keyboard_filter::KeyboardFilterManagerState);
    }
}
/*
impl<D> Dispatch<XxKeyboardFilterManagerV1, InputMethodUserData<D>, D> for InputMethodManagerState
where
    D: Dispatch<XxInputMethodV1, InputMethodUserData<D>>,
    //D: Dispatch<XxInputMethodKeyboardV1, KeyboardUserData<D>>,
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
            Request::KeyboardBind { keyboard, surface, extensions } => {
                
            },*/


/// Carries data assigned in a ::bind request.
#[derive(Debug, Clone)]
pub(crate) struct Filter {
    /// Keyboard provided by the filter client to sniff on target surface's events.
    pub im_keyboard: WlKeyboard,
    /// Keyboard filter assigned to this keyboard instance
    pub filter: XxKeyboardFilterV1,
    /// Surface on the filterer side which should be target of enter and leave
    pub im_surface: WlSurface,
}

/// Accessible through XxKeyboardFilterV1 instance.
#[derive(Debug)]
pub struct KeyboardFilterUserData<D: SeatHandler> {
    //pub(crate) keyboard_handle: KeyboardHandle<D>,
    pub(crate) keyboard_handle: Option<KeyboardHandle<D>>,
    manager_data: Arc<Mutex<KeyboardFilterManagerUserDataInner>>,
    bound_keyboard: WlKeyboard,
    bound_input_method: XxInputMethodV1,
}

fn get_filter<'a>() -> &'a queue::DispatchQueue {
    panic!();
}

impl<D> Dispatch<XxKeyboardFilterV1, KeyboardFilterUserData<D>, D> for KeyboardFilterManagerState
where
    D: Dispatch<XxKeyboardFilterV1, KeyboardFilterUserData<D>>,
    D: SeatHandler,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        resource: &XxKeyboardFilterV1,
        request: <XxKeyboardFilterV1 as Resource>::Request,
        data: &KeyboardFilterUserData<D>,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        /*
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
        */
        let filter = get_filter();
        
        use xx_keyboard_filter_v1::Request;
        match request {
            Request::Unbind => {
                for e in filter.events_to_filter.lock().unwrap().drain(..) {
                    filter.apply(e)
                }
                let mut mgr = data.manager_data.lock().unwrap();
                mgr.bound_keyboards.remove(&data.bound_keyboard);
                mgr.bound_ims.remove(&data.bound_input_method);
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
                            resource.post_error(xx_keyboard_filter_v1::Error::InvalidSerial, "Filter serial newer than awaited");
                            return;
                        };
                        (action, true)
                    } else {
                        // Events without a serial will not get a confirmation. Just pass them through and go to next event.
                        (Action::Passthrough, false)
                    };
                    if e.1.is_filterable() {
                        if let Action::Passthrough = action {
                            filter.apply(e)
                        }
                    } else {
                        /*resource.post_error(
                            xx_keyboard_filter_v1::Error::InvalidFilterAction,
                            format!("Only key events may be filtered, but tried {}", e.1.describe())
                        );*/
                        error!("FIXME: Wrong event to filter");
                        return;
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
        fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&Self::KeyboardFocus>) {
        }
        fn cursor_image(&mut self, _seat: &Seat<Self>, _image: crate::input::pointer::CursorImageStatus) {
        }
        fn led_state_changed(&mut self, _seat: &Seat<Self>, _led_state: crate::input::keyboard::LedState) {
        }
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
        T: wayland_server::Dispatch<protocol::xx_keyboard_filter_v1::XxKeyboardFilterV1, KeyboardFilterUserData<T>>,
    {
    }


    #[test]
    fn test_valid_assignment() {
        assert_is_manager_delegate::<Handler>();
        assert_is_delegate::<Handler>();
    }
}
