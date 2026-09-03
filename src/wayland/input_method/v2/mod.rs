//! Input method v2 protocol support.

use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, backend::GlobalId};

use wayland_protocols_misc::zwp_input_method_v2::server::{
    zwp_input_method_manager_v2::{self, ZwpInputMethodManagerV2},
    zwp_input_method_v2::ZwpInputMethodV2,
};

use crate::{
    input::{Seat, SeatHandler},
    wayland::{Dispatch2, GlobalData, GlobalDispatch2},
};

pub use input_method_handle::InputMethodUserData;
pub(crate) use input_method_handle::InputMethodV2Handle;

pub use input_method_keyboard_grab::{InputMethodKeyboardGrab, InputMethodKeyboardUserData};
pub use input_method_popup_surface::InputMethodPopupSurfaceUserData;
pub use input_method_popup_surface::PopupSurface;

use super::{InputMethodHandle, InputMethodHandler, InputMethodManagerGlobalData};
use crate::wayland::text_input::TextInputHandle;

const MANAGER_VERSION: u32 = 1;

/// The role of the input method popup (v2).
pub const INPUT_POPUP_SURFACE_ROLE: &str = "zwp_input_popup_surface_v2";

mod input_method_handle;
mod input_method_keyboard_grab;
mod input_method_popup_surface;

/// State of wp input method v2 protocol.
#[derive(Debug)]
pub struct InputMethodManagerState {
    global: GlobalId,
}

impl InputMethodManagerState {
    /// Initialize an input method manager global (v2).
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<ZwpInputMethodManagerV2, InputMethodManagerGlobalData>,
        D: Dispatch<ZwpInputMethodManagerV2, GlobalData>,
        D: Dispatch<ZwpInputMethodV2, InputMethodUserData<D>>,
        D: SeatHandler,
        D: 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let data = InputMethodManagerGlobalData::new(filter);
        let global = display.create_global::<D, ZwpInputMethodManagerV2, _>(MANAGER_VERSION, data);

        Self { global }
    }

    /// Get the id of the v2 manager global.
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

impl<D> GlobalDispatch2<ZwpInputMethodManagerV2, D> for InputMethodManagerGlobalData
where
    D: Dispatch<ZwpInputMethodManagerV2, GlobalData>,
    D: Dispatch<ZwpInputMethodV2, InputMethodUserData<D>>,
    D: SeatHandler,
    D: 'static,
{
    fn bind(
        &self,
        _: &mut D,
        _: &DisplayHandle,
        _: &Client,
        resource: New<ZwpInputMethodManagerV2>,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, GlobalData);
    }

    fn can_view(&self, client: &Client) -> bool {
        (self.filter)(client)
    }
}

impl<D> Dispatch2<ZwpInputMethodManagerV2, D> for GlobalData
where
    D: Dispatch<ZwpInputMethodV2, InputMethodUserData<D>>,
    D: SeatHandler + InputMethodHandler,
    D: 'static,
{
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        _: &ZwpInputMethodManagerV2,
        request: zwp_input_method_manager_v2::Request,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwp_input_method_manager_v2::Request::GetInputMethod { seat, input_method } => {
                let seat = Seat::<D>::from_resource(&seat).unwrap();

                let user_data = seat.user_data();
                user_data.insert_if_missing(TextInputHandle::default);
                user_data.insert_if_missing(InputMethodHandle::default);
                let input_method_handle = user_data.get::<InputMethodHandle>().unwrap();
                let text_input_handle = user_data.get::<TextInputHandle>().unwrap();
                text_input_handle.with_focused_text_input(|ti, surface| {
                    ti.enter(surface);
                });
                let keyboard_handle = seat.get_keyboard().unwrap();
                let instance = data_init.init(
                    input_method,
                    InputMethodUserData {
                        handle: input_method_handle.v2().clone(),
                        text_input_handle: text_input_handle.clone(),
                        keyboard_handle,
                        popup_geometry_callback: D::parent_geometry,
                        popup_repositioned: D::popup_repositioned,
                        new_popup: D::new_popup,
                        dismiss_popup: D::dismiss_popup,
                    },
                );
                input_method_handle.v2().add_instance(&instance);
            }
            zwp_input_method_manager_v2::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}
