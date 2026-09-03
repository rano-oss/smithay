//! Helpers for forwarding text-input state to both input-method v2 and v3.

use wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3::{
    ChangeCause, ContentHint, ContentPurpose,
};
use wayland_server::protocol::wl_surface::WlSurface;

use crate::input::{Seat, SeatHandler};
use crate::utils::{Logical, Rectangle};
use crate::wayland::{input_method, input_method_v3};

use input_method::InputMethodSeat;
use input_method_v3::InputMethodSeat as InputMethodV3Seat;

/// Handles for both input-method protocol versions on a seat.
#[derive(Clone, Debug)]
pub(crate) struct SeatInputMethods {
    v2: input_method::InputMethodHandle,
    v3: input_method_v3::InputMethodHandle,
}

impl SeatInputMethods {
    pub(crate) fn from_seat<D>(seat: &Seat<D>) -> Self
    where
        D: SeatHandler + 'static,
    {
        Self {
            v2: seat.input_method().clone(),
            v3: seat.input_method_v3().clone(),
        }
    }

    pub(crate) fn new(
        v2: input_method::InputMethodHandle,
        v3: input_method_v3::InputMethodHandle,
    ) -> Self {
        Self { v2, v3 }
    }

    pub(crate) fn has_instance(&self) -> bool {
        self.v2.has_instance() || self.v3.has_instance()
    }

    pub(crate) fn deactivate_all<D: SeatHandler + 'static>(&self, state: &mut D) {
        if self.v2.has_instance() {
            self.v2.deactivate_input_method(state);
        }
        if self.v3.has_instance() {
            self.v3.deactivate_input_method(state);
        }
    }

    pub(crate) fn activate_all<D: SeatHandler + 'static>(&self, state: &mut D, surface: &WlSurface) {
        if self.v2.has_instance() {
            self.v2.activate_input_method(state, surface);
        }
        if self.v3.has_instance() {
            self.v3.activate_input_method(state, surface);
        }
    }

    pub(crate) fn forward_surrounding_text(&self, text: String, cursor: u32, anchor: u32) {
        let text_v2 = text.clone();
        self.v2.with_instance(move |input_method| {
            input_method.object.surrounding_text(text_v2, cursor, anchor);
        });
        self.v3.with_instance(move |input_method| {
            input_method.object.surrounding_text(text, cursor, anchor);
        });
    }

    pub(crate) fn forward_text_change_cause(&self, cause: ChangeCause) {
        self.v2.with_instance(move |input_method| {
            input_method.object.text_change_cause(cause);
        });
        self.v3.with_instance(move |input_method| {
            input_method.object.text_change_cause(cause);
        });
    }

    pub(crate) fn forward_content_type(&self, hint: ContentHint, purpose: ContentPurpose) {
        self.v2.with_instance(move |input_method| {
            input_method.object.content_type(hint, purpose);
        });
        self.v3.with_instance(move |input_method| {
            input_method.object.content_type(hint, purpose);
        });
    }

    pub(crate) fn forward_cursor_rectangle<D: SeatHandler + input_method_v3::InputMethodHandler + 'static>(
        &self,
        state: &mut D,
        rect: Rectangle<i32, Logical>,
    ) {
        self.v2.set_text_input_rectangle::<D>(state, rect);
        self.v3.set_cursor_rectangle::<D>(state, rect);
    }

    pub(crate) fn text_input_done(&self) {
        self.v2.with_instance(|input_method| input_method.done());
        self.v3.done();
    }
}
