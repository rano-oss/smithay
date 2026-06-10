use std::cmp::PartialEq;
use std::sync::{Arc, Mutex};

use wayland_protocols_experimental::input_method::v1::server::xx_input_method_v1::XxInputMethodV1;
use wayland_protocols_experimental::input_method::v1::server::xx_input_popup_surface_v2::{
    self, XxInputPopupSurfaceV2,
};
use wayland_server::{backend::ClientId, protocol::wl_surface::WlSurface, Resource};

use crate::wayland::Dispatch2;

use crate::input::SeatHandler;
use crate::utils::{
    alive_tracker::{AliveTracker, IsAlive},
    Logical, Point, Rectangle, Serial,
};

use super::{
    configure_tracker::ConfigureTracker,
    positioner::{PositionerState, PositionerUserData},
    InputMethodHandler, InputMethodUserData,
};

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct ImPopupLocation {
    /// Area for the positioner, relative to parent
    pub anchor: Rectangle<i32, Logical>,
    /// Geometry of the popup surface relative to parent.
    pub geometry: Rectangle<i32, Logical>,
}

/// A handle to an input method popup surface
#[derive(Debug, Clone)]
pub struct PopupSurface {
    /// The popup surface role object.
    pub surface_role: XxInputPopupSurfaceV2,
    surface: WlSurface,
    /// Parent surface (text input surface). Doesn't change within popup lifetime.
    parent: PopupParent,
    configure_tracker: Arc<Mutex<ConfigureTracker<PopupSurfaceState>>>,
    /// Compositor-assigned state acknowledged by client.
    state: Arc<Mutex<PopupSurfaceState>>,
    /// Compositor-assigned state, not sent to client yet
    state_pending: Option<PopupSurfaceState>,
}

impl PopupSurface {
    /// Creates a new popup surface.
    /// Anchor is the anchor position relative to parent. Geometry is the popup position relative to parent.
    pub(crate) fn new(
        init: impl FnOnce(InputMethodPopupSurfaceUserData) -> XxInputPopupSurfaceV2,
        input_method: XxInputMethodV1,
        parent: PopupParent,
        surface: WlSurface,
        anchor: Rectangle<i32, Logical>,
        geometry: Rectangle<i32, Logical>,
        positioner_data: PositionerState,
    ) -> Self {
        let configure_tracker = Arc::new(Mutex::new(Default::default()));
        let state = Arc::new(Mutex::new(PopupSurfaceState::default()));

        let instance = InputMethodPopupSurfaceUserData::new(
            input_method.clone(),
            surface.clone(),
            configure_tracker.clone(),
            state.clone(),
            Mutex::new(positioner_data),
        );
        let surface_role = init(instance);
        Self {
            surface_role,
            configure_tracker,
            state,
            state_pending: Some(PopupSurfaceState {
                position: ImPopupLocation { anchor, geometry },
                configured: false,
                repositioned: None,
            }),
            surface,
            parent,
        }
    }

    /// Returns a copy of the positioner. That can be used to calculate a new position.
    pub fn positioner(&self) -> PositionerState {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        *role_data.positioner.lock().unwrap()
    }

    /// Returns whether the popup position is frozen.
    /// When frozen, cursor_rectangle changes do not reposition the popup.
    pub fn frozen(&self) -> bool {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        *role_data.frozen.lock().unwrap()
    }

    /// Is the input method popup surface referred by this handle still alive?
    #[inline]
    pub fn alive(&self) -> bool {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        self.surface.alive() && role_data.alive_tracker.alive()
    }

    /// Access to the underlying `wl_surface` of this popup
    #[inline]
    pub fn wl_surface(&self) -> &WlSurface {
        &self.surface
    }

    /// Get the parent surface info.
    pub fn get_parent(&self) -> &PopupParent {
        &self.parent
    }

    /// Get the input method controlling this popup.
    pub fn input_method(&self) -> &XxInputMethodV1 {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        &role_data.input_method
    }

    /// Location of the popup relative to parent surface.
    pub fn location(&self) -> Point<i32, Logical> {
        self.state.lock().unwrap().position.geometry.loc
    }

    /// `true` if an initial configure has been sent.
    pub fn is_initial_configure_sent(&self) -> bool {
        self.state.lock().unwrap().configured
    }

    /// Set position information for pending configure.
    pub fn set_position(&mut self, position: ImPopupLocation) {
        self.ensure_pending().position = position;
    }

    /// Adds the repositioned token to pending state.
    pub fn set_repositioned(&mut self, token: u32) {
        self.ensure_pending().repositioned = Some(token);
    }

    fn ensure_pending(&mut self) -> &mut PopupSurfaceState {
        self.state_pending
            .get_or_insert_with(|| self.state.lock().unwrap().clone())
    }

    /// Send a configure event to this popup surface.
    ///
    /// The serial of this configure will be tracked waiting for the client to ACK it.
    pub fn send_pending_configure(&mut self) {
        let Some(pending) = self.state_pending.as_mut() else {
            return;
        };
        pending.configured = true;
        let new_state = pending.clone();

        let sent_state = {
            let tracker = self.configure_tracker.lock().unwrap();
            tracker.last_pending_state().cloned()
        }
        .unwrap_or_else(|| self.state.lock().unwrap().clone());

        if new_state != sent_state {
            let mut tracker = self.configure_tracker.lock().unwrap();
            let serial = tracker.assign_serial(new_state.clone());

            let ImPopupLocation { anchor, geometry } = new_state.position.clone();
            let relative_to_popup = anchor.loc - geometry.loc;
            self.surface_role.start_configure(
                geometry.size.w as u32,
                geometry.size.h as u32,
                relative_to_popup.x,
                relative_to_popup.y,
                anchor.size.w as u32,
                anchor.size.h as u32,
                serial.into(),
            );

            if let (Some(new), sent) = (new_state.repositioned, sent_state.repositioned) {
                if Some(new) != sent {
                    self.surface_role.repositioned(new);
                }
            }
        }
    }
}

impl PartialEq for PopupSurface {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.surface_role == other.surface_role
    }
}

/// Compositor-defined state
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct PopupSurfaceState {
    /// Positioning information
    position: ImPopupLocation,
    /// Token to send to the client, if any
    ///
    /// The protocol doesn't mandate the lifecycle for this token, so this holds the last state and update events are sent on detected changes.
    repositioned: Option<u32>,
    /// Already issued a configure sequence
    configured: bool,
}

/// Parent surface and location for the IME popup.
#[derive(Debug, Clone)]
pub struct PopupParent {
    /// The surface over which the IME popup is shown.
    pub surface: WlSurface,
    /// The location of the parent surface.
    pub location: Rectangle<i32, Logical>,
}

/// Data accessible from XxInputPopupSurfaceV2 object
#[derive(Debug)]
pub struct InputMethodPopupSurfaceUserData {
    input_method: XxInputMethodV1,
    pub(super) alive_tracker: AliveTracker,
    pub(super) surface: WlSurface,
    pub(super) configure_tracker: Arc<Mutex<ConfigureTracker<PopupSurfaceState>>>,
    pub(super) state: Arc<Mutex<PopupSurfaceState>>,
    pub(super) positioner: Mutex<PositionerState>,
    pub(super) frozen: Mutex<bool>,
    pub(super) frozen_cursor_rectangle: Mutex<Option<Rectangle<i32, Logical>>>,
}

impl InputMethodPopupSurfaceUserData {
    fn new(
        input_method: XxInputMethodV1,
        surface: WlSurface,
        configure_tracker: Arc<Mutex<ConfigureTracker<PopupSurfaceState>>>,
        popup_state: Arc<Mutex<PopupSurfaceState>>,
        positioner: Mutex<PositionerState>,
    ) -> Self {
        Self {
            input_method,
            alive_tracker: AliveTracker::default(),
            surface,
            configure_tracker,
            state: popup_state,
            positioner,
            frozen: Mutex::new(false),
            frozen_cursor_rectangle: Mutex::new(None),
        }
    }
}

impl<D> Dispatch2<XxInputPopupSurfaceV2, D> for InputMethodPopupSurfaceUserData
where
    D: InputMethodHandler + SeatHandler,
{
    fn request(
        &self,
        state: &mut D,
        _client: &wayland_server::Client,
        popup: &XxInputPopupSurfaceV2,
        request: xx_input_popup_surface_v2::Request,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, D>,
    ) {
        use xx_input_popup_surface_v2::Request;
        match request {
            Request::AckConfigure { serial } => {
                let surface = &self.surface;

                let serial = Serial::from(serial);
                let Some(client_state) = self.configure_tracker.lock().unwrap().ack_serial(serial) else {
                    popup.post_error(
                        xx_input_popup_surface_v2::Error::InvalidSerial,
                        format!("Serial {} is not awaiting ack", <u32>::from(serial)),
                    );
                    return;
                };
                *self.state.lock().unwrap() = client_state.clone();
                state.popup_ack_configure(surface, serial, client_state);
            }
            Request::Reposition { positioner, token } => {
                let im: &InputMethodUserData<D> = self.input_method.data().unwrap();
                let popup = {
                    let positioner: &PositionerUserData = positioner.data().unwrap();
                    let positioner = *positioner.inner.lock().unwrap();
                    let mut inner = im.handle.inner.lock().unwrap();
                    let active_id = inner.active_input_method_id.clone().unwrap();
                    let instance = inner
                        .instances
                        .iter_mut()
                        .find(|i| i.object.id() == active_id)
                        .unwrap();
                    let cursor = self
                        .frozen_cursor_rectangle
                        .lock()
                        .unwrap()
                        .unwrap_or(instance.cursor_rectangle);
                    let popup = instance
                        .popup_handles
                        .iter_mut()
                        .find(|h| h.surface_role == *popup)
                        .expect("This popup not tracked by its input method");
                    let parent_surface = popup.get_parent().surface.clone();
                    let popup_geometry = state.popup_geometry(&parent_surface, &cursor, &positioner);
                    *self.positioner.lock().unwrap() = positioner;
                    popup.set_repositioned(token);
                    popup.set_position(ImPopupLocation {
                        anchor: cursor,
                        geometry: popup_geometry,
                    });
                    popup.clone()
                };
                state.popup_repositioned(popup);
                im.handle.done();
            }
            Request::SetFrozen { frozen } => {
                let is_frozen = frozen != 0;
                *self.frozen.lock().unwrap() = is_frozen;
                if is_frozen {
                    let im: &InputMethodUserData<D> = self.input_method.data().unwrap();
                    let inner = im.handle.inner.lock().unwrap();
                    if let Some(active_id) = &inner.active_input_method_id {
                        if let Some(instance) = inner.instances.iter().find(|i| i.object.id() == *active_id) {
                            *self.frozen_cursor_rectangle.lock().unwrap() = Some(instance.cursor_rectangle);
                        }
                    }
                } else {
                    *self.frozen_cursor_rectangle.lock().unwrap() = None;
                }
            }
            Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(&self, _state: &mut D, _client: ClientId, _object: &XxInputPopupSurfaceV2) {
        self.alive_tracker.destroy_notify();
    }
}
