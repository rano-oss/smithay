//! Shared IME → text-input forwarding used by both protocol versions.

use crate::wayland::text_input::TextInputHandle;

pub(crate) fn commit_string(handle: &TextInputHandle, text: String) {
    handle.with_active_text_input(|ti, _surface| {
        ti.commit_string(Some(text.clone()));
    });
}

pub(crate) fn set_preedit_string(
    handle: &TextInputHandle,
    text: String,
    cursor_begin: i32,
    cursor_end: i32,
) {
    handle.with_active_text_input(|ti, _surface| {
        ti.preedit_string(Some(text.clone()), cursor_begin, cursor_end);
    });
}

pub(crate) fn delete_surrounding_text(handle: &TextInputHandle, before_length: u32, after_length: u32) {
    handle.with_active_text_input(|ti, _surface| {
        ti.delete_surrounding_text(before_length, after_length);
    });
}

pub(crate) fn commit_done(handle: &TextInputHandle, serial: u32, current_serial: u32) {
    handle.done(serial != current_serial);
}
