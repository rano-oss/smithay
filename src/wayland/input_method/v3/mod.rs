//! Experimental xx-input-method protocol support (formerly zwp_input_method_v3).

use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, backend::GlobalId};

use crate::wayland::{Dispatch2, GlobalData, GlobalDispatch2};

use wayland_protocols_experimental::input_method::v1::server::{
    xx_input_method_manager_v2::{self, XxInputMethodManagerV2},
    xx_input_method_v1::XxInputMethodV1,
    xx_input_popup_positioner_v1::XxInputPopupPositionerV1,
    xx_input_popup_surface_v2::XxInputPopupSurfaceV2,
};

use crate::input::{Seat, SeatHandler};

pub(crate) use input_method_handle::{InputMethodUserData, InputMethodV3Handle};

use super::{InputMethodHandle, InputMethodHandler, InputMethodManagerGlobalData};
use crate::wayland::text_input::TextInputHandle;

const MANAGER_VERSION: u32 = 4;

/// The role of the input method popup (xx).
pub const INPUT_POPUP_SURFACE_ROLE: &str = "xx_input_popup_surface_v2";

mod configure_tracker;
mod input_method_handle;
mod input_method_popup_surface;
mod positioner;

pub use input_method_popup_surface::{InputMethodPopupSurfaceUserData, PopupSurface, PopupSurfaceState};
pub use positioner::{PositionerState, PositionerUserData};

/// State of xx input method protocol.
#[derive(Debug)]
pub struct InputMethodManagerState {
    global: GlobalId,
}

impl InputMethodManagerState {
    /// Initialize an input method manager global (xx).
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<XxInputMethodManagerV2, InputMethodManagerGlobalData>,
        D: Dispatch<XxInputMethodManagerV2, GlobalData>,
        D: Dispatch<XxInputMethodV1, InputMethodUserData<D>>,
        D: Dispatch<XxInputPopupSurfaceV2, InputMethodPopupSurfaceUserData>,
        D: Dispatch<XxInputPopupPositionerV1, PositionerUserData>,
        D: SeatHandler,
        D: 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let data = InputMethodManagerGlobalData::new(filter);
        let global = display.create_global::<D, XxInputMethodManagerV2, _>(MANAGER_VERSION, data);

        Self { global }
    }

    /// Get the id of the manager global.
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

impl<D> GlobalDispatch2<XxInputMethodManagerV2, D> for InputMethodManagerGlobalData
where
    D: Dispatch<XxInputMethodManagerV2, GlobalData>,
    D: Dispatch<XxInputMethodV1, InputMethodUserData<D>>,
    D: Dispatch<XxInputPopupSurfaceV2, InputMethodPopupSurfaceUserData>,
    D: Dispatch<XxInputPopupPositionerV1, PositionerUserData>,
    D: SeatHandler,
    D: 'static,
{
    fn bind(
        &self,
        _: &mut D,
        _: &DisplayHandle,
        _: &Client,
        resource: New<XxInputMethodManagerV2>,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, GlobalData);
    }

    fn can_view(&self, client: &Client) -> bool {
        (self.filter)(client)
    }
}

impl<D> Dispatch2<XxInputMethodManagerV2, D> for GlobalData
where
    D: Dispatch<XxInputMethodV1, InputMethodUserData<D>>,
    D: Dispatch<XxInputPopupPositionerV1, PositionerUserData>,
    D: SeatHandler + InputMethodHandler,
    D: 'static,
{
    fn request(
        &self,
        state: &mut D,
        client: &Client,
        _: &XxInputMethodManagerV2,
        request: xx_input_method_manager_v2::Request,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            xx_input_method_manager_v2::Request::GetInputMethod { seat, input_method } => {
                let seat = Seat::<D>::from_resource(&seat).unwrap();
                let user_data = seat.user_data();
                user_data.insert_if_missing(TextInputHandle::default);
                user_data.insert_if_missing(InputMethodHandle::default);
                let input_method_handle = user_data.get::<InputMethodHandle>().unwrap();
                let text_input_handle = user_data.get::<TextInputHandle>().unwrap();
                let instance = data_init.init(
                    input_method,
                    InputMethodUserData {
                        handle: input_method_handle.v3.clone(),
                        text_input_handle: text_input_handle.clone(),
                        keyboard_handle: seat.get_keyboard().unwrap(),
                        keyboard_filter: Default::default(),
                        dismiss_popup: D::dismiss_popup,
                    },
                );
                let app_id = match state.input_method_app_id(client, dh) {
                    Some(id) => id,
                    None => {
                        tracing::warn!(
                            "Input method client has no app_id (no security context?), rejecting registration"
                        );
                        instance.unavailable();
                        return;
                    }
                };

                input_method_handle.v3.add_instance(&instance, app_id.clone());
                // Enter before compositor policy so sync_activation sees text-input focus.
                text_input_handle.enter();
                // Compositor selects the active instance (layout policy); we activate once.
                state.input_method_instance_registered(&seat, &app_id);
                input_method_handle.sync_activation(state, &seat);
            }
            xx_input_method_manager_v2::Request::GetPositioner { id } => {
                data_init.init(id, PositionerUserData::default());
            }
            xx_input_method_manager_v2::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}
