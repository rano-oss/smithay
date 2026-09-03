//! Utilities for input method support
//!
//! This module provides you with utilities to handle input methods,
//! it must be used in conjunction with the text input module to work.
//!
//! ```
//! use smithay::input::{Seat, SeatState, SeatHandler, pointer::CursorImageStatus};
//! # use smithay::wayland::compositor::{CompositorHandler, CompositorState, CompositorClientState};
//! use smithay::wayland::input_method::{InputMethodHandler, InputMethodManagerState, PopupSurface};
//! use smithay::wayland::text_input::TextInputManagerState;
//! use smithay::reexports::wayland_server::{Display, protocol::wl_surface::WlSurface};
//! # use smithay::reexports::wayland_server::Client;
//! use smithay::utils::{Rectangle, Logical};
//!
//! # struct State { seat_state: SeatState<Self> };
//!
//! impl InputMethodHandler for State {
//!     fn new_popup(&mut self, surface: PopupSurface) {}
//!     fn dismiss_popup(&mut self, surface: PopupSurface) {}
//!     fn popup_repositioned(&mut self, surface: PopupSurface) {}
//!     fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
//!         Rectangle::default()
//!     }
//! }
//!
//! smithay::delegate_dispatch2!(State);
//!
//! # let mut display = wayland_server::Display::<State>::new().unwrap();
//! # let display_handle = display.handle();
//!
//! let mut seat_state = SeatState::<State>::new();
//!
//! // implement the required traits
//! impl SeatHandler for State {
//!     type KeyboardFocus = WlSurface;
//!     type PointerFocus = WlSurface;
//!     type TouchFocus = WlSurface;
//!     fn seat_state(&mut self) -> &mut SeatState<Self> {
//!         &mut self.seat_state
//!     }
//!     fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) { unimplemented!() }
//!     fn cursor_image(&mut self, seat: &Seat<Self>, image: CursorImageStatus) { unimplemented!() }
//! }
//!
//! # impl CompositorHandler for State {
//! #     fn compositor_state(&mut self) -> &mut CompositorState { unimplemented!() }
//! #     fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState { unimplemented!() }
//! #     fn commit(&mut self, surface: &WlSurface) {}
//! # }
//!
//! // Add the seat state to your state and create manager globals
//! InputMethodManagerState::new::<State, _>(&display_handle, |_client| true);
//! // Add text input capabilities, needed for the input method to work
//! TextInputManagerState::new::<State>(&display_handle);
//!
//! ```

use wayland_server::{Client, DisplayHandle, protocol::wl_surface::WlSurface};

use crate::{
    input::{Seat, SeatHandler},
    utils::{Logical, Rectangle, Serial},
};

mod handle;
mod manager;
mod popup;
mod v2;
mod v3;

pub use handle::InputMethodHandle;
pub use manager::InputMethodManagerGlobalData;
pub use popup::{PopupParent, PopupSurface};

pub use v2::{
    INPUT_POPUP_SURFACE_ROLE, InputMethodKeyboardGrab, InputMethodKeyboardUserData, InputMethodManagerState,
    InputMethodPopupSurfaceUserData, InputMethodUserData,
};

pub use v3::{
    InputMethodManagerState as InputMethodManagerStateV3, PopupSurfaceState, PositionerState,
    PositionerUserData,
};

pub(crate) use v3::InputMethodUserData as InputMethodV3UserData;

/// Adds input method popup to compositor state
pub trait InputMethodHandler {
    /// Add a popup surface to compositor state.
    fn new_popup(&mut self, surface: PopupSurface);

    /// Dismiss a popup surface from the compositor state.
    fn dismiss_popup(&mut self, surface: PopupSurface);

    /// Popup location has changed.
    fn popup_repositioned(&mut self, surface: PopupSurface);

    /// Sets the parent location so the popup surface can be placed correctly
    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical>;

    /// Returns the position of the popup, given the cursor rectangle expressed in position relative to surface.
    /// This may be called while locks on some input-method objects are held.
    fn popup_geometry(
        &self,
        _parent: &WlSurface,
        _cursor: &Rectangle<i32, Logical>,
        _positioner: &PositionerState,
    ) -> Rectangle<i32, Logical> {
        Rectangle::default()
    }

    /// Returns the app_id for an input method client.
    ///
    /// Typically resolved from the client's security context.
    /// If `None` is returned, the input method instance will not be registered.
    fn input_method_app_id(&self, _client: &Client, _dh: &DisplayHandle) -> Option<String> {
        None
    }

    /// Called when a new input method instance registers with the compositor.
    ///
    /// This allows the compositor to sync state (e.g. activate the correct IME for the current layout).
    fn input_method_instance_registered(&mut self) {}

    /// Optional hook when the client acknowledges a popup configure sequence.
    fn popup_ack_configure(
        &mut self,
        _surface: &WlSurface,
        _serial: Serial,
        _client_state: PopupSurfaceState,
    ) {
        // the compositor doesn't need to implement this if it doesn't have a use for it
    }
}

/// Extends [Seat] with input method functionality
pub trait InputMethodSeat {
    /// Get an input method associated with this seat
    fn input_method(&self) -> &InputMethodHandle;
}

impl<D: SeatHandler + 'static> InputMethodSeat for Seat<D> {
    fn input_method(&self) -> &InputMethodHandle {
        let user_data = self.user_data();
        user_data.insert_if_missing(InputMethodHandle::default);
        user_data.get::<InputMethodHandle>().unwrap()
    }
}
