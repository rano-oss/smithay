use std::{
    fmt,
    sync::{Arc, Mutex},
};

use wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3::Action;
use wayland_protocols_experimental::input_method::v1::server::{
    xx_input_method_v1::{self, ProtocolCompat, XxInputMethodV1},
    xx_input_popup_surface_v2::XxInputPopupSurfaceV2,
};
use wayland_server::{
    backend::{ClientId, ObjectId},
    protocol::wl_surface::WlSurface,
};
use wayland_server::{Client, DataInit, DisplayHandle, Resource};

use crate::{
    input::{keyboard::KeyboardHandle, Seat, SeatHandler},
    utils::{Logical, Rectangle},
    wayland::{compositor, keyboard_filter, seat::WaylandFocus, text_input::TextInputHandle, Dispatch2},
};

use super::{
    input_method_popup_surface::{ImPopupLocation, PopupParent, PopupSurface},
    positioner::PositionerUserData,
    InputMethodHandler, InputMethodPopupSurfaceUserData, INPUT_POPUP_SURFACE_ROLE,
};

/// Contains all input method instances and tracks which one is active.
#[derive(Default, Debug)]
pub(crate) struct InputMethodState {
    /// All registered input method instances.
    pub instances: Vec<InputMethod>,
    /// The object ID of the currently active input method instance.
    pub active_input_method_id: Option<ObjectId>,
}

impl InputMethodState {
    /// Get a mutable reference to the active input method instance, if any.
    pub fn active_instance_mut(&mut self) -> Option<&mut InputMethod> {
        let id = self.active_input_method_id.as_ref()?;
        self.instances.iter_mut().find(|i| i.object.id() == *id)
    }

    /// Get a reference to the active input method instance, if any.
    pub fn active_instance(&self) -> Option<&InputMethod> {
        let id = self.active_input_method_id.as_ref()?;
        self.instances.iter().find(|i| i.object.id() == *id)
    }
}

/// Contains input method state
pub(crate) struct InputMethod {
    pub object: XxInputMethodV1,
    pub serial: u32,
    pub active: bool,
    pub app_id: String,
    pub popup_handles: Vec<PopupSurface>,
    /// Relative to surface on which input method is enabled
    pub cursor_rectangle: Rectangle<i32, Logical>,
}

impl fmt::Debug for InputMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputMethod")
            .field("object", &self.object)
            .field("serial", &self.serial)
            .field("active", &self.active)
            .field("app_id", &self.app_id)
            .field("popup_handles", &self.popup_handles)
            .field("cursor_rectangle", &self.cursor_rectangle)
            .finish()
    }
}

impl InputMethod {
    /// Send the done incrementing the serial.
    pub(crate) fn done(&mut self) {
        self.object.done();
        self.serial += 1;
    }
}

/// Handle to a possible input method instance.
#[derive(Default, Debug, Clone)]
pub struct InputMethodHandle {
    pub(crate) inner: Arc<Mutex<InputMethodState>>,
}

impl InputMethodHandle {
    /// Assigns a new instance with the given app_id.
    pub(super) fn add_instance(&self, instance: &XxInputMethodV1, app_id: String) {
        let mut inner = self.inner.lock().unwrap();
        inner.instances.push(InputMethod {
            object: instance.clone(),
            serial: 0,
            active: false,
            app_id,
            popup_handles: vec![],
            cursor_rectangle: Rectangle::default(),
        });
    }

    /// Whether there's any registered input method instance available.
    pub(crate) fn has_instance(&self) -> bool {
        !self.inner.lock().unwrap().instances.is_empty()
    }

    /// List all registered input method app_ids.
    pub fn list_instances(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap()
            .instances
            .iter()
            .map(|i| i.app_id.clone())
            .collect()
    }

    /// Callback function to access the active input method instance.
    pub(crate) fn with_instance<F>(&self, f: F)
    where
        F: FnOnce(&mut InputMethod),
    {
        let mut inner = self.inner.lock().unwrap();
        if let Some(instance) = inner.active_instance_mut() {
            f(instance);
        }
    }

    /// Returns the serial of the active instance, or 0 if none.
    pub(crate) fn active_serial(&self) -> u32 {
        self.inner
            .lock()
            .unwrap()
            .active_instance()
            .map(|i| i.serial)
            .unwrap_or(0)
    }

    /// Set which input method instance should be active by app_id.
    /// Returns true if an instance with the given app_id was found and set as active.
    pub fn set_active_instance(&self, app_id: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();

        if let Some(instance) = inner.instances.iter().find(|i| i.app_id == app_id) {
            let object_id = instance.object.id();
            let old_active = inner.active_input_method_id.clone();

            // If switching to a different instance, deactivate old
            if old_active.as_ref() != Some(&object_id) {
                if let Some(old_id) = old_active {
                    if let Some(old_inst) = inner.instances.iter_mut().find(|i| i.object.id() == old_id) {
                        old_inst.object.deactivate();
                        old_inst.done();
                        old_inst.active = false;
                    }
                }
            }

            inner.active_input_method_id = Some(object_id);
            true
        } else {
            false
        }
    }

    /// Clear the active input method instance.
    pub fn clear_active_instance<D: SeatHandler + 'static>(&self, state: &mut D) {
        self.deactivate_input_method(state);
        let mut inner = self.inner.lock().unwrap();
        inner.active_input_method_id = None;
    }

    pub(crate) fn set_cursor_rectangle<D: SeatHandler + InputMethodHandler + 'static>(
        &self,
        state: &mut D,
        cursor: Rectangle<i32, Logical>,
    ) {
        let mut inner = self.inner.lock().unwrap();
        let Some(instance) = inner.active_instance_mut() else {
            return;
        };
        instance.cursor_rectangle = cursor;

        // Reposition popup(s) unless frozen
        let popups_to_reposition: Vec<_> = instance
            .popup_handles
            .iter_mut()
            .filter(|popup| !popup.frozen())
            .map(|popup| {
                let positioner = popup.positioner();
                let parent_surface = popup.get_parent().surface.clone();
                let popup_geometry = state.popup_geometry(&parent_surface, &cursor, &positioner);
                popup.set_position(ImPopupLocation {
                    anchor: cursor,
                    geometry: popup_geometry,
                });
                popup.clone()
            })
            .collect();

        for popup in popups_to_reposition {
            state.popup_repositioned(popup);
        }
    }

    /// Send `done` to the active input method instance, incrementing its serial.
    pub fn done(&self) {
        let mut inner = self.inner.lock().unwrap();
        let Some(instance) = inner.active_instance_mut() else {
            return;
        };
        for popup_surface in &mut instance.popup_handles {
            popup_surface.send_pending_configure();
        }
        instance.done();
    }

    /// Activate input method on the given surface.
    pub fn activate_input_method<D: SeatHandler + 'static>(&self, _state: &mut D, surface: &WlSurface) {
        self.with_instance(|im| {
            tracing::debug!(
                app_id = %im.app_id,
                serial = im.serial,
                "activate_input_method: activating IM and installing keyboard filter interceptor"
            );
            im.object.activate();
            im.object.announce_protocol_compat(ProtocolCompat::TextInputV3);
            let data = im.object.data::<InputMethodUserData<D>>().unwrap();
            let filter = data.keyboard_filter.lock().unwrap();
            if let Some(keyboard_filter) = filter.as_ref() {
                keyboard_filter.activate_interceptor(&data.seat, surface);
            }
            im.active = true;
        });
    }

    /// Deactivate the active input method.
    ///
    /// This includes a complete sequence including .done.
    /// Also clears any active preedit on the text-input client so the app
    /// doesn't keep showing stale preedit text after the IM is gone.
    pub fn deactivate_input_method<D: SeatHandler + 'static>(&self, state: &mut D) {
        self.with_instance(|im| {
            im.object.deactivate();
            im.done();
            im.active = false;
            let data = im.object.data::<InputMethodUserData<D>>().unwrap();
            // Clear preedit on the text-input client so the app stops showing it.
            data.text_input_handle.with_active_text_input(|ti, _surface| {
                ti.preedit_string(None, -1, -1);
            });
            // Send done so the client applies the cleared preedit.
            data.text_input_handle.done(false);

            for popup in im.popup_handles.drain(..) {
                (data.dismiss_popup)(state, popup.clone());
            }
            let filter = data.keyboard_filter.lock().unwrap();
            if let Some(keyboard_filter) = filter.as_ref() {
                keyboard_filter.deactivate_interceptor(&data.seat);
            }
        });
    }
}

/// User data of XxInputMethodV1 object
#[derive(Clone)]
pub struct InputMethodUserData<D: SeatHandler> {
    pub(crate) seat: Seat<D>,
    pub(super) handle: InputMethodHandle,
    pub(crate) text_input_handle: TextInputHandle,
    /// Handle to main keyboard for registering sub-keyboards
    pub(crate) keyboard_handle: KeyboardHandle<D>,
    /// Currently bound keyboard filter, set by the keyboard_filter protocol.
    pub(crate) keyboard_filter: Arc<Mutex<Option<keyboard_filter::Filter>>>,
    /// This is just a copy from InputMethodHandler. It's here in order to break the requirement for D: InputMethodHandler on functions that call dismiss_popup.
    pub(crate) dismiss_popup: fn(&mut D, PopupSurface),
}

impl<D: SeatHandler> fmt::Debug for InputMethodUserData<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputMethodUserData")
            .field("handle", &self.handle)
            .field("text_input_handle", &self.text_input_handle)
            .finish()
    }
}

impl<D> Dispatch2<XxInputMethodV1, D> for InputMethodUserData<D>
where
    D: SeatHandler,
    D: InputMethodHandler,
    D: wayland_server::Dispatch<XxInputPopupSurfaceV2, InputMethodPopupSurfaceUserData>,
    <D as SeatHandler>::KeyboardFocus: WaylandFocus,
    D: 'static,
{
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        im: &XxInputMethodV1,
        request: xx_input_method_v1::Request,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        use xx_input_method_v1::Request;
        match request {
            Request::CommitString { text } => {
                self.text_input_handle.with_active_text_input(|ti, _surface| {
                    ti.commit_string(Some(text.clone()));
                });
            }
            Request::SetPreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                self.text_input_handle.with_active_text_input(|ti, _surface| {
                    ti.preedit_string(Some(text.clone()), cursor_begin, cursor_end);
                });
            }
            Request::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                self.text_input_handle.with_active_text_input(|ti, _surface| {
                    ti.delete_surrounding_text(before_length, after_length);
                });
            }

            Request::Commit { serial } => {
                let discard = serial != self.handle.active_serial();
                self.text_input_handle.done(discard);
            }
            Request::PerformAction { action } => {
                let serial = self.handle.active_serial();
                let action = action.into_result().unwrap_or(Action::None);
                self.text_input_handle.with_active_text_input(|ti, _surface| {
                    if ti.version() >= 2 {
                        ti.action(action, serial);
                    }
                });
            }
            Request::MoveCursor { cursor: _, anchor: _ } => {
                tracing::debug!("move_cursor request received but zwp_text_input_v3 doesn't support it");
            }
            Request::GetInputPopupSurface {
                id,
                surface,
                positioner,
            } => {
                let mut input_method = self.handle.inner.lock().unwrap();
                let Some(instance) = input_method.active_instance_mut() else {
                    return;
                };

                // Only allow popup creation from the active instance
                if im.id() != instance.object.id() {
                    im.post_error(
                        xx_input_method_v1::Error::Inactive,
                        "Popup may only be created on the active input method.",
                    );
                    return;
                }

                if instance.active {
                    if compositor::give_role(&surface, INPUT_POPUP_SURFACE_ROLE).is_err()
                        && compositor::get_role(&surface) != Some(INPUT_POPUP_SURFACE_ROLE)
                    {
                        im.post_error(
                            xx_input_method_v1::Error::SurfaceHasRole,
                            "Surface already has a role.",
                        );
                        return;
                    }

                    let parent_surface = match self.text_input_handle.focus().clone() {
                        Some(parent) => parent,
                        None => {
                            // Race condition: focus may have been lost after client decided to create popup.
                            tracing::warn!(
                                "Ignoring popup creation: no surface in text input focus (likely race)"
                            );
                            return;
                        }
                    };

                    let location = state.parent_geometry(&parent_surface);
                    let parent = PopupParent {
                        surface: parent_surface,
                        location,
                    };

                    let positioner_data = *positioner
                        .data::<PositionerUserData>()
                        .unwrap()
                        .inner
                        .lock()
                        .unwrap();

                    let geometry =
                        state.popup_geometry(&parent.surface, &instance.cursor_rectangle, &positioner_data);

                    let popup = PopupSurface::new(
                        |data| data_init.init(id, data),
                        im.clone(),
                        parent,
                        surface,
                        instance.cursor_rectangle,
                        geometry,
                        positioner_data,
                    );
                    instance.popup_handles.push(popup.clone());
                    state.new_popup(popup);
                } else {
                    // Race condition: client may have sent this before receiving our deactivate.
                    // Silently ignore rather than killing the client with a fatal protocol error.
                    tracing::warn!(
                        "Ignoring popup creation on inactive input method (likely race with deactivate)"
                    );
                }
            }
            Request::Destroy => {
                // Nothing to do
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(&self, _state: &mut D, _client: ClientId, input_method: &XxInputMethodV1) {
        let destroyed_id = input_method.id();
        let mut inner = self.handle.inner.lock().unwrap();
        // Clear active ID if this was the active instance
        if inner.active_input_method_id.as_ref() == Some(&destroyed_id) {
            inner.active_input_method_id = None;
        }
        inner.instances.retain(|inst| inst.object.id() != destroyed_id);
        let keyboards = &self.keyboard_handle.arc.known_kbds;
        keyboards.clear_interceptor();
        self.text_input_handle.leave();
    }
}
