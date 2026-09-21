//! Unified seat-level input method handle wrapping v2 and v3 backends.

use wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3::{
    ChangeCause, ContentHint, ContentPurpose,
};
use wayland_server::protocol::wl_surface::WlSurface;

use crate::input::{Seat, SeatHandler};
use crate::utils::{Logical, Rectangle};
use crate::wayland::seat::WaylandFocus;
use crate::wayland::text_input::TextInputSeat;

use super::InputMethodHandler;
use super::v2::InputMethodV2Handle;
use super::v3::{InputMethodV3Handle, SetActiveInstanceResult};

/// Handle to input method state for a seat, covering both protocol versions.
///
/// Compositors and text-input integration should use this type exclusively.
/// Individual protocol versions live in the internal v2/v3 modules.
#[derive(Clone, Debug, Default)]
pub struct InputMethodHandle {
    pub(crate) v2: InputMethodV2Handle,
    pub(crate) v3: InputMethodV3Handle,
}

impl InputMethodHandle {
    /// Whether there's an active instance of input-method.
    pub(crate) fn has_instance(&self) -> bool {
        self.v2.has_instance() || self.v3.has_active_instance()
    }

    /// Whether any input method client has registered, even if not currently selected.
    pub(crate) fn has_registered_instance(&self) -> bool {
        self.v2.has_instance() || self.v3.has_registered_instances()
    }

    /// Deactivate the active input method.
    pub fn deactivate_input_method<D: SeatHandler + 'static>(&self, state: &mut D) {
        if self.v2.has_instance() {
            self.v2.deactivate_input_method(state);
        }
        if self.v3.has_active_instance() {
            self.v3.deactivate_input_method(state);
        }
    }

    /// Activate input method on the given surface.
    pub fn activate_input_method<D: SeatHandler + 'static>(&self, state: &mut D, surface: &WlSurface) {
        if self.v2.has_instance() {
            self.v2.activate_input_method(state, surface);
        }
        if self.v3.has_active_instance() {
            self.v3.activate_input_method(state, surface);
        }
    }

    /// Ensure the keyboard filter interceptor after text-input has been enabled.
    ///
    /// No-op when the interceptor is already active for this surface.
    pub fn activate_keyboard_filter_interceptor<D: SeatHandler + 'static>(&self, surface: &WlSurface) {
        if self.v3.has_active_instance() {
            self.v3.activate_keyboard_filter_interceptor::<D>(surface);
        }
    }

    pub(crate) fn surrounding_text(&self, text: String, cursor: u32, anchor: u32) {
        let text_clone = text.clone();
        self.v2.with_instance(move |input_method| {
            input_method.object.surrounding_text(text_clone, cursor, anchor);
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

    /// Notify the active input method that associated state has been committed.
    ///
    /// Compositors should call this after repositioning v3 IME popups so pending
    /// configure events are sent to the client.
    pub fn done(&self) {
        self.v2.with_instance(|input_method| input_method.done());
        self.v3.with_instance(|input_method| input_method.done());
    }

    /// Indicates that an input method has grabbed a keyboard
    pub fn keyboard_grabbed(&self) -> bool {
        self.v2.keyboard_grabbed()
    }

    /// App id of the currently selected v3 input method instance, if any.
    pub fn active_app_id(&self) -> Option<String> {
        self.v3.active_app_id()
    }

    /// Sync IM activation with current text-input focus.
    ///
    /// Call after compositor policy changes the selected instance (e.g. layout switch).
    pub fn sync_activation<D: SeatHandler + 'static>(&self, state: &mut D, seat: &Seat<D>) {
        if !self.has_instance() {
            self.deactivate_input_method(state);
            return;
        }

        if let Some(surface) = seat.text_input().focus() {
            self.activate_input_method(state, &surface);
        }
    }

    /// Select the active v3 input method instance by app_id.
    ///
    /// When `sync` is true, also runs [`Self::sync_activation`] if the selection changed
    /// (layout switches). When false, only updates the active instance — use from
    /// [`InputMethodHandler::input_method_instance_registered`] so smithay can run a
    /// single `sync_activation` afterward.
    pub fn set_active_instance<D: SeatHandler + InputMethodHandler + 'static>(
        &self,
        state: &mut D,
        seat: &Seat<D>,
        app_id: &str,
        sync: bool,
    ) -> bool
    where
        D::KeyboardFocus: WaylandFocus,
    {
        let before = self.active_app_id();
        match self.v3.set_active_instance(state, app_id) {
            SetActiveInstanceResult::NotFound => return false,
            SetActiveInstanceResult::Unchanged => {}
            SetActiveInstanceResult::Changed => {
                self.v3.replay_last_cursor_rectangle(state);
            }
        }
        if sync && self.active_app_id() != before {
            self.sync_activation(state, seat);
        }
        true
    }

    /// Clear the active v3 input method instance and sync activation.
    pub fn clear_active_instance<D: SeatHandler + 'static>(&self, state: &mut D, seat: &Seat<D>)
    where
        D::KeyboardFocus: WaylandFocus,
    {
        self.v3.clear_active_instance(state);
        self.sync_activation(state, seat);
    }
}
