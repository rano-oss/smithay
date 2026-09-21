use std::cmp::PartialEq;
use std::sync::{Arc, Mutex};

use wayland_protocols::wp::input_method::zv3::server::zwp_input_method_v3::ZwpInputMethodV3;
use wayland_protocols::wp::input_method::zv3::server::zwp_input_popup_surface_v3::{
    self, PopupPositionMode, ZwpInputPopupSurfaceV3,
};
use wayland_server::{Resource, backend::ClientId, protocol::wl_surface::WlSurface};

use crate::input::SeatHandler;
use crate::utils::{
    Logical, Point, Rectangle, Serial,
    alive_tracker::{AliveTracker, IsAlive},
};
use crate::wayland::Dispatch2;

use super::super::PopupParent;
use super::{
    InputMethodHandler, InputMethodUserData,
    configure_tracker::PopupConfigureAttributes,
    positioner::{PositionerState, PositionerUserData},
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PopupLocation {
    /// Area for the positioner, relative to parent
    pub anchor: Rectangle<i32, Logical>,
    /// Geometry of the popup surface relative to parent.
    pub geometry: Rectangle<i32, Logical>,
}

#[derive(Debug, Clone)]
pub struct PopupSurface {
    /// The surface role for the input method popup
    pub surface_role: ZwpInputPopupSurfaceV3,
    /// Surface containing the popup
    surface: WlSurface,
    /// Surface containing the text input. This surface doesn't change within the lifetime of the popup.
    parent: PopupParent,
    /// Tracks configures and serials
    configure: Arc<Mutex<PopupConfigureAttributes>>,
    /// The compositor-assigned state acknowledged by client.
    acked_state: Arc<Mutex<PopupSurfaceState>>,
    pub(crate) position_mode: PopupPositionMode,
    pub(crate) anchored_cursor_rectangle: Option<Rectangle<i32, Logical>>,
    /// Waiting for text-input cursor_rectangle while the IME preedit caret is at
    /// 0. Cleared (freeze) when the IME moves the caret away from 0.
    pub(crate) awaiting_anchor: bool,
}

impl PopupSurface {
    /// Creates a new popup surface.
    /// Anchor is the anchor position relative to parent. Geometry is the popup position relative to parent.
    pub(crate) fn new(
        init: impl FnOnce(InputMethodPopupSurfaceUserData) -> ZwpInputPopupSurfaceV3,
        input_method: ZwpInputMethodV3,
        parent: PopupParent,
        surface: WlSurface,
        anchor: Rectangle<i32, Logical>,
        geometry: Rectangle<i32, Logical>,
        positioner_data: PositionerState,
    ) -> Self {
        let configure = Arc::new(Mutex::new(PopupConfigureAttributes::with_server_pending(
            PopupSurfaceState {
                position: PopupLocation { anchor, geometry },
                configured: false,
                repositioned: None,
            },
        )));
        let acked_state = Arc::new(Mutex::new(PopupSurfaceState::default()));

        let instance = InputMethodPopupSurfaceUserData::new(
            input_method.clone(),
            surface.clone(),
            configure.clone(),
            acked_state.clone(),
            Mutex::new(positioner_data),
        );
        let surface_role = init(instance);
        Self {
            surface_role,
            configure,
            acked_state,
            surface,
            parent,
            position_mode: PopupPositionMode::FollowCursor,
            anchored_cursor_rectangle: None,
            awaiting_anchor: false,
        }
    }

    /// Returns a copy of the positioner. That can be used to calculate a new position.
    pub fn positioner(&self) -> PositionerState {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        *role_data.positioner.lock().unwrap()
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

    /// Access to the parent surface associated with this popup
    pub fn get_parent(&self) -> &PopupParent {
        &self.parent
    }

    /// Access the input method using this popup
    pub fn input_method(&self) -> &ZwpInputMethodV3 {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        &role_data.input_method
    }

    /// Used to access the location of an input popup surface relative to the parent.
    ///
    /// Returns the latest compositor-side position, including configures that have
    /// been sent but not yet acknowledged by the IME client.
    pub fn location(&self) -> Point<i32, Logical> {
        self.configure
            .lock()
            .unwrap()
            .current_server_state()
            .position
            .geometry
            .loc
    }

    /// Anchor rectangle relative to the parent surface (acked configure state).
    pub fn anchor_rectangle(&self) -> Rectangle<i32, Logical> {
        self.acked_state.lock().unwrap().position.anchor
    }

    /// `true` if the surface sent a
    /// configure sequence since creating the popup object.
    pub fn is_initial_configure_sent(&self) -> bool {
        self.configure.lock().unwrap().initial_configure_sent
    }

    /// Set position information that should take effect when mapping.
    /// Updates pending state.
    pub fn set_position(&self, position: PopupLocation) {
        self.configure
            .lock()
            .unwrap()
            .with_pending_state(|state| state.position = position);
    }

    /// Last known / pending popup location relative to the parent.
    pub fn current_location(&self) -> PopupLocation {
        self.configure.lock().unwrap().current_server_state().position
    }

    /// Adds the repositioned token to pending state.
    pub fn set_repositioned(&mut self, token: u32) {
        self.configure
            .lock()
            .unwrap()
            .with_pending_state(|state| state.repositioned = Some(token));
    }

    /// Send a configure event to this popup surface to suggest it a new configuration
    ///
    /// The serial of this configure will be tracked waiting for the client to ACK it.
    /// Call this from input_method.done
    pub fn send_pending_configure(&self) {
        let surface_role = self.surface_role.clone();
        self.configure
            .lock()
            .unwrap()
            .send_pending_configure(|new_state, sent_state, serial| {
                let PopupLocation { anchor, geometry } = new_state.position.clone();
                let relative_to_popup = anchor.loc - geometry.loc;
                surface_role.start_configure(
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
                        surface_role.repositioned(new);
                    }
                }
            });
    }
}

impl PartialEq for PopupSurface {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.surface_role == other.surface_role
    }
}

/// Compositor-defined state
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PopupSurfaceState {
    /// Positioning information
    position: PopupLocation,
    /// Token to send to the client, if any
    ///
    /// The protocol doesn't mandate the lifecycle for this token, so this holds the last state and update events are sent on detected changes.
    repositioned: Option<u32>,
    /// Already issued a configure sequence
    configured: bool,
}

impl PopupSurfaceState {
    pub(super) fn set_configured(&mut self) {
        self.configured = true;
    }
}

/// Data accessible from ZwpInputPopupSurfaceV3 object
#[derive(Debug)]
pub struct InputMethodPopupSurfaceUserData {
    /// Input method controlling this popup
    input_method: ZwpInputMethodV3,
    pub(super) alive_tracker: AliveTracker,
    pub(super) surface: WlSurface,
    pub(super) configure: Arc<Mutex<PopupConfigureAttributes>>,
    /// State acknowledged by client.
    pub(super) acked_state: Arc<Mutex<PopupSurfaceState>>,
    /// Computes the position of the popup according to provided rules
    pub(super) positioner: Mutex<PositionerState>,
}

impl InputMethodPopupSurfaceUserData {
    fn new(
        input_method: ZwpInputMethodV3,
        surface: WlSurface,
        configure: Arc<Mutex<PopupConfigureAttributes>>,
        acked_state: Arc<Mutex<PopupSurfaceState>>,
        positioner: Mutex<PositionerState>,
    ) -> Self {
        Self {
            input_method,
            alive_tracker: AliveTracker::default(),
            surface,
            configure,
            acked_state,
            positioner,
        }
    }
}

impl<D> Dispatch2<ZwpInputPopupSurfaceV3, D> for InputMethodPopupSurfaceUserData
where
    D: InputMethodHandler + SeatHandler,
{
    fn request(
        &self,
        state: &mut D,
        _client: &wayland_server::Client,
        popup: &ZwpInputPopupSurfaceV3,
        request: zwp_input_popup_surface_v3::Request,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, D>,
    ) {
        use zwp_input_popup_surface_v3::Request;
        match request {
            Request::AckConfigure { serial } => {
                let surface = &self.surface;

                let serial = Serial::from(serial);
                let client_state = self.configure.lock().unwrap().ack_configure(serial);

                let client_state = match client_state {
                    Some(state) => state,
                    None => {
                        popup.post_error(
                            zwp_input_popup_surface_v3::Error::InvalidSerial,
                            format!("Serial {} is not awaiting ack", <u32>::from(serial)),
                        );
                        return;
                    }
                };
                *self.acked_state.lock().unwrap() = client_state.clone();
                state.popup_ack_configure(surface, serial, client_state);
            }
            Request::Reposition { positioner, token } => {
                let im: &InputMethodUserData<D> = self.input_method.data().unwrap();
                let positioner: &PositionerUserData = positioner.data().unwrap();
                let positioner = *positioner.inner.lock().unwrap();

                let mut inner = im.handle.inner.lock().unwrap();
                let owner_id = self.input_method.id();
                let Some(instance) = inner.instances.iter_mut().find(|i| i.object.id() == owner_id) else {
                    return;
                };
                let Some(popup) = instance
                    .popup_handles
                    .iter_mut()
                    .find(|h| h.surface_role == *popup)
                else {
                    return;
                };
                *self.positioner.lock().unwrap() = positioner;
                popup.set_repositioned(token);

                // StartOfPreedit must never follow the live caret via Size/Reposition
                // when unanchored — that is what made Kate track the end caret.
                let cursor = if popup.position_mode == PopupPositionMode::FollowCursor {
                    Some(instance.text_input_rectangle)
                } else {
                    popup.anchored_cursor_rectangle
                };
                let parent_surface = cursor.as_ref().map(|_| popup.get_parent().surface.clone());
                let popup = popup.clone();
                drop(inner);

                if let (Some(cursor), Some(parent_surface)) = (cursor, parent_surface) {
                    let popup_geometry = state.popup_geometry(&parent_surface, &cursor, &positioner);
                    popup.set_position(PopupLocation {
                        anchor: cursor,
                        geometry: popup_geometry,
                    });
                }

                state.popup_repositioned(popup.into());
                im.handle.done();
            }
            Request::SetPopupPositionMode { mode } => {
                let mode = mode.into_result().unwrap_or(PopupPositionMode::FollowCursor);
                let im: &InputMethodUserData<D> = self.input_method.data().unwrap();
                let mut inner = im.handle.inner.lock().unwrap();
                let owner_id = self.input_method.id();
                let Some(instance) = inner.instances.iter_mut().find(|i| i.object.id() == owner_id) else {
                    return;
                };
                let Some(popup) = instance
                    .popup_handles
                    .iter_mut()
                    .find(|h| h.surface_role == *popup)
                else {
                    return;
                };
                let previous = popup.position_mode;
                popup.position_mode = mode;
                if mode == PopupPositionMode::StartOfPreedit {
                    // Only arm a new lock when entering this mode or after the
                    // anchor was cleared (e.g. CommitString). Re-setting the same
                    // mode must not reopen awaiting — Kate/Alacritty report the
                    // caret at preedit end and would steal the lock.
                    if previous != PopupPositionMode::StartOfPreedit
                        || popup.anchored_cursor_rectangle.is_none()
                    {
                        popup.awaiting_anchor = true;
                    }
                } else {
                    popup.awaiting_anchor = false;
                    popup.anchored_cursor_rectangle = None;
                }
            }
            Request::Destroy => {
                // Nothing to do
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(&self, _state: &mut D, _client: ClientId, _object: &ZwpInputPopupSurfaceV3) {
        self.alive_tracker.destroy_notify();
    }
}
