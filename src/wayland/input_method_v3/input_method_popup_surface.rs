use std::cmp::PartialEq;
use std::sync::{Arc, Mutex};

use wayland_protocols::wp::input_method::zv3::server::zwp_input_method_v3::ZwpInputMethodV3;
use wayland_protocols::wp::input_method::zv3::server::zwp_input_popup_surface_v3::{
    self, PopupPositionMode, ZwpInputPopupSurfaceV3,
};
use wayland_server::{backend::ClientId, protocol::wl_surface::WlSurface, Resource};

use crate::input::SeatHandler;
use crate::utils::{
    alive_tracker::{AliveTracker, IsAlive},
    Logical, Point, Rectangle, Serial,
};
use crate::wayland::Dispatch2;

use super::{
    configure_tracker::PopupConfigureAttributes,
    positioner::{PositionerState, PositionerUserData},
    InputMethodHandler, InputMethodUserData,
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImPopupLocation {
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
                position: ImPopupLocation { anchor, geometry },
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
        }
    }

    /// Returns a copy of the positioner. That can be used to calculate a new position.
    pub fn positioner(&self) -> PositionerState {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        *role_data.positioner.lock().unwrap()
    }

    /// Whether this popup tracks live cursor rectangle updates.
    pub fn follows_cursor(&self) -> bool {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        *role_data.position_mode.lock().unwrap() == PopupPositionMode::FollowCursor
    }

    /// Anchored cursor rectangle while in start_of_preedit mode.
    pub fn anchor_cursor(&self) -> Option<Rectangle<i32, Logical>> {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        *role_data.anchored_cursor_rectangle.lock().unwrap()
    }

    /// Whether this popup is in start-of-preedit positioning mode.
    pub fn is_start_of_preedit(&self) -> bool {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        *role_data.position_mode.lock().unwrap() == PopupPositionMode::StartOfPreedit
    }

    /// Whether this popup is waiting for the anchor probe cursor rectangle.
    pub fn is_awaiting_preedit_anchor(&self) -> bool {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        *role_data.awaiting_preedit_anchor.lock().unwrap()
    }

    /// Clear a frozen start-of-preedit anchor so the next preedit re-probes.
    pub fn clear_preedit_anchor(&self) {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        *role_data.anchored_cursor_rectangle.lock().unwrap() = None;
        *role_data.awaiting_preedit_anchor.lock().unwrap() = false;
        *role_data.preedit_anchor_needs_probe.lock().unwrap() = true;
    }

    /// Whether the next preedit segment needs an empty-preedit anchor probe.
    pub fn preedit_anchor_needs_probe(&self) -> bool {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        *role_data.preedit_anchor_needs_probe.lock().unwrap()
    }

    /// Freeze the popup anchor at the current cursor rectangle without probing.
    pub(crate) fn freeze_preedit_anchor(&self, cursor: Rectangle<i32, Logical>) {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        *role_data.anchored_cursor_rectangle.lock().unwrap() = Some(cursor);
        *role_data.awaiting_preedit_anchor.lock().unwrap() = false;
        *role_data.preedit_anchor_needs_probe.lock().unwrap() = false;
    }

    /// Probe the preedit start by sending an empty preedit to the text-input client.
    pub(crate) fn begin_preedit_anchor_probe(
        &self,
        text_input_handle: &crate::wayland::text_input::TextInputHandle,
    ) {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        *role_data.anchored_cursor_rectangle.lock().unwrap() = None;
        *role_data.awaiting_preedit_anchor.lock().unwrap() = true;
        text_input_handle.with_active_text_input(|ti, _surface| {
            ti.preedit_string(Some(String::new()), 0, 0);
        });
        text_input_handle.done(false);
    }

    fn set_start_of_preedit_mode(&self) {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        *role_data.position_mode.lock().unwrap() = PopupPositionMode::StartOfPreedit;
    }

    /// Store the cursor rectangle returned after probing preedit start.
    pub(crate) fn try_capture_preedit_anchor(&self, cursor: Rectangle<i32, Logical>) -> bool {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        let mut awaiting = role_data.awaiting_preedit_anchor.lock().unwrap();
        if !*awaiting {
            return false;
        }
        *awaiting = false;
        *role_data.anchored_cursor_rectangle.lock().unwrap() = Some(cursor);
        *role_data.preedit_anchor_needs_probe.lock().unwrap() = false;
        true
    }

    /// Reset popup positioning to the default follow-cursor behavior.
    pub fn reset_popup_position_mode(&self) {
        let role_data: &InputMethodPopupSurfaceUserData = self.surface_role.data().unwrap();
        *role_data.position_mode.lock().unwrap() = PopupPositionMode::FollowCursor;
        *role_data.anchored_cursor_rectangle.lock().unwrap() = None;
        *role_data.awaiting_preedit_anchor.lock().unwrap() = false;
        *role_data.preedit_anchor_needs_probe.lock().unwrap() = false;
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

    /// Used to access the location of an input popup surface relative to the parent
    pub fn location(&self) -> Point<i32, Logical> {
        self.acked_state.lock().unwrap().position.geometry.loc
    }

    /// `true` if the surface sent a
    /// configure sequence since creating the popup object.
    pub fn is_initial_configure_sent(&self) -> bool {
        self.configure.lock().unwrap().initial_configure_sent
    }

    /// Set position information that should take effect when mapping.
    /// Updates pending state.
    pub fn set_position(&mut self, position: ImPopupLocation) {
        self.configure
            .lock()
            .unwrap()
            .with_pending_state(|state| state.position = position);
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
    pub fn send_pending_configure(&mut self) {
        let surface_role = self.surface_role.clone();
        self.configure.lock().unwrap().send_pending_configure(
            |new_state, sent_state, serial| {
                let ImPopupLocation { anchor, geometry } = new_state.position.clone();
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
            },
        );
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
    position: ImPopupLocation,
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

/// Parent surface and location for the IME popup.
#[derive(Debug, Clone)]
pub struct PopupParent {
    /// The surface over which the IME popup is shown.
    pub surface: WlSurface,
    /// The location of the parent surface relative to TODO.
    pub location: Rectangle<i32, Logical>,
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
    pub(super) position_mode: Mutex<PopupPositionMode>,
    pub(super) anchored_cursor_rectangle: Mutex<Option<Rectangle<i32, Logical>>>,
    pub(super) awaiting_preedit_anchor: Mutex<bool>,
    /// After `commit_string`, the next segment must probe; the first segment can use the live cursor.
    pub(super) preedit_anchor_needs_probe: Mutex<bool>,
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
            position_mode: Mutex::new(PopupPositionMode::FollowCursor),
            anchored_cursor_rectangle: Mutex::new(None),
            awaiting_preedit_anchor: Mutex::new(false),
            preedit_anchor_needs_probe: Mutex::new(false),
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
                let popup = {
                    let positioner: &PositionerUserData = positioner.data().unwrap();
                    let positioner = *positioner.inner.lock().unwrap();
                    let mut inner = im.handle.inner.lock().unwrap();
                    // This request comes to an input_method object, so an empty instance is a bug.
                    let active_id = inner.active_input_method_id.clone().unwrap();
                    let instance = inner
                        .instances
                        .iter_mut()
                        .find(|i| i.object.id() == active_id)
                        .unwrap();
                    let cursor = self
                        .anchored_cursor_rectangle
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
            Request::SetPopupPositionMode { mode } => {
                let mode = mode.into_result().unwrap_or(PopupPositionMode::FollowCursor);
                let im: &InputMethodUserData<D> = self.input_method.data().unwrap();
                let popup = {
                    let inner = im.handle.inner.lock().unwrap();
                    let active_id = inner.active_input_method_id.clone().unwrap();
                    let instance = inner
                        .instances
                        .iter()
                        .find(|i| i.object.id() == active_id)
                        .unwrap();
                    instance
                        .popup_handles
                        .iter()
                        .find(|h| h.surface_role == *popup)
                        .expect("This popup not tracked by its input method")
                        .clone()
                };
                match mode {
                    PopupPositionMode::FollowCursor => popup.reset_popup_position_mode(),
                    PopupPositionMode::StartOfPreedit => popup.set_start_of_preedit_mode(),
                    _ => {}
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
