use std::mem;
use std::sync::{Arc, Mutex};

use tracing::{debug, warn};
use wl_input_method as wayland_protocols_experimental;
use wayland_protocols_experimental::text_input::v3::server::xx_text_input_v3::{
    self, ChangeCause, ContentHint, ContentPurpose, SupportedFeatures, XxTextInputV3,
};
use wayland_server::backend::{ClientId, ObjectId};
use wayland_server::{protocol::wl_surface::WlSurface, Dispatch, Resource};

use crate::input::SeatHandler;
use crate::utils::{Logical, Rectangle};
use crate::wayland::input_method;
use crate::wayland::input_method_v3;

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
        F: FnMut(&XxTextInputV3, &WlSurface, u32),
    {
        if let Some(surface) = dbg!(self.focus.as_ref()).filter(|surface| dbg!(surface.is_alive())) {
            for text_input in self.instances.iter() {
                let instance_id = text_input.instance.id();
                if instance_id.same_client_as(&surface.id()) {
                    f(&text_input.instance, surface, text_input.serial);
                    break;
                }
            }
        };
    }

    fn with_active_text_input<F>(&mut self, mut f: F)
    where
        F: FnMut(&XxTextInputV3, &WlSurface, u32),
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
            f(&text_input.instance, surface, text_input.serial);
        }
    }
}

/// Handle to text input instances
#[derive(Default, Debug, Clone)]
pub struct TextInputHandle {
    pub(crate) inner: Arc<Mutex<TextInput>>,
}

impl TextInputHandle {
    pub(super) fn add_instance(&self, instance: &XxTextInputV3) {
        let mut inner = self.inner.lock().unwrap();
        inner.instances.push(Instance {
            instance: instance.clone(),
            serial: 0,
            pending_update: Default::default(),
        });
    }

    fn increment_serial(&self, text_input: &XxTextInputV3) {
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
        inner.with_focused_client_all_text_inputs(|text_input, focus, _| {
            text_input.leave(focus);
        });
    }

    /// Send `enter` on the text-input instance for the currently focused
    /// surface.
    pub fn enter(&self) {
        let mut inner = self.inner.lock().unwrap();
        // NOTE: protocol states that if we have multiple text inputs enabled, `enter` must
        // be send for each of them.
        inner.with_focused_client_all_text_inputs(|text_input, focus, _| {
            text_input.enter(focus);
        });
    }

    /// The `discard_state` is used when the input-method signaled that
    /// the state should be discarded and wrong serial sent.
    /// Returns `true` if event was sent
    pub fn done(&self, discard_state: bool) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let mut sent = false;
        inner.with_active_text_input(|text_input, _, serial| {
            if discard_state {
                debug!("discarding text-input state due to serial");
                // Discarding is done by sending non-matching serial.
                text_input.done(0);
            } else {
                text_input.done(serial);
            };
            sent = true;
        });
        sent
    }

    /// Access the text-input instance for the currently focused surface.
    pub fn with_focused_text_input<F>(&self, mut f: F)
    where
        F: FnMut(&XxTextInputV3, &WlSurface),
    {
        let mut inner = self.inner.lock().unwrap();
        inner.with_focused_client_all_text_inputs(|ti, surface, _| {
            f(ti, surface);
        });
    }

    /// Access the active text-input instance for the currently focused surface.
    pub fn with_active_text_input<F>(&self, mut f: F)
    where
        F: FnMut(&XxTextInputV3, &WlSurface),
    {
        let mut inner = self.inner.lock().unwrap();
        inner.with_active_text_input(|ti, surface, _| {
            f(ti, surface);
        });
    }

    /// Call the callback with the serial of the active text_input or with the passed
    /// `default` one when empty.
    // TODO: only used in input method v2
    pub(crate) fn active_text_input_serial_or_default<F>(&self, default: u32, mut callback: F)
    where
        F: FnMut(u32),
    {
        let mut inner = self.inner.lock().unwrap();
        let mut should_default = true;
        inner.with_active_text_input(|_, _, serial| {
            should_default = false;
            callback(serial);
        });
        if should_default {
            callback(default)
        }
    }
}

/// User data of XxTextInputV3 object
#[derive(Debug)]
pub struct TextInputUserData {
    pub(super) handle: TextInputHandle,
    pub(crate) input_method_handle: input_method::InputMethodHandle,
    pub(crate) input_method_v3_handle: input_method_v3::InputMethodHandle,
}

impl<D> Dispatch<XxTextInputV3, TextInputUserData, D> for TextInputManagerState
where
    D: Dispatch<XxTextInputV3, TextInputUserData>,
    D: SeatHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &wayland_server::Client,
        resource: &XxTextInputV3,
        request: xx_text_input_v3::Request,
        data: &TextInputUserData,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, D>,
    ) {
        // Always increment serial to not desync with clients.
        if matches!(request, xx_text_input_v3::Request::Commit) {
            data.handle.increment_serial(resource);
        }

        // Discard requests without any active input method instance.
        if !data.input_method_handle.has_instance() && !data.input_method_v3_handle.has_instance() {
            debug!("discarding text-input request without IME running");
            return;
        }

        if data.input_method_handle.has_instance() && data.input_method_v3_handle.has_instance() {
            warn!("Two separate versions of input method registered for the seat. Expect conflicts.");
            // We'll try to drive both IM instances because it makes the code simpler. The results are going to be unexpected no matter what strategy is chosen now.
        }

        let focus = match data.handle.focus() {
            Some(focus) if focus.id().same_client_as(&resource.id()) => focus,
            _ => {
                debug!("discarding text-input request for unfocused client");
                return;
            }
        };

        let mut guard = data.handle.inner.lock().unwrap();
        let pending_update = match guard.instances.iter_mut().find_map(|instance| {
            if instance.instance == *resource {
                Some(&mut instance.pending_update)
            } else {
                None
            }
        }) {
            Some(pending_update) => pending_update,
            None => {
                debug!("got request for untracked text-input");
                return;
            }
        };

        use xx_text_input_v3::Request::*;
        match dbg!(request) {
            Enable => {
                pending_update.enable = Some(true);
            }
            Disable => {
                pending_update.enable = Some(false);
            }
            SetSurroundingText { text, cursor, anchor } => {
                pending_update.surrounding_text = Some((text, cursor as u32, anchor as u32));
            }
            SetTextChangeCause { cause } => {
                pending_update.text_change_cause = Some(cause.into_result().unwrap());
            }
            SetContentType { hint, purpose } => {
                pending_update.content_type =
                    Some((hint.into_result().unwrap(), purpose.into_result().unwrap()));
            }
            SetCursorRectangle { x, y, width, height } => {
                pending_update.cursor_rectangle = Some(Rectangle::new((x, y).into(), (width, height).into()));
            }
            AnnounceSupportedFeatures { features } => {
                pending_update.supported_features = Some(
                    features.into_result()
                        .map_err(|value| warn!("Unknown `features`: {value}. Assuming no extra features supported."))
                        .unwrap_or(SupportedFeatures::empty())
                );
            }
            SetAvailableActions { available_actions } => {
                pending_update.available_actions = Some(available_actions);
            }
            Commit => {
                let mut update = mem::take(pending_update);
                let _ = pending_update;
                let active_text_input_id = &mut guard.active_text_input_id;

                if active_text_input_id.is_some() && *active_text_input_id != Some(resource.id()) {
                    dbg!("exit");
                    debug!("discarding text_input request since we already have an active one");
                    return;
                }

                match update.enable {
                    Some(true) => {
                        *active_text_input_id = Some(resource.id());
                        // Drop the guard before calling to other subsystem.
                        drop(guard);
                        data.input_method_handle.activate_input_method(state, &focus);
                        data.input_method_v3_handle.activate_input_method(state, &focus);
                    }
                    Some(false) => {
                        *active_text_input_id = None;
                        // Drop the guard before calling to other subsystem.
                        drop(guard);
                        data.input_method_handle.deactivate_input_method(state);
                        data.input_method_v3_handle.deactivate_input_method(state);
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

                if let Some((text, cursor, anchor)) = update.surrounding_text.take() {
                    data.input_method_handle.with_instance(|input_method| {
                        input_method.object.surrounding_text(text.clone(), cursor, anchor)
                    });
                    dbg!("surround");
                    data.input_method_v3_handle.with_instance(move |input_method| {
                        dbg!("surround in");
                        input_method.object.surrounding_text(text, cursor, anchor)
                    });
                }

                if let Some(cause) = update.text_change_cause.take() {
                    data.input_method_handle.with_instance(move |input_method| {
                        input_method.set_text_change_cause(cause);
                    });
                    data.input_method_v3_handle.with_instance(move |input_method| {
                        input_method.set_text_change_cause(cause);
                    });
                }

                if let Some((hint, purpose)) = update.content_type.take() {
                    data.input_method_handle.with_instance(move |input_method| {
                        input_method.set_content_type(hint, purpose);
                    });
                    data.input_method_v3_handle.with_instance(move |input_method| {
                        input_method.set_content_type(hint, purpose);
                    });
                }

                if let Some(rect) = update.cursor_rectangle.take() {
                    data.input_method_handle
                        .set_text_input_rectangle::<D>(state, rect);
                    data.input_method_v3_handle.set_cursor_rectangle::<D>(state, rect);
                }

                if let Some(actions) = update.available_actions.take() {
                    data.input_method_v3_handle.with_instance(move |input_method| {
                        input_method.object.set_available_actions(actions);
                    });
                }
                
                if let Some(features) = update.supported_features.take() {
                    data.input_method_v3_handle.with_instance(move |input_method| {
                        input_method.object.announce_supported_features(features);
                    });
                }

                data.input_method_handle.with_instance(|input_method| {
                    input_method.done();
                });
                data.input_method_v3_handle.done();
            }
            Destroy => {
                // Nothing to do
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut D, _client: ClientId, text_input: &XxTextInputV3, data: &TextInputUserData) {
        let destroyed_id = text_input.id();
        let deactivate_im = {
            let mut inner = data.handle.inner.lock().unwrap();
            inner.instances.retain(|inst| inst.instance.id() != destroyed_id);
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
                    .any(|inst| inst.instance.id().same_client_as(&destroyed_id))
        };

        if deactivate_im {
            data.input_method_handle.deactivate_input_method(state);
            data.input_method_v3_handle.deactivate_input_method(state);
        }
    }
}

#[derive(Debug)]
struct Instance {
    instance: XxTextInputV3,
    serial: u32,
    pending_update: TextInputStateChange,
}

#[derive(Debug, Default)]
struct TextInputStateChange {
    enable: Option<bool>,
    surrounding_text: Option<(String, u32, u32)>,
    content_type: Option<(ContentHint, ContentPurpose)>,
    cursor_rectangle: Option<Rectangle<i32, Logical>>,
    text_change_cause: Option<ChangeCause>,
    available_actions: Option<Vec<u8>>,
    supported_features: Option<SupportedFeatures>,
}