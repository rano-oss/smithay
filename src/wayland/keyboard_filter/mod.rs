//! Keyboard filter protocol (IME key interception / passthrough).

mod keyboard_filter_handle;

pub use keyboard_filter_handle::KeyboardFilterUserData;

use std::{
    collections::{HashSet, VecDeque},
    sync::{Arc, Mutex},
};

use wayland_protocols::wp::{
    input_method::zv3::server::zwp_input_method_v3::ZwpInputMethodV3,
    keyboard_filter::zv1::server::{
        zwp_keyboard_filter_manager_v1::{self, ZwpKeyboardFilterManagerV1},
        zwp_keyboard_filter_v1::ZwpKeyboardFilterV1,
    },
};
use wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    backend::GlobalId,
    protocol::wl_keyboard::WlKeyboard,
};

use crate::{
    input::SeatHandler,
    wayland::{
        Dispatch2, GlobalDispatch2,
        input_method::InputMethodV3UserData,
        seat::{KeyboardUserData, WaylandFocus},
    },
};

const MANAGER_VERSION: u32 = 1;

/// Data associated with the global.
#[allow(missing_debug_implementations)]
pub struct KeyboardFilterManagerGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

/// Data accessible from ZwpKeyboardFilterManagerV1
#[derive(Debug)]
pub struct KeyboardFilterManagerUserData {
    inner: Arc<Mutex<KeyboardFilterManagerUserDataInner>>,
}

#[derive(Debug, Default)]
pub(crate) struct KeyboardFilterManagerUserDataInner {
    pub(crate) bound_keyboards: HashSet<WlKeyboard>,
    pub(crate) bound_ims: HashSet<ZwpInputMethodV3>,
}

/// State of the keyboard filter protocol.
#[derive(Debug)]
pub struct KeyboardFilterManagerState {
    global: GlobalId,
}

impl KeyboardFilterManagerState {
    /// Initialize a keyboard filter manager global.
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<ZwpKeyboardFilterManagerV1, KeyboardFilterManagerGlobalData>,
        D: Dispatch<ZwpKeyboardFilterManagerV1, KeyboardFilterManagerUserData>,
        D: SeatHandler,
        D: 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let data = KeyboardFilterManagerGlobalData {
            filter: Box::new(filter),
        };
        let global = display.create_global::<D, ZwpKeyboardFilterManagerV1, _>(MANAGER_VERSION, data);
        Self { global }
    }

    /// Get the id of manager global.
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

impl<D> GlobalDispatch2<ZwpKeyboardFilterManagerV1, D> for KeyboardFilterManagerGlobalData
where
    D: Dispatch<ZwpKeyboardFilterManagerV1, KeyboardFilterManagerUserData>,
    D: Dispatch<ZwpKeyboardFilterV1, KeyboardFilterUserData<D>>,
    D: SeatHandler,
    D: 'static,
{
    fn bind(
        &self,
        _: &mut D,
        _: &DisplayHandle,
        _: &Client,
        resource: New<ZwpKeyboardFilterManagerV1>,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(
            resource,
            KeyboardFilterManagerUserData {
                inner: Arc::new(Mutex::new(Default::default())),
            },
        );
    }

    fn can_view(&self, client: &Client) -> bool {
        (self.filter)(client)
    }
}

impl<D> Dispatch2<ZwpKeyboardFilterManagerV1, D> for KeyboardFilterManagerUserData
where
    D: Dispatch<ZwpKeyboardFilterV1, KeyboardFilterUserData<D>>,
    D: SeatHandler,
    <D as SeatHandler>::KeyboardFocus: WaylandFocus,
    D: 'static,
{
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        resource: &ZwpKeyboardFilterManagerV1,
        request: zwp_keyboard_filter_manager_v1::Request,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwp_keyboard_filter_manager_v1::Request::BindToInputMethod {
                keyboard,
                input_method,
                surface,
                extensions,
            } => {
                {
                    let bind = self.inner.lock().unwrap();
                    if bind.bound_keyboards.contains(&keyboard) || bind.bound_ims.contains(&input_method)
                    {
                        resource.post_error(
                            zwp_keyboard_filter_manager_v1::Error::AlreadyBound,
                            "keyboard or input method already bound",
                        );
                        return;
                    }
                }

                let imdata = input_method.data::<InputMethodV3UserData<D>>().unwrap();
                let keyboard_data = keyboard.data::<KeyboardUserData<D>>().unwrap();
                let kb_handle = keyboard_data
                    .handle
                    .as_ref()
                    .expect("Seat doesn't support keyboard");

                if !Arc::ptr_eq(&kb_handle.arc, &imdata.keyboard_handle.arc) {
                    resource.post_error(
                        zwp_keyboard_filter_manager_v1::Error::WrongSeat,
                        "The keyboard is attached to a different seat than the input method",
                    );
                    return;
                }

                let pending_events = Arc::new(Mutex::new(VecDeque::new()));
                let filter_udata = KeyboardFilterUserData {
                    keyboard_handle: kb_handle.clone(),
                    pending_events,
                    focused_surface: Mutex::new(None),
                    manager_data: self.inner.clone(),
                    bound_keyboard: keyboard.clone(),
                    bound_input_method: input_method.clone(),
                    im_surface: surface,
                    client_held_keys: Mutex::new(HashSet::new()),
                };

                let keyboard_filter = data_init.init::<ZwpKeyboardFilterV1, _>(extensions, filter_udata);

                // Late bind: focus may already exist before the filter is created.
                if let Some(focus) = imdata.text_input_handle.focus() {
                    keyboard_filter
                        .data::<KeyboardFilterUserData<D>>()
                        .unwrap()
                        .activate_interceptor(&focus);
                }

                *imdata.keyboard_filter.lock().unwrap() = Some(keyboard_filter);
                let mut bind = self.inner.lock().unwrap();
                bind.bound_keyboards.insert(keyboard);
                bind.bound_ims.insert(input_method);
            }
            zwp_keyboard_filter_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}
