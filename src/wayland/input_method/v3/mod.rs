//! Input method v3 protocol support.

use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, backend::GlobalId};

use crate::wayland::{Dispatch2, GlobalData, GlobalDispatch2};

use wayland_protocols::wp::input_method::zv3::server::{
    zwp_input_method_manager_v3::{self, ZwpInputMethodManagerV3},
    zwp_input_method_v3::ZwpInputMethodV3,
    zwp_input_popup_positioner_v3::ZwpInputPopupPositionerV3,
    zwp_input_popup_surface_v3::ZwpInputPopupSurfaceV3,
};

use crate::input::{Seat, SeatHandler};

pub(crate) use input_method_handle::{InputMethodUserData, V3InputMethodHandle};

use super::{InputMethodHandle, InputMethodHandler};
use crate::wayland::text_input::TextInputHandle;

const MANAGER_VERSION: u32 = 2;

/// The role of the input method popup (v3).
pub const INPUT_POPUP_SURFACE_ROLE: &str = "zwp_input_popup_surface_v3";

mod configure_tracker;
mod input_method_handle;
mod input_method_popup_surface;
mod positioner;

pub use input_method_popup_surface::{InputMethodPopupSurfaceUserData, PopupSurface, PopupSurfaceState};
pub use positioner::{PositionerState, PositionerUserData};

/// Data associated with an InputMethodManager global (v3).
#[allow(missing_debug_implementations)]
pub struct InputMethodManagerGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

/// State of wp input method v3 protocol.
#[derive(Debug)]
pub struct InputMethodManagerState {
    global: GlobalId,
}

impl InputMethodManagerState {
    /// Initialize an input method manager global (v3).
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<ZwpInputMethodManagerV3, InputMethodManagerGlobalData>,
        D: Dispatch<ZwpInputMethodManagerV3, GlobalData>,
        D: Dispatch<ZwpInputMethodV3, InputMethodUserData<D>>,
        D: Dispatch<ZwpInputPopupSurfaceV3, InputMethodPopupSurfaceUserData>,
        D: Dispatch<ZwpInputPopupPositionerV3, PositionerUserData>,
        D: SeatHandler,
        D: 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let data = InputMethodManagerGlobalData {
            filter: Box::new(filter),
        };
        let global = display.create_global::<D, ZwpInputMethodManagerV3, _>(MANAGER_VERSION, data);

        Self { global }
    }

    /// Get the id of the v3 manager global.
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

impl<D> GlobalDispatch2<ZwpInputMethodManagerV3, D> for InputMethodManagerGlobalData
where
    D: Dispatch<ZwpInputMethodManagerV3, GlobalData>,
    D: Dispatch<ZwpInputMethodV3, InputMethodUserData<D>>,
    D: Dispatch<ZwpInputPopupSurfaceV3, InputMethodPopupSurfaceUserData>,
    D: Dispatch<ZwpInputPopupPositionerV3, PositionerUserData>,
    D: SeatHandler,
    D: 'static,
{
    fn bind(
        &self,
        _: &mut D,
        _: &DisplayHandle,
        _: &Client,
        resource: New<ZwpInputMethodManagerV3>,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, GlobalData);
    }

    fn can_view(&self, client: &Client) -> bool {
        (self.filter)(client)
    }
}

impl<D> Dispatch2<ZwpInputMethodManagerV3, D> for GlobalData
where
    D: Dispatch<ZwpInputMethodV3, InputMethodUserData<D>>,
    D: Dispatch<ZwpInputPopupPositionerV3, PositionerUserData>,
    D: SeatHandler + InputMethodHandler,
    D: 'static,
{
    fn request(
        &self,
        state: &mut D,
        client: &Client,
        _: &ZwpInputMethodManagerV3,
        request: zwp_input_method_manager_v3::Request,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwp_input_method_manager_v3::Request::GetInputMethod { seat, input_method } => {
                let seat = Seat::<D>::from_resource(&seat).unwrap();

                let user_data = seat.user_data();
                user_data.insert_if_missing(TextInputHandle::default);
                user_data.insert_if_missing(InputMethodHandle::default);
                let input_method_handle = user_data.get::<InputMethodHandle>().unwrap();
                let text_input_handle = user_data.get::<TextInputHandle>().unwrap();

                let app_id = match state.input_method_app_id(client, dh) {
                    Some(id) => id,
                    None => {
                        tracing::warn!(
                            "Input method client has no app_id (no security context?), rejecting registration"
                        );
                        let instance = data_init.init(
                            input_method,
                            InputMethodUserData {
                                seat: seat.clone(),
                                handle: input_method_handle.v3().clone(),
                                text_input_handle: text_input_handle.clone(),
                                keyboard_handle: seat.get_keyboard().unwrap(),
                                keyboard_filter: Default::default(),
                                dismiss_popup: D::dismiss_popup,
                            },
                        );
                        instance.unavailable();
                        return;
                    }
                };

                text_input_handle.enter();
                let instance = data_init.init(
                    input_method,
                    InputMethodUserData {
                        seat: seat.clone(),
                        handle: input_method_handle.v3().clone(),
                        text_input_handle: text_input_handle.clone(),
                        keyboard_handle: seat.get_keyboard().unwrap(),
                        keyboard_filter: Default::default(),
                        dismiss_popup: D::dismiss_popup,
                    },
                );
                input_method_handle.v3().add_instance(&instance, app_id);
                state.input_method_instance_registered();
            }
            zwp_input_method_manager_v3::Request::GetPositioner { id } => {
                data_init.init(id, PositionerUserData::default());
            }
            zwp_input_method_manager_v3::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}
