use std::mem;
use std::sync::{Arc, Mutex};

use tracing::{debug, warn};

use wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3;
use wayland_server::backend::{ClientId, ObjectId};
use wayland_server::{protocol::wl_surface::WlSurface, Dispatch, Resource};
use zwp_text_input_v3::{ChangeCause, ContentHint, ContentPurpose, ZwpTextInputV3};

use crate::input::SeatHandler;
use crate::utils::{Logical, Rectangle};
use crate::wayland::compositor::{self, HookId};
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
        F: FnMut(&ZwpTextInputV3, &WlSurface, u32),
    {
        if let Some(surface) = self.focus.as_ref().filter(|surface| surface.is_alive()) {
            for text_input in self.instances.iter() {
                let instance_id = text_input.instance.id();
                if instance_id.same_client_as(&surface.id()) {
                    f(&text_input.instance, surface, text_input.serial);
                }
            }
        };
    }

    fn with_active_text_input<F>(&mut self, mut f: F)
    where
        F: FnMut(&ZwpTextInputV3, &WlSurface, u32),
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
    pub(super) fn add_instance(&self, instance: &ZwpTextInputV3) {
        let mut inner = self.inner.lock().unwrap();
        inner.instances.push(Instance {
            instance: instance.clone(),
            serial: 0,
            pending_state: Default::default(),
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

    /// Return the currently focused surface.
    pub fn focus(&self) -> Option<WlSurface> {
        self.inner.lock().unwrap().focus.clone()
    }

    /// Returns true if a text input client has enabled text input (sent enable+commit).
    pub fn has_active_text_input(&self) -> bool {
        self.inner.lock().unwrap().active_text_input_id.is_some()
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
            if text_input.version() >= 2 {
                let data = text_input.data::<TextInputUserData>().unwrap();
                let mut hook = data.surface_commit_hook.lock().unwrap();
                if let Some(hook) = hook.take() {
                    compositor::remove_post_commit_hook(&focus, &hook);
                }
            }
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

    /// Access the text-input instances for the currently focused surface.
    pub fn with_focused_text_input<F>(&self, mut f: F)
    where
        F: FnMut(&ZwpTextInputV3, &WlSurface),
    {
        let mut inner = self.inner.lock().unwrap();
        inner.with_focused_client_all_text_inputs(|ti, surface, _| {
            f(ti, surface);
        });
    }

    /// Access the active text-input instance for the currently focused surface.
    pub fn with_active_text_input<F>(&self, mut f: F)
    where
        F: FnMut(&ZwpTextInputV3, &WlSurface),
    {
        let mut inner = self.inner.lock().unwrap();
        inner.with_active_text_input(|ti, surface, _| {
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
        inner.with_active_text_input(|_, _, serial| {
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
    pub(crate) input_method_handle: input_method::InputMethodHandle,
    pub(crate) input_method_v3_handle: input_method_v3::InputMethodHandle,
    /// For version 2 and above, this associates the text-input to a surface.
    /// wl_surface.commit triggers the text-input state update.
    /// This holds the post-commit hook id that does the state update.
    /// This `HookId` makes it possible to unregister the hook
    /// and stop updates when text-input is disabled.
    pub(super) surface_commit_hook: Mutex<Option<HookId>>,
}

impl<D> Dispatch<ZwpTextInputV3, TextInputUserData, D> for TextInputManagerState
where
    D: Dispatch<ZwpTextInputV3, TextInputUserData>,
    D: SeatHandler,
    D: input_method_v3::InputMethodHandler,
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
            Some(focus) => {
                debug!(
                    "discarding text-input request for unfocused client: focus={:?} resource={:?}",
                    focus.id(),
                    resource.id()
                );
                return;
            }
            None => {
                debug!("discarding text-input request: no focus set");
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
            Some(value) => value,
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
                // Guard against clients sending us unknown values from future versions.
                let cause = cause.into_result().unwrap_or(ChangeCause::Other);
                pending_state.text_change_cause = Some(cause);
            }
            zwp_text_input_v3::Request::SetContentType { hint, purpose } => {
                // Guard against clients sending us unknown values from future versions.
                let hint = ContentHint::from_bits_truncate(u32::from(hint));
                let purpose = purpose.into_result().unwrap_or(ContentPurpose::Normal);
                pending_state.content_type = Some((hint, purpose));
            }
            zwp_text_input_v3::Request::SetCursorRectangle { x, y, width, height } => {
                pending_state.cursor_rectangle = Some(Rectangle::new((x, y).into(), (width, height).into()));
            }
            zwp_text_input_v3::Request::Commit => {
                let mut new_state = mem::take(pending_state);
                let active_text_input_id = &mut guard.active_text_input_id;

                if active_text_input_id.is_some() && *active_text_input_id != Some(resource.id()) {
                    debug!("discarding text_input request since we already have an active one");
                    return;
                }

                match new_state.enable {
                    Some(true) => {
                        *active_text_input_id = Some(resource.id());
                        data.input_method_handle.activate_input_method(state, &focus);
                        data.input_method_v3_handle.activate_input_method(state, &focus);
                    }
                    Some(false) => {
                        *active_text_input_id = None;
                        data.input_method_handle.deactivate_input_method(state);
                        data.input_method_v3_handle.deactivate_input_method(state);
                        return;
                    }
                    None => {
                        if *active_text_input_id != Some(resource.id()) {
                            debug!("discarding text_input requests before enabling it");
                            return;
                        }
                    }
                }
                drop(guard);
                if let Some((text, cursor, anchor)) = new_state.surrounding_text.take() {
                    data.input_method_handle.with_instance(|input_method| {
                        input_method.object.surrounding_text(text.clone(), cursor, anchor)
                    });
                    data.input_method_v3_handle.with_instance(move |input_method| {
                        input_method.object.surrounding_text(text, cursor, anchor)
                    });
                }

                if let Some(cause) = new_state.text_change_cause.take() {
                    let cause = match cause {
                        ChangeCause::InputMethod => zwp_text_input_v3::ChangeCause::InputMethod,
                        ChangeCause::Other => zwp_text_input_v3::ChangeCause::Other,
                        _ => zwp_text_input_v3::ChangeCause::Other,
                    };
                    data.input_method_handle.with_instance(move |input_method| {
                        input_method.object.text_change_cause(cause);
                    });
                    data.input_method_v3_handle.with_instance(move |input_method| {
                        input_method.object.text_change_cause(cause);
                    });
                }

                if let Some((hint, purpose)) = new_state.content_type.take() {
                    data.input_method_handle.with_instance(move |input_method| {
                        input_method.object.content_type(hint, purpose);
                    });
                    data.input_method_v3_handle.with_instance(move |input_method| {
                        input_method.object.content_type(hint, purpose);
                    });
                }

                if let Some(actions) = new_state.available_actions.take() {
                    let action_bytes: Vec<u8> = actions.iter().flat_map(|a| a.to_ne_bytes()).collect();
                    data.input_method_v3_handle.with_instance(move |input_method| {
                        input_method.object.set_available_actions(action_bytes);
                    });
                }

                let cursor_state = new_state.cursor_rectangle.take();
                if let Some(rect) = cursor_state {
                    data.input_method_handle
                        .set_text_input_rectangle::<D>(state, rect);
                    data.input_method_v3_handle.set_cursor_rectangle::<D>(state, rect);
                }

                data.input_method_handle.with_instance(|input_method| {
                    input_method.done();
                });
                data.input_method_v3_handle.done();
            }
            zwp_text_input_v3::Request::SetAvailableActions { available_actions } => {
                let actions: Vec<u32> = available_actions
                    .chunks_exact(4)
                    .map(|chunk| u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                    .collect();
                pending_state.available_actions = Some(actions);
            }
            zwp_text_input_v3::Request::ShowInputPanel => {
                pending_state.show_input_panel = true;
            }
            zwp_text_input_v3::Request::HideInputPanel => {
                pending_state.hide_input_panel = true;
            }
            zwp_text_input_v3::Request::Destroy => {
                // Nothing to do
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut D, _client: ClientId, text_input: &ZwpTextInputV3, data: &TextInputUserData) {
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
    instance: ZwpTextInputV3,
    serial: u32,
    pending_state: TextInputState,
}

/// State of the text_input object set on text-input.commit
#[derive(Debug, Default, Clone)]
struct TextInputState {
    enable: Option<bool>,
    surrounding_text: Option<(String, u32, u32)>,
    content_type: Option<(ContentHint, ContentPurpose)>,
    cursor_rectangle: Option<Rectangle<i32, Logical>>,
    text_change_cause: Option<ChangeCause>,
    /// Available actions (since v3.2). Each u32 is an action enum value.
    available_actions: Option<Vec<u32>>,
    /// Show input panel requested (since v3.2).
    show_input_panel: bool,
    /// Hide input panel requested (since v3.2).
    hide_input_panel: bool,
}
