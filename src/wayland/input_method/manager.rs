use std::sync::Arc;

use wayland_protocols_experimental::input_method::v1::server::{
    xx_input_method_manager_v2::XxInputMethodManagerV2, xx_input_method_v1::XxInputMethodV1,
    xx_input_popup_positioner_v1::XxInputPopupPositionerV1, xx_input_popup_surface_v2::XxInputPopupSurfaceV2,
};
use wayland_protocols_misc::zwp_input_method_v2::server::{
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2, zwp_input_method_v2::ZwpInputMethodV2,
};
use wayland_server::{Client, Dispatch, DisplayHandle, GlobalDispatch};

use crate::{input::SeatHandler, wayland::GlobalData};

use super::v2::{InputMethodManagerState as V2Manager, InputMethodUserData as V2UserData};
use super::v3::{
    InputMethodManagerState as V3Manager, InputMethodPopupSurfaceUserData, InputMethodUserData as V3UserData,
    PositionerUserData,
};

/// Data associated with an input method manager global.
#[allow(missing_debug_implementations)]
pub struct InputMethodManagerGlobalData {
    pub(crate) filter: Arc<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

impl InputMethodManagerGlobalData {
    pub(crate) fn new(filter: Arc<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>) -> Self {
        Self { filter }
    }
}

/// Wrapper that sets up all input-method manager globals smithay supports.
///
/// Call [`Self::new`] once from the compositor; clients bind the protocol they speak.
/// Seat-level state is shared via [`super::InputMethodHandle`].
#[derive(Debug)]
pub struct InputMethodManagerState {
    _zwp: V2Manager,
    _xx: V3Manager,
}

impl InputMethodManagerState {
    /// Create input-method manager globals with a shared client filter.
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<ZwpInputMethodManagerV2, InputMethodManagerGlobalData>,
        D: Dispatch<ZwpInputMethodManagerV2, GlobalData>,
        D: Dispatch<ZwpInputMethodV2, V2UserData<D>>,
        D: GlobalDispatch<XxInputMethodManagerV2, InputMethodManagerGlobalData>,
        D: Dispatch<XxInputMethodManagerV2, GlobalData>,
        D: Dispatch<XxInputMethodV1, V3UserData<D>>,
        D: Dispatch<XxInputPopupSurfaceV2, InputMethodPopupSurfaceUserData>,
        D: Dispatch<XxInputPopupPositionerV1, PositionerUserData>,
        D: SeatHandler,
        D: 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let filter: Arc<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync> = Arc::new(filter);
        Self {
            _zwp: V2Manager::new::<D>(display, filter.clone()),
            _xx: V3Manager::new::<D>(display, filter),
        }
    }
}
