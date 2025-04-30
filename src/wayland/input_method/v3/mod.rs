//! Utilities for input method support
//!
//! This module provides you with utilities to handle input methods,
//! it must be used in conjunction with the text input module to work.
//!
//! ```
//! use smithay::{
//!     delegate_seat, delegate_input_method_manager, delegate_text_input_manager,
//! #   delegate_compositor,
//! };
//! use smithay::input::{Seat, SeatState, SeatHandler, pointer::CursorImageStatus};
//! # use smithay::wayland::compositor::{CompositorHandler, CompositorState, CompositorClientState};
//! use smithay::wayland::input_method::{InputMethodManagerState, InputMethodHandler, PopupSurface};
//! use smithay::wayland::text_input::TextInputManagerState;
//! use smithay::reexports::wayland_server::{Display, protocol::wl_surface::WlSurface};
//! # use smithay::reexports::wayland_server::Client;
//! use smithay::utils::{Rectangle, Logical};
//!
//! # struct State { seat_state: SeatState<Self> };
//!
//! delegate_seat!(State);
//! # delegate_compositor!(State);
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
//! // Delegate input method handling for State to InputMethodManagerState.
//! delegate_input_method_manager!(State);
//!
//! delegate_text_input_manager!(State);
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

use tracing::warn;
use wayland_server::{backend::GlobalId, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New};

use wayland_protocols::wp::input_method::v3::server::{
    wp_input_method_manager_v3::{self, WpInputMethodManagerV3},
    wp_input_method_v3::WpInputMethodV3,
};

use crate::{
    input::{Seat, SeatHandler},
    utils::SERIAL_COUNTER,
    wayland::text_input::v3_2::TextInputHandle,
};

pub use input_method_handle::{InputMethodHandle, InputMethodUserData};

const MANAGER_VERSION: u32 = 1;
/// Size of key buffer used to reprocess keys e.g text to search
pub const BUFFERSIZE: usize = 10;

mod input_method_handle;

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

/// Data associated with a InputMethodManager global.
#[allow(missing_debug_implementations)]
pub struct InputMethodManagerGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

/// State of wp misc input method protocol
#[derive(Debug)]
pub struct InputMethodManagerState {
    global: GlobalId,
}

impl InputMethodManagerState {
    /// Initialize a text input manager global.
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<WpInputMethodManagerV3, InputMethodManagerGlobalData>,
        D: Dispatch<WpInputMethodManagerV3, ()>,
        D: Dispatch<WpInputMethodV3, InputMethodUserData<D>>,
        D: SeatHandler,
        D: 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let data = InputMethodManagerGlobalData {
            filter: Box::new(filter),
        };
        let global = display.create_global::<D, WpInputMethodManagerV3, _>(MANAGER_VERSION, data);

        Self { global }
    }

    /// Get the id of ZwpTextInputManagerV3 global
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

impl<D> GlobalDispatch<WpInputMethodManagerV3, InputMethodManagerGlobalData, D> for InputMethodManagerState
where
    D: GlobalDispatch<WpInputMethodManagerV3, InputMethodManagerGlobalData>,
    D: Dispatch<WpInputMethodManagerV3, ()>,
    D: Dispatch<WpInputMethodV3, InputMethodUserData<D>>,
    D: SeatHandler,
    D: 'static,
{
    fn bind(
        _: &mut D,
        _: &DisplayHandle,
        _: &Client,
        resource: New<WpInputMethodManagerV3>,
        _: &InputMethodManagerGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, global_data: &InputMethodManagerGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<WpInputMethodManagerV3, (), D> for InputMethodManagerState
where
    D: Dispatch<WpInputMethodManagerV3, ()>,
    D: Dispatch<WpInputMethodV3, InputMethodUserData<D>>,
    D: SeatHandler,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _: &WpInputMethodManagerV3,
        request: wp_input_method_manager_v3::Request,
        _: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_input_method_manager_v3::Request::GetInputMethod {
                seat,
                input_method,
                app_id,
            } => {
                let seat = Seat::<D>::from_resource(&seat).unwrap();
                let user_data = seat.user_data();
                user_data.insert_if_missing(TextInputHandle::default);
                user_data.insert_if_missing(InputMethodHandle::default);
                let handle = user_data.get::<InputMethodHandle>().unwrap();
                let text_input_handle = user_data.get::<TextInputHandle>().unwrap();
                let keyboard_handle = seat.get_keyboard().unwrap();
                let instance = data_init.init(
                    input_method,
                    InputMethodUserData {
                        handle: handle.clone(),
                        text_input_handle: text_input_handle.clone(),
                        keyboard_handle: keyboard_handle.clone(),
                    },
                );
                handle.add_instance(&instance, app_id.clone());
                text_input_handle.with_focused_instance(|instance, surface| {
                    let im_app_id = if let Some(im_app_id) = instance.im_app_id.as_ref() {
                        im_app_id.clone()
                    } else {
                        instance.im_app_id = Some(app_id.clone());
                        app_id.clone()
                    };
                    if im_app_id == app_id {
                        instance.object.enter(surface)
                    }
                });
                let keymap_file = keyboard_handle.arc.keymap.lock().unwrap();
                let res = keymap_file.with_fd(false, |fd, size| {
                    instance.keymap(
                        wayland_server::protocol::wl_keyboard::KeymapFormat::XkbV1,
                        fd,
                        size as u32,
                    );
                });
                let guard = keyboard_handle.arc.internal.lock().unwrap();
                instance.repeat_info(guard.repeat_rate, guard.repeat_delay);
                if let Err(err) = res {
                    warn!(err = ?err, "Failed to send keymap to client");
                } else {
                    // Modifiers can be latched when taking the grab, thus we must send them to keep
                    // them in sync.
                    let mods = guard.mods_state.serialized;
                    instance.modifiers(
                        SERIAL_COUNTER.next_serial().into(),
                        mods.depressed,
                        mods.latched,
                        mods.locked,
                        mods.layout_effective,
                    );
                }
            }
            wp_input_method_manager_v3::Request::Destroy => {
                // Nothing to do
            }
            _ => unreachable!(),
        }
    }
}

#[allow(missing_docs)]
#[macro_export]
macro_rules! delegate_input_method_manager_v3 {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        $crate::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::reexports::wayland_protocols::wp::input_method::v3::server::wp_input_method_manager_v3::WpInputMethodManagerV3:
            $crate::wayland::input_method::v3::InputMethodManagerGlobalData
        ] => $crate::wayland::input_method::v3::InputMethodManagerState);

        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::reexports::wayland_protocols::wp::input_method::v3::server::wp_input_method_manager_v3::WpInputMethodManagerV3: ()
        ] => $crate::wayland::input_method::v3::InputMethodManagerState);
        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::reexports::wayland_protocols::wp::input_method::v3::server::wp_input_method_v3::WpInputMethodV3:
            $crate::wayland::input_method::v3::InputMethodUserData<Self>
        ] => $crate::wayland::input_method::v3::InputMethodManagerState);
    };
}
