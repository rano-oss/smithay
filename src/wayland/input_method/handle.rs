//! Unified seat-level input method handle wrapping v2 and v3 backends.

use wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3::{
    ChangeCause, ContentHint, ContentPurpose,
};
use wayland_server::protocol::wl_surface::WlSurface;

use crate::input::SeatHandler;
use crate::utils::{Logical, Rectangle};

use super::InputMethodHandler;
use super::v2::V2InputMethodHandle;
use super::v3::V3InputMethodHandle;

/// Handle to input method state for a seat, covering both protocol versions.
///
/// Compositors and text-input integration should use this type exclusively.
/// Individual protocol versions live in the internal v2/v3 modules.
#[derive(Clone, Debug, Default)]
pub struct InputMethodHandle {
    v2: V2InputMethodHandle,
    v3: V3InputMethodHandle,
}

impl InputMethodHandle {
    pub(crate) fn v2(&self) -> &V2InputMethodHandle {
        &self.v2
    }

    pub(crate) fn v3(&self) -> &V3InputMethodHandle {
        &self.v3
    }

    /// Whether there's an active instance of input-method.
    pub(crate) fn has_instance(&self) -> bool {
        self.v2.has_instance() || self.v3.has_active_instance()
    }

    /// Deactivate the active input method.
    pub(crate) fn deactivate_input_method<D: SeatHandler + 'static>(&self, state: &mut D) {
        if self.v2.has_instance() {
            self.v2.deactivate_input_method(state);
        }
        if self.v3.has_active_instance() {
            self.v3.deactivate_input_method(state);
        }
    }

    /// Activate input method on the given surface.
    pub(crate) fn activate_input_method<D: SeatHandler + 'static>(&self, state: &mut D, surface: &WlSurface) {
        if self.v2.has_instance() {
            self.v2.activate_input_method(state, surface);
        }
        if self.v3.has_active_instance() {
            self.v3.activate_input_method(state, surface);
        }
    }

    pub(crate) fn surrounding_text(&self, text: String, cursor: u32, anchor: u32) {
        let text_v2 = text.clone();
        self.v2.with_instance(move |input_method| {
            input_method.object.surrounding_text(text_v2, cursor, anchor);
        });
        self.v3.with_instance(move |input_method| {
            input_method.object.surrounding_text(text, cursor, anchor);
        });
    }

    pub(crate) fn text_change_cause(&self, cause: ChangeCause) {
        self.v2.with_instance(move |input_method| {
            input_method.object.text_change_cause(cause);
        });
        self.v3.with_instance(move |input_method| {
            input_method.object.text_change_cause(cause);
        });
    }

    pub(crate) fn content_type(&self, hint: ContentHint, purpose: ContentPurpose) {
        self.v2.with_instance(move |input_method| {
            input_method.object.content_type(hint, purpose);
        });
        self.v3.with_instance(move |input_method| {
            input_method.object.content_type(hint, purpose);
        });
    }

    pub(crate) fn cursor_rectangle<D: SeatHandler + InputMethodHandler + 'static>(
        &self,
        state: &mut D,
        rect: Rectangle<i32, Logical>,
    ) {
        self.v2.set_text_input_rectangle(state, rect);
        self.v3.set_text_input_rectangle(state, rect);
    }

    pub(crate) fn text_input_done<D: SeatHandler + InputMethodHandler + 'static>(&self, state: &mut D) {
        self.v2.with_instance(|input_method| input_method.done());
        self.v3.text_input_done(state);
    }

    /// Indicates that an input method has grabbed a keyboard
    pub fn keyboard_grabbed(&self) -> bool {
        self.v2.keyboard_grabbed()
    }

    /// Select the active v3 input method instance by app_id.
    pub fn set_active_instance(&self, app_id: &str) -> bool {
        self.v3.set_active_instance(app_id)
    }

    /// Clear the active v3 input method instance.
    pub fn clear_active_instance<D: SeatHandler + 'static>(&self, state: &mut D) {
        self.v3.clear_active_instance(state);
    }

    /// List registered v3 input method app_ids.
    pub fn list_instances(&self) -> Vec<String> {
        self.v3.list_instances()
    }
}
