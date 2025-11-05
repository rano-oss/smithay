use wayland_server::protocol::wl_surface::WlSurface;
use wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3::ZwpTextInputV3;

use crate::wayland::text_input;

#[derive(Clone, Debug, Default)]
pub(crate) struct TextInputHandles {
    v3: text_input::TextInputHandle,
}

impl TextInputHandles {
    pub fn new(v3: text_input::TextInputHandle) -> Self {
        Self { v3 }
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
            // same for xx
        }
    }

    pub fn focus(&self) -> Option<WlSurface> {
        self.v3.focus().or_else(||
            // same for xx
            None
        )
    }

    pub fn done(&self, discard_state: bool) {
        if !self.v3.done(discard_state) {
            // same for xx
        }
    }

    pub fn enter(&self) {
        // Enter internally broadcasts to all instances
        self.v3.enter();
        // same for xx.
    }
    
    pub fn leave(&self) {
        // Leave internally broadcasts to all instances
        self.v3.leave();
        // same for xx. 
    }
}

pub(crate) enum TextInput<'a> {
    V3(&'a ZwpTextInputV3),
}
