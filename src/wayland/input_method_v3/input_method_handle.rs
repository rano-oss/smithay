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
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, Resource};

use crate::{
    input::{keyboard::KeyboardHandle, Seat, SeatHandler},
    utils::{Logical, Rectangle},
    wayland::{compositor, keyboard_filter, seat::WaylandFocus, text_input::TextInputHandle},
};

use super::{
    input_method_popup_surface::{ImPopupLocation, PopupParent, PopupSurface},
    positioner::PositionerUserData,
    InputMethodHandler, InputMethodManagerState, InputMethodPopupSurfaceUserData, INPUT_POPUP_SURFACE_ROLE,
};

/// Contains all input method instances and tracks which one is active.
#[derive(Default, Debug)]
pub(crate) struct InputMethodState {
    /// All registered input method instances.
    pub instances: Vec<InputMethod>,
    /// The object ID of the currently active input method instance.
    pub active_input_method_id: Option<ObjectId>,
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
        let active_id = match &inner.active_input_method_id {
            Some(id) => id.clone(),
            None => return,
        };
        if let Some(instance) = inner.instances.iter_mut().find(|i| i.object.id() == active_id) {
            f(instance);
        }
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
        let active_id = match &inner.active_input_method_id {
            Some(id) => id.clone(),
            None => return,
        };
        if let Some(instance) = inner.instances.iter_mut().find(|i| i.object.id() == active_id) {
            instance.cursor_rectangle = cursor;
            for popup_surface in &mut instance.popup_handles {
                let popup_geometry = state.popup_geometry(
                    &popup_surface.get_parent().surface,
                    &cursor,
                    &popup_surface.positioner(),
                );

                let anchor = cursor; // FIXME: choose the anchor which the positioner wants

                popup_surface.set_position(ImPopupLocation {
                    anchor,
                    geometry: popup_geometry,
                });

                state.popup_repositioned(popup_surface.clone());
            }
        }
    }

    /// Send `done` to the active input method instance, incrementing its serial.
    pub fn done(&self) {
        let mut inner = self.inner.lock().unwrap();
        let active_id = match &inner.active_input_method_id {
            Some(id) => id.clone(),
            None => return,
        };

        if let Some(instance) = inner.instances.iter_mut().find(|i| i.object.id() == active_id) {
            for popup_surface in &mut instance.popup_handles {
                popup_surface.send_pending_configure();
            }
            instance.done();
        }
    }

    /// Activate input method on the given surface.
    pub fn activate_input_method<D: SeatHandler + 'static>(&self, _state: &mut D, surface: &WlSurface) {
        self.with_instance(|im| {
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
    pub fn deactivate_input_method<D: SeatHandler + 'static>(&self, state: &mut D) {
        self.with_instance(|im| {
            im.object.deactivate();
            im.done();
            im.active = false;
            let data = im.object.data::<InputMethodUserData<D>>().unwrap();
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

impl<D> Dispatch<XxInputMethodV1, InputMethodUserData<D>, D> for InputMethodManagerState
where
    D: Dispatch<XxInputMethodV1, InputMethodUserData<D>>,
    D: Dispatch<XxInputPopupSurfaceV2, InputMethodPopupSurfaceUserData>,
    D: SeatHandler,
    D: InputMethodHandler,
    <D as SeatHandler>::KeyboardFocus: WaylandFocus,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        im: &XxInputMethodV1,
        request: xx_input_method_v1::Request,
        data: &InputMethodUserData<D>,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        use xx_input_method_v1::Request;
        match request {
            Request::CommitString { text } => {
                data.text_input_handle.with_active_text_input(|ti, _surface| {
                    ti.commit_string(Some(text.clone()));
                });
            }
            Request::SetPreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                data.text_input_handle.with_active_text_input(|ti, _surface| {
                    ti.preedit_string(Some(text.clone()), cursor_begin, cursor_end);
                });
            }
            Request::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                data.text_input_handle.with_active_text_input(|ti, _surface| {
                    ti.delete_surrounding_text(before_length, after_length);
                });
            }

            Request::Commit { serial } => {
                let current_serial = {
                    let inner = data.handle.inner.lock().unwrap();
                    let active_id = inner.active_input_method_id.clone();
                    active_id
                        .and_then(|id| inner.instances.iter().find(|i| i.object.id() == id))
                        .map(|i| i.serial)
                        .unwrap_or(0)
                };

                data.text_input_handle.done(serial != current_serial);
            }
            Request::PerformAction { action } => {
                let serial = {
                    let inner = data.handle.inner.lock().unwrap();
                    let active_id = inner.active_input_method_id.clone();
                    active_id
                        .and_then(|id| inner.instances.iter().find(|i| i.object.id() == id))
                        .map(|i| i.serial)
                        .unwrap_or(0)
                };
                let action = action.into_result().unwrap_or(Action::None);
                data.text_input_handle.with_active_text_input(|ti, _surface| {
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
                let mut input_method = data.handle.inner.lock().unwrap();
                let active_id = match &input_method.active_input_method_id {
                    Some(id) => id.clone(),
                    None => return,
                };
                let instance = match input_method
                    .instances
                    .iter_mut()
                    .find(|i| i.object.id() == active_id)
                {
                    Some(inst) => inst,
                    None => return,
                };

                // Only allow popup creation from the active instance
                if im.id() != active_id {
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

                    let parent_surface = match data.text_input_handle.focus().clone() {
                        Some(parent) => parent,
                        None => {
                            im.post_error(
                                xx_input_method_v1::Error::Inactive,
                                "Popup may only be created on an active input method (no surface in text input focus).",
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
                    im.post_error(
                        xx_input_method_v1::Error::Inactive,
                        "Popup may only be created on an active input method.",
                    );
                }
            }
            Request::Destroy => {
                // Nothing to do
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        _state: &mut D,
        _client: ClientId,
        input_method: &XxInputMethodV1,
        data: &InputMethodUserData<D>,
    ) {
        let destroyed_id = input_method.id();
        let mut inner = data.handle.inner.lock().unwrap();

        // Clear active ID if this was the active instance
        if inner.active_input_method_id.as_ref() == Some(&destroyed_id) {
            inner.active_input_method_id = None;
        }

        inner.instances.retain(|inst| inst.object.id() != destroyed_id);
        drop(inner);

        let keyboards = &data.keyboard_handle.arc.known_kbds;
        keyboards.clear_interceptor();
        data.text_input_handle.leave();
    }
}
