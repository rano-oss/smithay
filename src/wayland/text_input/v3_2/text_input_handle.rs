use std::cmp::min;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tracing::debug;
use wayland_protocols::wp::text_input::v3::server::wp_text_input_v3::{self, WpTextInputV3};
use wayland_server::backend::ClientId;
use wayland_server::{protocol::wl_surface::WlSurface, Dispatch, Resource};

use crate::input::SeatHandler;
use crate::utils::IsAlive;
use crate::wayland::input_method::v3::{InputMethodHandle, BUFFERSIZE};

use super::TextInputManagerState;

#[derive(Debug)]
pub(crate) struct Instance {
    pub object: WpTextInputV3,
    pub serial: u32,
    pub app_id: String,
    pub im_app_id: Option<String>,
}

#[derive(Default, Debug)]
pub(crate) struct TextInput {
    instances: Vec<Instance>,
    focus: Option<WlSurface>,
}

impl TextInput {
    fn serial(&self) -> u32 {
        if let Some(ref surface) = self.focus {
            if !surface.alive() {
                return 0;
            }
            for ti in self.instances.iter() {
                if ti.object.id().same_client_as(&surface.id()) {
                    return ti.serial;
                }
            }
        }
        0
    }

    fn with_focused_instance<F>(&mut self, mut f: F)
    where
        F: FnMut(&mut Instance, &WlSurface),
    {
        if let Some(ref surface) = self.focus {
            if !surface.alive() {
                return;
            }
            for ti in self.instances.iter_mut() {
                if ti.object.id().same_client_as(&surface.id()) {
                    f(ti, surface);
                }
            }
        }
    }

    fn im_app_id(&self) -> Option<String> {
        if let Some(ref surface) = self.focus {
            if !surface.alive() {
                return None;
            }
            for ti in self.instances.iter() {
                if ti.object.id().same_client_as(&surface.id()) {
                    return ti.im_app_id.clone();
                }
            }
            None
        } else {
            None
        }
    }

    fn input_method_destroyed(&mut self, app_id: String) {
        for ti in self.instances.iter_mut() {
            ti.im_app_id.take_if(|im_app_id| *im_app_id == app_id);
            if let Some(ref surface) = self.focus {
                if ti.object.id().same_client_as(&surface.id()) {
                    ti.object.leave(surface);
                }
            }
        }
    }
}

/// Handle to text input instances
#[derive(Default, Debug, Clone)]
pub struct TextInputHandle {
    pub(crate) inner: Arc<Mutex<TextInput>>,
}

impl TextInputHandle {
    pub(super) fn add_instance(&self, instance: &WpTextInputV3, app_id: String, im_app_id: Option<String>) {
        let mut inner = self.inner.lock().unwrap();
        inner.instances.push(Instance {
            object: instance.clone(),
            serial: 0,
            app_id,
            im_app_id,
        });
    }

    fn increment_serial(&self, text_input: &WpTextInputV3) {
        let mut inner = self.inner.lock().unwrap();
        for ti in inner.instances.iter_mut() {
            if &ti.object == text_input {
                ti.serial += 1;
            }
        }
    }

    /// Gets currently focused text input serial
    pub fn serial(&self) -> u32 {
        let inner = self.inner.lock().unwrap();
        inner.serial()
    }

    /// Return the currently focused surface.
    pub fn focus(&self) -> Option<WlSurface> {
        self.inner.lock().unwrap().focus.clone()
    }

    /// Sets the input method app id
    pub fn set_im_app_id(&self, ti_to_im_map: HashMap<String, String>) {
        let mut inner = self.inner.lock().unwrap();
        for instance in inner.instances.iter_mut() {
            if let Some(im_app_id) = ti_to_im_map.get(&instance.app_id) {
                instance.im_app_id = Some(im_app_id.clone())
            }
        }
    }

    pub(crate) fn input_method_destroyed(&self, app_id: String) {
        self.inner.lock().unwrap().input_method_destroyed(app_id);
    }

    /// Return the input method app id
    pub fn im_app_id(&self) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        inner.im_app_id()
    }

    /// Advance the focus for the client to `surface`.
    ///
    /// This doesn't send any 'enter' or 'leave' events.
    pub fn set_focus(&self, surface: Option<WlSurface>) {
        self.inner.lock().unwrap().focus = surface;
    }

    /// Send `leave` on the text-input instance for the currently focused
    /// surface.
    pub fn leave(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.with_focused_instance(|text_input, focus| {
            text_input.object.leave(focus);
        });
    }

    /// Send `enter` on the text-input instance for the currently focused
    /// surface.
    pub fn enter(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.with_focused_instance(|text_input, focus| {
            text_input.object.enter(focus);
        });
    }

    /// The `discard_state` is used when the input-method signaled that
    /// the state should be discarded and wrong serial sent.
    pub fn done(&self, discard_state: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.with_focused_instance(|text_input, _| {
            if discard_state {
                debug!("discarding text-input state due to serial");
                // Discarding is done by sending non-matching serial.
                text_input.object.done(0);
            } else {
                text_input.object.done(text_input.serial);
            }
        });
    }

    /// Access the text-input instance for the currently focused surface.
    pub(crate) fn with_focused_instance<F>(&self, mut f: F)
    where
        F: FnMut(&mut Instance, &WlSurface),
    {
        let mut inner = self.inner.lock().unwrap();
        inner.with_focused_instance(|instance, surface| {
            f(instance, surface);
        });
    }

    /// Call the callback with the serial of the focused text_input or with the passed
    /// `default` one when empty.
    pub(crate) fn focused_text_input_serial_or_default<F>(&self, default: u32, mut callback: F)
    where
        F: FnMut(u32),
    {
        let mut inner = self.inner.lock().unwrap();
        let mut should_default = true;
        inner.with_focused_instance(|ti, _| {
            should_default = false;
            callback(ti.serial);
        });
        if should_default {
            callback(default);
        }
    }
}

/// User data of WpTextInputV3 object
#[derive(Debug)]
pub struct TextInputUserData {
    pub(super) handle: TextInputHandle,
    pub(crate) input_method_handle: InputMethodHandle,
}

impl<D> Dispatch<WpTextInputV3, TextInputUserData, D> for TextInputManagerState
where
    D: Dispatch<WpTextInputV3, TextInputUserData>,
    D: SeatHandler,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &wayland_server::Client,
        resource: &WpTextInputV3,
        request: wp_text_input_v3::Request,
        data: &TextInputUserData,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, D>,
    ) {
        // Always increment serial to not desync with clients.
        if matches!(request, wp_text_input_v3::Request::Commit) {
            data.handle.increment_serial(resource);
        }

        // Discard requests without any active input method instance.
        if !data.input_method_handle.has_instance() {
            debug!("discarding text-input request without IME running");
            return;
        }

        let focus = match data.handle.focus() {
            Some(focus) if focus.id().same_client_as(&resource.id()) => focus,
            _ => {
                debug!("discarding text-input request for unfocused client");
                return;
            }
        };

        let im_app_id = if let Some(app_id) = data.handle.im_app_id() {
            app_id
        } else if let Some(app_id) = data.input_method_handle.currently_set_input_method() {
            app_id
        } else {
            debug!("discarding text-input request with missing app_id for input method");
            return;
        };

        match request {
            wp_text_input_v3::Request::Enable => {
                println!("EnableERGO: {}", im_app_id);
                data.input_method_handle.activate_input_method(&focus, im_app_id)
            }
            wp_text_input_v3::Request::Disable => {
                println!("DisableERGO: {}", im_app_id);
                data.input_method_handle.deactivate_input_method(false, im_app_id);
            }
            wp_text_input_v3::Request::SetSurroundingText { text, cursor, anchor } => {
                data.input_method_handle.with_instance(im_app_id, |input_method| {
                    input_method
                        .object
                        .surrounding_text(text.clone(), cursor as u32, anchor as u32)
                });
            }
            wp_text_input_v3::Request::SetTextChangeCause { cause } => {
                data.input_method_handle.with_instance(im_app_id, |input_method| {
                    input_method
                        .object
                        .text_change_cause(cause.into_result().unwrap())
                });
            }
            wp_text_input_v3::Request::SetContentType { hint, purpose } => {
                data.input_method_handle.with_instance(im_app_id, |input_method| {
                    input_method
                        .object
                        .content_type(hint.into_result().unwrap(), purpose.into_result().unwrap());
                });
            }
            wp_text_input_v3::Request::SetCursorRectangle { x, y, width, height } => {
                println!("Setting cursor rectangle");
                data.input_method_handle.with_instance(im_app_id, |input_method| {
                    input_method.object.cursor_rectangle(x, y, width, height)
                })
            }
            wp_text_input_v3::Request::Commit => {
                println!("Committing");
                data.input_method_handle.with_instance(im_app_id, |input_method| {
                    input_method.done();
                });
            }
            wp_text_input_v3::Request::Destroy => {
                // Nothing to do
            }
            wp_text_input_v3::Request::ProcessKeys { serial } => {
                let im_inner = data.input_method_handle.inner.lock().unwrap();
                let key_index = im_inner.keys.iter().position(|key| key.3 == serial);
                if let Some(key_index) = key_index {
                    data.handle
                        .focused_text_input_serial_or_default(serial, |serial| {
                            let instance = im_inner.instances.iter().find(|inst| inst.app_id == im_app_id);
                            if let Some(instance) = instance {
                                for i in key_index..min(BUFFERSIZE, im_inner.keys.len()) {
                                    let key = im_inner.keys[i];
                                    instance.object.key(serial, key.4, key.0, key.1.into());
                                    let serialized = key.2.serialized;
                                    instance.object.modifiers(
                                        serial,
                                        serialized.depressed,
                                        serialized.latched,
                                        serialized.locked,
                                        serialized.layout_effective,
                                    )
                                }
                            }
                        });
                }
            }
            wp_text_input_v3::Request::SetAvailableActions { available_actions } => {
                data.input_method_handle.with_instance(im_app_id, |input_method| {
                    input_method.object.available_actions(available_actions.clone())
                })
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(_state: &mut D, _client: ClientId, text_input: &WpTextInputV3, data: &TextInputUserData) {
        let mut inner = data.handle.inner.lock().unwrap();
        let im_app_id = data.handle.im_app_id();
        let deactivate_im = {
            let destroyed_id = text_input.id();
            inner.instances.retain(|inst| inst.object.id() != destroyed_id);
            let destroyed_focused = inner
                .focus
                .as_ref()
                .map(|focus| focus.id().same_client_as(&destroyed_id))
                .unwrap_or(true);

            // Deactivate IM when we either lost focus entirely or destroyed text-input for the
            // currently focused client.
            destroyed_focused
                && !inner
                    .instances
                    .iter()
                    .any(|inst| inst.object.id().same_client_as(&destroyed_id))
                && im_app_id.is_some()
        };

        if deactivate_im {
            data.input_method_handle
                .deactivate_input_method(true, im_app_id.unwrap());
        }
    }
}
