//! Utilities for input method support
//!
//! This module provides utilities to handle input methods (v2 and v3 protocols).
//! It must be used in conjunction with the text input module.
//!
//! Compositors interact with a single [`InputMethodHandle`] per seat; individual
//! protocol versions live in [`v2`] and [`v3`].

use wayland_server::{Client, DisplayHandle, protocol::wl_surface::WlSurface};

use crate::{
    input::{Seat, SeatHandler},
    utils::{Logical, Rectangle, Serial},
};

mod handle;
mod popup;
mod text_input_sync;

pub mod v2;
pub mod v3;

pub use handle::InputMethodHandle;
pub use popup::InputMethodPopup;

// Backward-compatible re-exports of v2 types at the module root.
pub use v2::{
    InputMethodKeyboardGrab, InputMethodKeyboardUserData, InputMethodManagerGlobalData,
    InputMethodManagerState, InputMethodPopupSurfaceUserData, InputMethodUserData, PopupParent,
    PopupSurface, INPUT_POPUP_SURFACE_ROLE,
};

/// Compositor hooks for input-method popup surfaces from either protocol version.
pub trait InputMethodHandler {
    /// Add a popup surface to compositor state.
    fn new_popup(&mut self, surface: InputMethodPopup);

    /// Dismiss a popup surface from the compositor state.
    fn dismiss_popup(&mut self, surface: InputMethodPopup);

    /// Popup location has changed.
    fn popup_repositioned(&mut self, surface: InputMethodPopup);

    /// Sets the parent location so the popup surface can be placed correctly.
    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical>;

    /// v3: compute popup geometry from cursor rect and positioner.
    fn popup_geometry(
        &self,
        _parent: &WlSurface,
        _cursor: &Rectangle<i32, Logical>,
        _positioner: &v3::PositionerState,
    ) -> Rectangle<i32, Logical> {
        Rectangle::default()
    }

    /// v3: resolve app_id for an input method client from security context.
    fn input_method_app_id(&self, _client: &Client, _dh: &DisplayHandle) -> Option<String> {
        None
    }

    /// v3: called when a new input method instance registers.
    fn input_method_instance_registered(&mut self) {}

    /// v3: optional hook when the client acknowledges a popup configure sequence.
    fn popup_ack_configure(
        &mut self,
        _surface: &WlSurface,
        _serial: Serial,
        _client_state: v3::PopupSurfaceState,
    ) {
    }
}

/// Extends [`Seat`] with input method functionality.
pub trait InputMethodSeat {
    /// Get the input method handle associated with this seat.
    fn input_method(&self) -> &InputMethodHandle;
}

impl<D: SeatHandler + 'static> InputMethodSeat for Seat<D> {
    fn input_method(&self) -> &InputMethodHandle {
        let user_data = self.user_data();
        user_data.insert_if_missing(InputMethodHandle::default);
        user_data.get::<InputMethodHandle>().unwrap()
    }
}
