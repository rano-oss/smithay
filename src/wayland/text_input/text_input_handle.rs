use std::mem;
use std::sync::{Arc, Mutex};

use tracing::debug;
use wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3::{
    self, ChangeCause, ContentHint, ContentPurpose, ZwpTextInputV3,
};
use wayland_server::backend::{ClientId, ObjectId};
use wayland_server::{protocol::wl_surface::WlSurface, Dispatch, Resource};

use crate::input::SeatHandler;
use crate::utils::{Logical, Rectangle};
use crate::wayland::input_method::InputMethodHandle;

use super::TextInputManagerState;

#[derive(Default, Debug)]
pub(crate) struct TextInput {
    instances: Vec<Instance>,
    focus: Option<WlSurface>,
    active_text_input_id: Option<ObjectId>,
}

impl TextInput {
    fn with_focused_client_all_text_inputs<F>(&mut self, mut f: F)
    where
        F: FnMut(&ZwpTextInputV3, &WlSurface, u32, &InputMethodHandle),
    {
        if let Some(surface) = self.focus.as_ref().filter(|surface| surface.is_alive()) {
            for text_input in self.instances.iter() {
                let instance_id = text_input.instance.id();
                if instance_id.same_client_as(&surface.id()) {
                    f(
                        &text_input.instance,
                        surface,
                        text_input.serial,
                        &text_input.input_method_handle,
                    );
                    break;
                }
            }
        };
    }

    fn with_active_text_input<F>(&mut self, mut f: F)
    where
        F: FnMut(&ZwpTextInputV3, &WlSurface, u32, &InputMethodHandle),
    {
        let active_id = match &self.active_text_input_id {
            Some(active_text_input_id) => active_text_input_id,
            None => return,
        };

        let surface = match self.focus.as_ref().filter(|surface| surface.is_alive()) {
            Some(surface) => surface,
            None => return,
        };

        let surface_id = surface.id();
        if let Some(text_input) = self
            .instances
            .iter()
            .filter(|instance| instance.instance.id().same_client_as(&surface_id))
            .find(|instance| &instance.instance.id() == active_id)
        {
            f(
                &text_input.instance,
                surface,
                text_input.serial,
                &text_input.input_method_handle,
            );
        }
    }
}

/// Handle to text input instances
#[derive(Default, Debug, Clone)]
pub struct TextInputHandle {
    pub(crate) inner: Arc<Mutex<TextInput>>,
}

impl TextInputHandle {
    pub(super) fn add_instance(&self, instance: &ZwpTextInputV3, input_method_handle: InputMethodHandle) {
        let mut inner = self.inner.lock().unwrap();
        inner.instances.push(Instance {
            instance: instance.clone(),
            serial: 0,
            pending_state: Default::default(),
            input_method_handle,
        });
    }

    fn increment_serial(&self, text_input: &ZwpTextInputV3) {
        if let Some(instance) = self
            .inner
            .lock()
            .unwrap()
            .instances
            .iter_mut()
            .find(|instance| instance.instance == *text_input)
        {
            instance.serial += 1
        }
    }

    /// Set the input method handle for a specific text input instance.
    /// Returns true if the instance was found and updated.
    pub fn set_input_method(&self, text_input_id: ObjectId, input_method_handle: InputMethodHandle) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if let Some(instance) = inner
            .instances
            .iter_mut()
            .find(|inst| inst.instance.id() == text_input_id)
        {
            instance.input_method_handle = input_method_handle;
            true
        } else {
            false
        }
    }

    /// Get the input method handle for a specific text input instance.
    pub fn get_input_method(&self, text_input_id: &ObjectId) -> Option<InputMethodHandle> {
        let inner = self.inner.lock().unwrap();
        inner
            .instances
            .iter()
            .find(|inst| inst.instance.id() == *text_input_id)
            .map(|inst| inst.input_method_handle.clone())
    }

    /// Get the input method handle for the currently active text input.
    pub fn get_active_input_method(&self) -> Option<InputMethodHandle> {
        let inner = self.inner.lock().unwrap();
        let active_id = inner.active_text_input_id.as_ref()?;
        inner
            .instances
            .iter()
            .find(|inst| inst.instance.id() == *active_id)
            .map(|inst| inst.input_method_handle.clone())
    }

    /// Return the currently focused surface.
    pub fn focus(&self) -> Option<WlSurface> {
        self.inner.lock().unwrap().focus.clone()
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
        // Leaving clears the active text input.
        inner.active_text_input_id = None;
        // NOTE: we implement it in a symmetrical way with `enter`.
        inner.with_focused_client_all_text_inputs(|text_input, focus, _, _| {
            text_input.leave(focus);
        });
    }

    /// Send `enter` on the text-input instance for the currently focused
    /// surface.
    pub fn enter(&self) {
        let mut inner = self.inner.lock().unwrap();
        // NOTE: protocol states that if we have multiple text inputs enabled, `enter` must
        // be send for each of them.
        inner.with_focused_client_all_text_inputs(|text_input, focus, _, _| {
            text_input.enter(focus);
        });
    }

    /// The `discard_state` is used when the input-method signaled that
    /// the state should be discarded and wrong serial sent.
    pub fn done(&self, discard_state: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.with_active_text_input(|text_input, _, serial, _| {
            if discard_state {
                debug!("discarding text-input state due to serial");
                // Discarding is done by sending non-matching serial.
                text_input.done(0);
            } else {
                text_input.done(serial);
            }
        });
    }

    /// Access the text-input instance for the currently focused surface.
    pub fn with_focused_text_input<F>(&self, mut f: F)
    where
        F: FnMut(&ZwpTextInputV3, &WlSurface),
    {
        let mut inner = self.inner.lock().unwrap();
        inner.with_focused_client_all_text_inputs(|ti, surface, _, _| {
            f(ti, surface);
        });
    }

    /// Access the active text-input instance for the currently focused surface.
    pub fn with_active_text_input<F>(&self, mut f: F)
    where
        F: FnMut(&ZwpTextInputV3, &WlSurface),
    {
        let mut inner = self.inner.lock().unwrap();
        inner.with_active_text_input(|ti, surface, _, _| {
            f(ti, surface);
        });
    }

    /// Call the callback with the serial of the active text_input or with the passed
    /// `default` one when empty.
    pub(crate) fn active_text_input_serial_or_default<F>(&self, default: u32, mut callback: F)
    where
        F: FnMut(u32),
    {
        let mut inner = self.inner.lock().unwrap();
        let mut should_default = true;
        inner.with_active_text_input(|_, _, serial, _| {
            should_default = false;
            callback(serial);
        });
        if should_default {
            callback(default)
        }
    }
}

/// User data of ZwpTextInputV3 object
#[derive(Debug)]
pub struct TextInputUserData {
    pub(super) handle: TextInputHandle,
}

impl<D> Dispatch<ZwpTextInputV3, TextInputUserData, D> for TextInputManagerState
where
    D: Dispatch<ZwpTextInputV3, TextInputUserData>,
    D: SeatHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &wayland_server::Client,
        resource: &ZwpTextInputV3,
        request: zwp_text_input_v3::Request,
        data: &TextInputUserData,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, D>,
    ) {
        // Always increment serial to not desync with clients.
        if matches!(request, zwp_text_input_v3::Request::Commit) {
            data.handle.increment_serial(resource);
        }

        // Check if there's an active input method for this text input
        let has_active_im = {
            let inner = data.handle.inner.lock().unwrap();
            inner
                .instances
                .iter()
                .find(|inst| inst.instance == *resource)
                .map(|inst| inst.input_method_handle.has_instance())
                .unwrap_or(false)
        };

        if !has_active_im {
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

        let mut guard = data.handle.inner.lock().unwrap();
        let pending_state = match guard.instances.iter_mut().find_map(|instance| {
            if instance.instance == *resource {
                Some(&mut instance.pending_state)
            } else {
                None
            }
        }) {
            Some(pending_state) => pending_state,
            None => {
                debug!("got request for untracked text-input");
                return;
            }
        };

        match request {
            zwp_text_input_v3::Request::Enable => {
                pending_state.enable = Some(true);
            }
            zwp_text_input_v3::Request::Disable => {
                pending_state.enable = Some(false);
            }
            zwp_text_input_v3::Request::SetSurroundingText { text, cursor, anchor } => {
                pending_state.surrounding_text = Some((text, cursor as u32, anchor as u32));
            }
            zwp_text_input_v3::Request::SetTextChangeCause { cause } => {
                pending_state.text_change_cause = Some(cause.into_result().unwrap());
            }
            zwp_text_input_v3::Request::SetContentType { hint, purpose } => {
                pending_state.content_type =
                    Some((hint.into_result().unwrap(), purpose.into_result().unwrap()));
            }
            zwp_text_input_v3::Request::SetCursorRectangle { x, y, width, height } => {
                pending_state.cursor_rectangle = Some(Rectangle::new((x, y).into(), (width, height).into()));
            }
            zwp_text_input_v3::Request::Commit => {
                let mut new_state = mem::take(pending_state);

                // Get input_method_handle
                let input_method_handle = guard
                    .instances
                    .iter()
                    .find(|inst| inst.instance == *resource)
                    .map(|inst| inst.input_method_handle.clone());

                let active_text_input_id = &mut guard.active_text_input_id;

                if active_text_input_id.is_some() && *active_text_input_id != Some(resource.id()) {
                    debug!("discarding text_input request since we already have an active one");
                    return;
                }

                match new_state.enable {
                    Some(true) => {
                        *active_text_input_id = Some(resource.id());
                        // Drop the guard before calling to other subsystem.
                        drop(guard);
                        if let Some(im_handle) = input_method_handle {
                            im_handle.activate_input_method(state, &focus);
                        }
                    }
                    Some(false) => {
                        *active_text_input_id = None;
                        // Drop the guard before calling to other subsystem.
                        drop(guard);
                        if let Some(im_handle) = input_method_handle {
                            im_handle.deactivate_input_method(state);
                        }
                        return;
                    }
                    None => {
                        if *active_text_input_id != Some(resource.id()) {
                            debug!("discarding text_input requests before enabling it");
                            return;
                        }

                        // Drop the guard before calling to other subsystems later on.
                        drop(guard);
                    }
                }

                let input_method_handle = {
                    let inner = data.handle.inner.lock().unwrap();
                    inner
                        .instances
                        .iter()
                        .find(|inst| inst.instance == *resource)
                        .map(|inst| inst.input_method_handle.clone())
                };

                if let Some(im_handle) = input_method_handle {
                    if let Some((text, cursor, anchor)) = new_state.surrounding_text.take() {
                        im_handle.with_instance(|input_method| {
                            input_method
                                .object
                                .surrounding_text(text.to_string(), cursor, anchor)
                        });
                    }

                    if let Some(cause) = new_state.text_change_cause.take() {
                        im_handle.with_instance(|input_method| {
                            input_method.object.text_change_cause(cause);
                        });
                    }

                    if let Some((hint, purpose)) = new_state.content_type.take() {
                        im_handle.with_instance(|input_method| {
                            input_method.object.content_type(hint, purpose);
                        });
                    }

                    if let Some(rect) = new_state.cursor_rectangle.take() {
                        im_handle.set_text_input_rectangle::<D>(state, rect);
                    }

                    im_handle.with_instance(|input_method| {
                        input_method.done();
                    });
                }
            }
            zwp_text_input_v3::Request::Destroy => {
                // Nothing to do
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut D, _client: ClientId, text_input: &ZwpTextInputV3, data: &TextInputUserData) {
        let destroyed_id = text_input.id();
        let (deactivate_im, im_handle) = {
            let mut inner = data.handle.inner.lock().unwrap();
            let im_handle = inner
                .instances
                .iter()
                .find(|inst| inst.instance.id() == destroyed_id)
                .map(|inst| inst.input_method_handle.clone());

            inner.instances.retain(|inst| inst.instance.id() != destroyed_id);
            let destroyed_focused = inner
                .focus
                .as_ref()
                .map(|focus| focus.id().same_client_as(&destroyed_id))
                .unwrap_or(true);

            // Deactivate IM when we either lost focus entirely or destroyed text-input for the
            // currently focused client.
            let should_deactivate = destroyed_focused
                && !inner
                    .instances
                    .iter()
                    .any(|inst| inst.instance.id().same_client_as(&destroyed_id));

            (should_deactivate, im_handle)
        };

        if deactivate_im {
            if let Some(im_handle) = im_handle {
                im_handle.deactivate_input_method(state);
            }
        }
    }
}

#[derive(Debug)]
struct Instance {
    instance: ZwpTextInputV3,
    serial: u32,
    pending_state: TextInputState,
    input_method_handle: InputMethodHandle,
}

#[derive(Debug, Default)]
struct TextInputState {
    enable: Option<bool>,
    surrounding_text: Option<(String, u32, u32)>,
    content_type: Option<(ContentHint, ContentPurpose)>,
    cursor_rectangle: Option<Rectangle<i32, Logical>>,
    text_change_cause: Option<ChangeCause>,
}
