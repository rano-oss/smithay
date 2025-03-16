//! Utilities for text input support
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

use wayland_protocols::wp::text_input::v3::server::{
    wp_text_input_manager_v3::{self, WpTextInputManagerV3},
    wp_text_input_v3::WpTextInputV3,
};
use wayland_server::{backend::GlobalId, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New};

use crate::input::{Seat, SeatHandler};

pub use text_input_handle::TextInputHandle;
pub use text_input_handle::TextInputUserData;

use crate::wayland::input_method::v3::InputMethodHandle;

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
        D: GlobalDispatch<WpTextInputManagerV3, ()>,
        D: Dispatch<WpTextInputManagerV3, ()>,
        D: Dispatch<WpTextInputV3, TextInputUserData>,
        D: 'static,
    {
        let global = display.create_global::<D, WpTextInputManagerV3, _>(MANAGER_VERSION, ());

        Self { global }
    }

    /// Get the id of WpTextInputManagerV3 global
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

impl<D> GlobalDispatch<WpTextInputManagerV3, (), D> for TextInputManagerState
where
    D: GlobalDispatch<WpTextInputManagerV3, ()>,
    D: Dispatch<WpTextInputManagerV3, ()>,
    D: Dispatch<WpTextInputV3, TextInputUserData>,
    D: 'static,
{
    fn bind(
        _: &mut D,
        _: &DisplayHandle,
        _: &Client,
        resource: New<WpTextInputManagerV3>,
        _: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
    }
}

impl<D> Dispatch<WpTextInputManagerV3, (), D> for TextInputManagerState
where
    D: Dispatch<WpTextInputManagerV3, ()>,
    D: Dispatch<WpTextInputV3, TextInputUserData>,
    D: SeatHandler,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &WpTextInputManagerV3,
        request: wp_text_input_manager_v3::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_text_input_manager_v3::Request::GetTextInput { id, seat, app_id } => {
                println!("Got the text input!");
                let seat = Seat::<D>::from_resource(&seat).unwrap();
                let user_data = seat.user_data();
                user_data.insert_if_missing(TextInputHandle::default);
                user_data.insert_if_missing(InputMethodHandle::default);
                let handle = user_data.get::<TextInputHandle>().unwrap();
                let input_method_handle = user_data.get::<InputMethodHandle>().unwrap();
                let instance = data_init.init(
                    id,
                    TextInputUserData {
                        handle: handle.clone(),
                        input_method_handle: input_method_handle.clone(),
                    },
                );
                handle.add_instance(
                    &instance,
                    app_id,
                    input_method_handle.currently_set_input_method(),
                );
                if input_method_handle.has_instance() {
                    println!("Handle entered, cause there was an input method present!");
                    handle.enter();
                }
            }
            wp_text_input_manager_v3::Request::Destroy => {
                // Nothing to do
            }
            _ => unreachable!(),
        }
    }
}

#[allow(missing_docs)] // TODO
#[macro_export]
macro_rules! delegate_text_input_manager_v3_2 {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        $crate::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::reexports::wayland_protocols::wp::text_input::v3::server::wp_text_input_manager_v3::WpTextInputManagerV3: ()
        ] => $crate::wayland::text_input::v3_2::TextInputManagerState);

        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::reexports::wayland_protocols::wp::text_input::v3::server::wp_text_input_manager_v3::WpTextInputManagerV3: ()
        ] => $crate::wayland::text_input::v3_2::TextInputManagerState);

        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::reexports::wayland_protocols::wp::text_input::v3::server::wp_text_input_v3::WpTextInputV3:
            $crate::wayland::text_input::v3_2::TextInputUserData
        ] => $crate::wayland::text_input::v3_2::TextInputManagerState);
    };
}
