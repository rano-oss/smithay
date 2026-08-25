use std::fs::File;
use std::os::unix::fs::FileExt;
use std::os::unix::io::OwnedFd;
use std::{
    fmt,
    sync::{Arc, Mutex},
};

use crate::input::keyboard::{KeyboardTarget, KeymapFile, ModifiersState};
use crate::{
    input::{Seat, SeatHandler},
    utils::SERIAL_COUNTER,
    wayland::{
        Dispatch2,
        seat::{WaylandFocus, keyboard::for_each_focused_kbds},
    },
};
use tracing::debug;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::Error::NoKeymap;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::{
    self, ZwpVirtualKeyboardV1,
};
use wayland_server::{
    Client, DataInit, DisplayHandle, Resource,
    protocol::wl_keyboard::{KeyState, KeymapFormat},
};
use wkb::WKB;

/// Maximum virtual-keyboard keymap payload accepted from clients (1 MiB).
const MAX_VIRTUAL_KEYMAP_SIZE: usize = 1_048_576;

#[derive(Debug, Default)]
pub(crate) struct VirtualKeyboard {
    state: Option<VirtualKeyboardState>,
}

struct VirtualKeyboardState {
    keymap: KeymapFile,
    mods: ModifiersState,
    wkb: WKB,
}

impl fmt::Debug for VirtualKeyboardState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VirtualKeyboardState")
            .field("keymap", &self.keymap)
            .field("mods", &self.mods)
            .field("wkb", &self.wkb)
            .finish()
    }
}

/// Handle to a virtual keyboard instance
#[derive(Debug, Clone, Default)]
pub(crate) struct VirtualKeyboardHandle {
    pub(crate) inner: Arc<Mutex<VirtualKeyboard>>,
}

/// User data of ZwpVirtualKeyboardV1 object
pub struct VirtualKeyboardUserData<D: SeatHandler> {
    pub(super) handle: VirtualKeyboardHandle,
    pub(crate) seat: Seat<D>,
}

impl<D: SeatHandler> fmt::Debug for VirtualKeyboardUserData<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VirtualKeyboardUserData")
            .field("handle", &self.handle)
            .field("seat", &self.seat.arc)
            .finish()
    }
}

impl<D> Dispatch2<ZwpVirtualKeyboardV1, D> for VirtualKeyboardUserData<D>
where
    D: SeatHandler + 'static,
    <D as SeatHandler>::KeyboardFocus: WaylandFocus,
{
    fn request(
        &self,
        user_data: &mut D,
        _client: &Client,
        virtual_keyboard: &ZwpVirtualKeyboardV1,
        request: zwp_virtual_keyboard_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwp_virtual_keyboard_v1::Request::Keymap { format, fd, size } => {
                update_keymap(self, format, fd, size as usize);
            }
            zwp_virtual_keyboard_v1::Request::Key { time, key, state } => {
                // Ensure keymap was initialized.
                let mut virtual_data = self.handle.inner.lock().unwrap();
                let vk_state = match virtual_data.state.as_mut() {
                    Some(vk_state) => vk_state,
                    None => {
                        virtual_keyboard.post_error(NoKeymap, "`key` sent before keymap.");
                        return;
                    }
                };

                // Ensure virtual keyboard's keymap is active.
                let keyboard_handle = self.seat.get_keyboard().unwrap();
                let mut internal = keyboard_handle.arc.internal.lock().unwrap();
                let focus = internal.focus.as_mut().map(|(focus, _)| focus);
                keyboard_handle.send_keymap(user_data, &focus, &vk_state.keymap, vk_state.mods);

                if let Some(wl_surface) = focus.and_then(|f| f.wl_surface()) {
                    for_each_focused_kbds(&self.seat, &wl_surface, |kbd| {
                        // This should be wl_keyboard::KeyState, but the protocol does not state
                        // the parameter is an enum.
                        let key_state = if state == 1 {
                            KeyState::Pressed
                        } else {
                            KeyState::Released
                        };

                        kbd.key(SERIAL_COUNTER.next_serial().0, time, key, key_state);
                    });
                }
            }
            zwp_virtual_keyboard_v1::Request::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                // Ensure keymap was initialized.
                let mut virtual_data = self.handle.inner.lock().unwrap();
                let state = match virtual_data.state.as_mut() {
                    Some(state) => state,
                    None => {
                        virtual_keyboard.post_error(NoKeymap, "`modifiers` sent before keymap.");
                        return;
                    }
                };

                // Update virtual keyboard's modifier state.
                state
                    .wkb
                    .update_modifiers(mods_depressed, mods_latched, mods_locked, group);
                state.mods.update_with(&state.wkb);

                // Ensure virtual keyboard's keymap is active.
                let keyboard_handle = self.seat.get_keyboard().unwrap();
                let mut internal = keyboard_handle.arc.internal.lock().unwrap();
                let focus = internal.focus.as_mut().map(|(focus, _)| focus);
                let keymap_changed =
                    keyboard_handle.send_keymap(user_data, &focus, &state.keymap, state.mods);

                // Report modifiers change to all keyboards.
                if !keymap_changed {
                    if let Some(focus) = focus {
                        focus.modifiers(&self.seat, user_data, state.mods, SERIAL_COUNTER.next_serial());
                    }
                }
            }
            zwp_virtual_keyboard_v1::Request::Destroy => {
                // Nothing to do
            }
            _ => unreachable!(),
        }
    }
}

/// Handle the zwp_virtual_keyboard_v1::keymap request.
fn update_keymap<D>(data: &VirtualKeyboardUserData<D>, format: u32, fd: OwnedFd, size: usize)
where
    D: SeatHandler + 'static,
{
    if format != KeymapFormat::XkbV1 as u32 {
        debug!("Unsupported keymap format: {format:?}");
        return;
    }
    if size == 0 || size > MAX_VIRTUAL_KEYMAP_SIZE {
        debug!("Virtual keyboard keymap size out of range: {size}");
        return;
    }
    let file = File::from(fd);
    let mut bytes = vec![0; size];
    if file.read_exact_at(&mut bytes, 0).is_err() {
        debug!("Failed to read virtual keyboard keymap from fd");
        return;
    }
    // wl_keyboard keymaps are normally NUL-terminated, and `size` includes that terminator.
    if bytes.last() == Some(&0) {
        bytes.pop();
    }
    let keymap = match str::from_utf8(&bytes) {
        Ok(keymap) => keymap,
        Err(err) => {
            debug!("Virtual keyboard keymap is not valid UTF-8: {err}");
            return;
        }
    };
    let wkb = match WKB::new_from_string(keymap) {
        Ok(wkb) => wkb,
        Err(err) => {
            debug!("Failed to load virtual keyboard keymap: {err}");
            return;
        }
    };
    let new_keymap = match wkb.as_xkb_string() {
        Some(keymap) => keymap,
        None => {
            debug!("Failed to serialize virtual keyboard keymap");
            return;
        }
    };

    // Store active virtual keyboard map.
    let mut inner = data.handle.inner.lock().unwrap();
    let mods = inner.state.take().map(|state| state.mods).unwrap_or_default();
    inner.state = Some(VirtualKeyboardState {
        mods,
        keymap: KeymapFile::new(new_keymap),
        wkb,
    });
}
