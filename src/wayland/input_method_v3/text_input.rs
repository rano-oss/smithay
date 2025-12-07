use wayland_server::protocol::wl_surface::WlSurface;
use wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3::ZwpTextInputV3;
//use wl_input_method as wayland_protocols_experimental;
use wayland_protocols_experimental::text_input::v3::server::xx_text_input_v3::XxTextInputV3;

use crate::wayland::{text_input, text_input_next};

#[derive(Clone, Debug, Default)]
pub(crate) struct TextInputHandles {
    v3: text_input::TextInputHandle,
    xx: text_input_next::TextInputHandle,
}

impl TextInputHandles {
    pub fn new(
        v3: text_input::TextInputHandle,
        xx: text_input_next::TextInputHandle,
    ) -> Self {
        Self { v3, xx }
    }
    /// Access the active text-input instance for the currently focused surface.
    pub fn with_active_text_input<F>(&self, mut f: F)
    where
        F: FnMut(&TextInput<'_>, &WlSurface),
    {
        let mut found = false;
        self.v3.with_active_text_input(|ti, s| {
            found = true;
            f(&TextInput::V3(ti), s)
        });
        if !found {
            self.xx.with_active_text_input(|ti, s| {
                f(&TextInput::Xx(ti), s)
            });
        }
    }

    pub fn focus(&self) -> Option<WlSurface> {
        self.v3.focus().or_else(|| self.xx.focus())
    }

    pub fn done(&self, discard_state: bool) {
        if !self.v3.done(discard_state) {
            self.xx.done(discard_state);
        }
    }

    pub fn enter(&self) {
        // Enter internally broadcasts to all instances
        self.v3.enter();
        self.xx.enter();
    }
    
    pub fn leave(&self) {
        // Leave internally broadcasts to all instances
        self.v3.leave();
        self.xx.leave();
    }
}

pub(crate) enum TextInput<'a> {
    V3(&'a ZwpTextInputV3),
    Xx(&'a XxTextInputV3),
}
