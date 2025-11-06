//! Utilities for experimental text input support
//!
//! This module provides you with utilities to handle text input surfaces,
//! it is usually used in conjunction with the input method module.
//!
//! Text input focus is automatically set to the same surface that has keyboard focus.
//!
//! ```
//! use smithay::{
//!     delegate_seat, delegate_text_input_manager,
//! #   delegate_compositor,
//! };
//! use smithay::input::{Seat, SeatState, SeatHandler, pointer::CursorImageStatus};
//! # use smithay::wayland::compositor::{CompositorHandler, CompositorState, CompositorClientState};
//! use smithay::wayland::text_input::TextInputManagerState;
//! use smithay::reexports::wayland_server::{Display, protocol::wl_surface::WlSurface};
//! # use smithay::reexports::wayland_server::Client;
//!
//! # struct State { seat_state: SeatState<Self> };
//!
//! delegate_seat!(State);
//! // Delegate text input handling for State to TextInputManagerState.
//! delegate_text_input_manager!(State);
//!
//! # let mut display = Display::<State>::new().unwrap();
//! # let display_handle = display.handle();
//!
//! let seat_state = SeatState::<State>::new();
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
//! // Add the seat state to your state and create manager global
//! TextInputManagerState::new::<State>(&display_handle);
//!
//! # impl CompositorHandler for State {
//! #     fn compositor_state(&mut self) -> &mut CompositorState { unimplemented!() }
//! #     fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState { unimplemented!() }
//! #     fn commit(&mut self, surface: &WlSurface) {}
//! # }
//! # delegate_compositor!(State);
//! ```
//!
use wl_input_method as wayland_protocols_experimental;

use wayland_protocols_experimental::text_input::v3::server::{
    xx_text_input_manager_v3::{self, XxTextInputManagerV3},
    xx_text_input_v3::XxTextInputV3,
};
use wayland_server::{backend::GlobalId, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New};

use crate::input::{Seat, SeatHandler};

pub use text_input_handle::TextInputHandle;
pub use text_input_handle::TextInputUserData;

use super::input_method;
use super::input_method_v3;

const MANAGER_VERSION: u32 = 1;

mod text_input_handle;

/// Extends [Seat] with text input functionality
pub trait TextInputSeat {
    /// Get text input associated with this seat
    fn text_input(&self) -> &TextInputHandle;
}

impl<D: SeatHandler + 'static> TextInputSeat for Seat<D> {
    fn text_input(&self) -> &TextInputHandle {
        let user_data = self.user_data();
        user_data.insert_if_missing(TextInputHandle::default);
        user_data.get::<TextInputHandle>().unwrap()
    }
}

/// State of wp text input protocol
#[derive(Debug)]
pub struct TextInputManagerState {
    global: GlobalId,
}

impl TextInputManagerState {
    /// Initialize a text input manager global.
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<XxTextInputManagerV3, ()>,
        D: Dispatch<XxTextInputManagerV3, ()>,
        D: Dispatch<XxTextInputV3, TextInputUserData>,
        D: 'static,
    {
        let global = display.create_global::<D, XxTextInputManagerV3, _>(MANAGER_VERSION, ());

        Self { global }
    }

    /// Get the id of XxTextInputManagerV3 global
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

impl<D> GlobalDispatch<XxTextInputManagerV3, (), D> for TextInputManagerState
where
    D: GlobalDispatch<XxTextInputManagerV3, ()>,
    D: Dispatch<XxTextInputManagerV3, ()>,
    D: Dispatch<XxTextInputV3, TextInputUserData>,
    D: 'static,
{
    fn bind(
        _: &mut D,
        _: &DisplayHandle,
        _: &Client,
        resource: New<XxTextInputManagerV3>,
        _: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
    }
}

impl<D> Dispatch<XxTextInputManagerV3, (), D> for TextInputManagerState
where
    D: Dispatch<XxTextInputManagerV3, ()>,
    D: Dispatch<XxTextInputV3, TextInputUserData>,
    D: SeatHandler,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &XxTextInputManagerV3,
        request: xx_text_input_manager_v3::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            xx_text_input_manager_v3::Request::GetTextInput { id, seat } => {
                let seat = Seat::<D>::from_resource(&seat).unwrap();

                let user_data = seat.user_data();
                user_data.insert_if_missing(TextInputHandle::default);
                user_data.insert_if_missing(input_method::InputMethodHandle::default);
                user_data.insert_if_missing(input_method_v3::InputMethodHandle::default);
                let handle = user_data.get::<TextInputHandle>().unwrap();
                let input_method_handle = user_data.get::<input_method::InputMethodHandle>().unwrap();
                let input_method_v3_handle = user_data.get::<input_method_v3::InputMethodHandle>().unwrap();
                let instance = data_init.init(
                    id,
                    TextInputUserData {
                        handle: handle.clone(),
                        input_method_handle: input_method_handle.clone(),
                        input_method_v3_handle: input_method_v3_handle.clone(),
                    },
                );
                handle.add_instance(&instance);
                if input_method_handle.has_instance() {
                    handle.enter();
                }
                if input_method_v3_handle.has_instance() {
                    handle.enter();
                }
            }
            xx_text_input_manager_v3::Request::Destroy => {
                // Nothing to do
            }
            _ => unreachable!(),
        }
    }
}

#[allow(missing_docs)] // TODO
#[macro_export]
macro_rules! delegate_text_input_next_manager {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        $crate::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::reexports::wayland_protocols_experimental::text_input::v3::server::xx_text_input_manager_v3::XxTextInputManagerV3: ()
        ] => $crate::wayland::text_input_next::TextInputManagerState);

        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::reexports::wayland_protocols_experimental::text_input::v3::server::xx_text_input_manager_v3::XxTextInputManagerV3: ()
        ] => $crate::wayland::text_input_next::TextInputManagerState);

        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::reexports::wayland_protocols_experimental::text_input::v3::server::xx_text_input_v3::XxTextInputV3:
            $crate::wayland::text_input_next::TextInputUserData
        ] => $crate::wayland::text_input_next::TextInputManagerState);
    };
}


#[cfg(test)]
mod test {
    use super::*;

    struct Handler {}

    delegate_text_input_next_manager!(Handler);

    fn assert_is_manager_delegate<T>()
    where
        T: wayland_server::Dispatch<XxTextInputManagerV3, ()>,
    {
    }

    fn assert_is_delegate<T>()
    where
        T: wayland_server::Dispatch<XxTextInputV3, TextInputUserData>,
    {
    }

    
    #[test]
    fn test_valid_assignment() {
        assert_is_manager_delegate::<Handler>();
        assert_is_delegate::<Handler>();
    }
}