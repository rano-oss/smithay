use std::{
    fmt,
    sync::{Arc, Mutex},
};

use wayland_protocols::wp::{
    input_method::zv3::server::zwp_input_popup_surface_v3::PopupPositionMode,
    text_input::zv3::server::zwp_text_input_v3::Action,
};
use wayland_protocols::wp::{
    input_method::zv3::server::{
        zwp_input_method_v3::{self, ZwpInputMethodV3},
        zwp_input_popup_surface_v3::ZwpInputPopupSurfaceV3,
    },
    keyboard_filter::zv1::server::zwp_keyboard_filter_v1::ZwpKeyboardFilterV1,
};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, Resource};
use wayland_server::{
    backend::{ClientId, ObjectId},
    protocol::wl_surface::WlSurface,
};

use crate::{
    input::{SeatHandler, keyboard::KeyboardHandle},
    utils::{Logical, Rectangle},
    wayland::{
        Dispatch2, compositor, keyboard_filter::KeyboardFilterUserData, seat::WaylandFocus,
        text_input::TextInputHandle,
    },
};

use super::super::{InputMethodHandler, PopupParent, PopupSurface as ImPopupSurface};
use super::{
    INPUT_POPUP_SURFACE_ROLE, InputMethodPopupSurfaceUserData,
    input_method_popup_surface::{PopupLocation, PopupSurface},
    positioner::{PositionerState, PositionerUserData},
};

/// Result of attempting to select an input method instance by app_id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetActiveInstanceResult {
    NotFound,
    Unchanged,
    Changed,
}

/// Contains all input method instances and tracks which one is active.
#[derive(Default, Debug)]
pub(crate) struct InputMethodState {
    /// All registered input method instances.
    pub instances: Vec<InputMethod>,
    /// The object ID of the currently active input method instance.
    pub active_input_method_id: Option<ObjectId>,
    /// Last cursor rectangle forwarded from the text-input client.
    pub last_cursor_rectangle: Option<Rectangle<i32, Logical>>,
}

/// Contains input method state
pub(crate) struct InputMethod {
    pub object: ZwpInputMethodV3,
    pub serial: u32,
    pub app_id: String,
    pub popup_handles: Vec<PopupSurface>,
    /// Relative to surface on which input method is enabled
    pub text_input_rectangle: Rectangle<i32, Logical>,
}

impl fmt::Debug for InputMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputMethod")
            .field("object", &self.object)
            .field("serial", &self.serial)
            .field("app_id", &self.app_id)
            .field("popup_handles", &self.popup_handles)
            .field("text_input_rectangle", &self.text_input_rectangle)
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
pub(crate) struct InputMethodV3Handle {
    pub(crate) inner: Arc<Mutex<InputMethodState>>,
}

impl InputMethodV3Handle {
    /// Assigns a new instance with the given app_id.
    ///
    /// Replaces any prior instance registered under the same app_id so reconnects
    /// do not leave duplicate stale entries in the instance list.
    pub(super) fn add_instance(&self, instance: &ZwpInputMethodV3, app_id: String) {
        let mut inner = self.inner.lock().unwrap();
        inner.instances.retain(|i| i.app_id != app_id);
        if let Some(active_id) = inner.active_input_method_id.clone() {
            let active_still_exists = inner.instances.iter().any(|i| i.object.id() == active_id);
            if !active_still_exists {
                inner.active_input_method_id = None;
            }
        }
        let cursor = inner.last_cursor_rectangle.unwrap_or_default();
        inner.instances.push(InputMethod {
            object: instance.clone(),
            serial: 0,
            app_id,
            popup_handles: vec![],
            text_input_rectangle: cursor,
        });
    }

    /// Whether an input method instance is selected to receive protocol traffic.
    pub(crate) fn has_active_instance(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner
            .active_input_method_id
            .as_ref()
            .is_some_and(|active_id| inner.instances.iter().any(|i| i.object.id() == *active_id))
    }

    /// Whether any input method client has registered with the compositor.
    pub(crate) fn has_registered_instances(&self) -> bool {
        !self.inner.lock().unwrap().instances.is_empty()
    }

    /// Callback function to access the active input method instance.
    pub(crate) fn with_instance<R>(&self, f: impl FnOnce(&mut InputMethod) -> R) -> Option<R> {
        let mut inner = self.inner.lock().unwrap();
        let active_id = inner.active_input_method_id.clone()?;
        inner
            .instances
            .iter_mut()
            .find(|i| i.object.id() == active_id)
            .map(f)
    }

    /// App id of the currently selected input method instance, if any.
    pub fn active_app_id(&self) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        let active_id = inner.active_input_method_id.as_ref()?;
        inner
            .instances
            .iter()
            .find(|i| i.object.id() == *active_id)
            .map(|i| i.app_id.clone())
    }

    /// Set which input method instance should be active by app_id.
    pub fn set_active_instance<D: SeatHandler + 'static>(
        &self,
        state: &mut D,
        app_id: &str,
    ) -> SetActiveInstanceResult {
        let inner = self.inner.lock().unwrap();
        let target_id = inner
            .instances
            .iter()
            .find(|i| i.app_id == app_id)
            .map(|i| i.object.id());
        let Some(target_id) = target_id else {
            return SetActiveInstanceResult::NotFound;
        };
        let old_active = inner.active_input_method_id.clone();
        let had_active = inner
            .active_input_method_id
            .as_ref()
            .is_some_and(|active_id| inner.instances.iter().any(|i| i.object.id() == *active_id));
        drop(inner);

        if old_active.as_ref() == Some(&target_id) {
            return SetActiveInstanceResult::Unchanged;
        }

        if had_active {
            // deactivate_input_method locks `inner` internally — must not hold it here.
            self.deactivate_input_method(state);
        }

        let mut inner = self.inner.lock().unwrap();
        inner.active_input_method_id = Some(target_id);
        SetActiveInstanceResult::Changed
    }

    /// Re-apply the last known text-input cursor rectangle to the active IME instance.
    ///
    /// Needed after layout switches: the newly active IME instance starts with a default
    /// rectangle unless we replay the last cursor position from the focused text field.
    pub(crate) fn replay_last_cursor_rectangle<D: SeatHandler + 'static>(&self, state: &mut D) {
        let cursor = self.inner.lock().unwrap().last_cursor_rectangle;
        if let Some(cursor) = cursor {
            self.set_text_input_rectangle(state, cursor);
        }
    }

    pub(crate) fn set_text_input_rectangle<D: SeatHandler + 'static>(
        &self,
        state: &mut D,
        cursor: Rectangle<i32, Logical>,
    ) {
        let mut inner = self.inner.lock().unwrap();
        inner.last_cursor_rectangle = Some(cursor);

        let Some(active_id) = inner.active_input_method_id.clone() else {
            return;
        };
        let Some(instance) = inner.instances.iter_mut().find(|i| i.object.id() == active_id) else {
            return;
        };
        instance.text_input_rectangle = cursor;

        let data = instance.object.data::<InputMethodUserData<D>>().unwrap();
        let popup_geometry = data.popup_geometry;
        // Parent/positioner snapshots only — geometry needs the compositor without this lock.
        let mut pending: Vec<(usize, WlSurface, PositionerState, bool)> = Vec::new();
        for (index, popup) in instance.popup_handles.iter().enumerate() {
            if popup.position_mode == PopupPositionMode::StartOfPreedit && popup.awaiting_anchor {
                if popup.anchored_cursor_rectangle != Some(cursor) {
                    pending.push((
                        index,
                        popup.get_parent().surface.clone(),
                        popup.positioner(),
                        true,
                    ));
                }
            } else if popup.position_mode == PopupPositionMode::FollowCursor {
                pending.push((
                    index,
                    popup.get_parent().surface.clone(),
                    popup.positioner(),
                    false,
                ));
            }
        }
        drop(inner);

        let mut applied: Vec<(usize, PopupLocation, bool)> = Vec::new();
        for (index, parent, positioner, set_anchor) in pending {
            let geometry = popup_geometry(state, &parent, &cursor, &positioner);
            applied.push((
                index,
                PopupLocation {
                    anchor: cursor,
                    geometry,
                },
                set_anchor,
            ));
        }

        let mut inner = self.inner.lock().unwrap();
        let Some(active_id) = inner.active_input_method_id.clone() else {
            return;
        };
        let Some(instance) = inner.instances.iter_mut().find(|i| i.object.id() == active_id) else {
            return;
        };

        let mut configure_popups = false;
        for (index, new_loc, set_anchor) in applied {
            let Some(popup) = instance.popup_handles.get_mut(index) else {
                continue;
            };
            if set_anchor {
                popup.anchored_cursor_rectangle = Some(cursor);
            }
            if popup.current_location() != new_loc {
                popup.set_position(new_loc);
                configure_popups = true;
            }
        }
        drop(inner);

        if configure_popups {
            self.send_popup_configures(state);
        }
    }

    /// Send pending popup configures and notify the compositor handler.
    fn send_popup_configures<D: SeatHandler + 'static>(&self, state: &mut D) {
        let mut inner = self.inner.lock().unwrap();
        let Some(active_id) = inner.active_input_method_id.clone() else {
            return;
        };
        let Some(instance) = inner.instances.iter_mut().find(|i| i.object.id() == active_id) else {
            return;
        };
        for popup_surface in &mut instance.popup_handles {
            popup_surface.send_pending_configure();
        }
        let configure_sent = instance
            .object
            .data::<InputMethodUserData<D>>()
            .unwrap()
            .ime_popup_configure_sent;
        let popups: Vec<_> = instance
            .popup_handles
            .iter()
            .cloned()
            .map(ImPopupSurface::V3)
            .collect();
        drop(inner);

        for popup in popups {
            configure_sent(state, popup);
        }
    }

    /// Clear the active input method instance.
    pub fn clear_active_instance<D: SeatHandler + 'static>(&self, state: &mut D) {
        let _app_id = self.active_app_id();
        self.deactivate_input_method(state);
        let mut inner = self.inner.lock().unwrap();
        inner.active_input_method_id = None;
    }

    /// Send `done` to the active input method instance, incrementing its serial.
    pub(crate) fn done(&self) {
        self.with_instance(|instance| {
            for popup_surface in &mut instance.popup_handles {
                popup_surface.send_pending_configure();
            }
            instance.done();
        });
    }

    /// Activate input method on the given surface.
    ///
    /// Installs the keyboard filter immediately so key events reach the IME
    /// instead of leaking to the client as raw key presses. Preedit delivery
    /// still requires the client to enable text-input.
    pub fn activate_input_method<D: SeatHandler + 'static>(&self, _state: &mut D, surface: &WlSurface) {
        self.with_instance(|im| {
            let data = im.object.data::<InputMethodUserData<D>>().unwrap();
            im.object.activate();
            if let Some(keyboard_filter) = data.keyboard_filter.lock().unwrap().as_ref() {
                keyboard_filter
                    .data::<KeyboardFilterUserData<D>>()
                    .unwrap()
                    .ensure_interceptor(surface);
            }
        });
    }

    /// Ensure the keyboard filter interceptor is installed for `surface`.
    ///
    /// Skips reinstall when already active for the same surface (common after
    /// keyboard-enter activate). Still installs when activate ran before the
    /// filter was bound.
    pub(crate) fn ensure_keyboard_filter_interceptor<D: SeatHandler + 'static>(&self, surface: &WlSurface) {
        self.with_instance(|im| {
            let data = im.object.data::<InputMethodUserData<D>>().unwrap();
            if let Some(keyboard_filter) = data.keyboard_filter.lock().unwrap().as_ref() {
                keyboard_filter
                    .data::<KeyboardFilterUserData<D>>()
                    .unwrap()
                    .ensure_interceptor(surface);
            }
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
            let data = im.object.data::<InputMethodUserData<D>>().unwrap();
            // Clear preedit on the text-input client so the app stops showing it.
            data.text_input_handle.with_active_text_input(|ti, _surface| {
                ti.preedit_string(None, -1, -1);
            });
            // Send done so the client applies the cleared preedit.
            data.text_input_handle.done(false);

            for popup in im.popup_handles.drain(..) {
                (data.dismiss_popup)(state, popup.clone().into());
            }
            let filter = data.keyboard_filter.lock().unwrap();
            if let Some(keyboard_filter) = filter.as_ref() {
                keyboard_filter
                    .data::<KeyboardFilterUserData<D>>()
                    .unwrap()
                    .deactivate_interceptor();
            }
        });
    }
}

/// User data of ZwpInputMethodV3 object
#[derive(Clone)]
pub struct InputMethodUserData<D: SeatHandler> {
    pub(crate) handle: InputMethodV3Handle,
    pub(crate) text_input_handle: TextInputHandle,
    /// Handle to main keyboard for registering sub-keyboards
    pub(crate) keyboard_handle: KeyboardHandle<D>,
    /// Currently bound keyboard filter, set by the keyboard_filter protocol.
    pub(crate) keyboard_filter: Arc<Mutex<Option<ZwpKeyboardFilterV1>>>,
    pub(crate) dismiss_popup: fn(&mut D, ImPopupSurface),
    pub(crate) popup_geometry:
        fn(&D, &WlSurface, &Rectangle<i32, Logical>, &PositionerState) -> Rectangle<i32, Logical>,
    pub(crate) ime_popup_configure_sent: fn(&mut D, ImPopupSurface),
}

impl<D: SeatHandler> fmt::Debug for InputMethodUserData<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputMethodUserData")
            .field("handle", &self.handle)
            .field("text_input_handle", &self.text_input_handle)
            .finish()
    }
}

impl<D> Dispatch2<ZwpInputMethodV3, D> for InputMethodUserData<D>
where
    D: Dispatch<ZwpInputPopupSurfaceV3, InputMethodPopupSurfaceUserData>,
    D: SeatHandler,
    D: InputMethodHandler,
    <D as SeatHandler>::KeyboardFocus: WaylandFocus,
    D: 'static,
{
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        im: &ZwpInputMethodV3,
        request: zwp_input_method_v3::Request,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        use zwp_input_method_v3::Request;
        match request {
            Request::CommitString { text } => {
                self.text_input_handle.with_active_text_input(|ti, _surface| {
                    ti.commit_string(Some(text.clone()));
                });
                self.handle.with_instance(|instance| {
                    for popup in &mut instance.popup_handles {
                        if popup.position_mode == PopupPositionMode::StartOfPreedit {
                            // Clear the lock but do not arm awaiting — the next
                            // client cursor is often a bare post-commit caret.
                            // The IME re-arms via StartOfPreedit + preedit caret
                            // at 0 when the new segment begins.
                            popup.anchored_cursor_rectangle = None;
                            popup.awaiting_anchor = false;
                        }
                    }
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
                let mut inner = self.handle.inner.lock().unwrap();
                let last_cursor = inner.last_cursor_rectangle;
                let Some(active_id) = inner.active_input_method_id.clone() else {
                    return;
                };
                let Some(instance) = inner.instances.iter_mut().find(|i| i.object.id() == active_id) else {
                    return;
                };

                let mut seed_targets: Vec<(usize, WlSurface, PositionerState)> = Vec::new();
                for (index, popup) in instance.popup_handles.iter_mut().enumerate() {
                    if popup.position_mode != PopupPositionMode::StartOfPreedit {
                        continue;
                    }
                    if text.is_empty() {
                        popup.awaiting_anchor = true;
                        popup.anchored_cursor_rectangle = None;
                    } else if cursor_begin == 0 && cursor_end == 0 {
                        // Arm + seed from insertion point so Qt/Kate stay
                        // stable if they never report a caret-at-0 rect.
                        popup.awaiting_anchor = true;
                        if popup.anchored_cursor_rectangle.is_none() && last_cursor.is_some() {
                            seed_targets.push((
                                index,
                                popup.get_parent().surface.clone(),
                                popup.positioner(),
                            ));
                        }
                    } else {
                        popup.awaiting_anchor = false;
                    }
                }
                drop(inner);

                let mut configure_popups = false;
                if let Some(seed) = last_cursor {
                    for (index, parent, positioner) in seed_targets {
                        let geometry = state.popup_geometry(&parent, &seed, &positioner);

                        let mut inner = self.handle.inner.lock().unwrap();
                        let Some(active_id) = inner.active_input_method_id.clone() else {
                            continue;
                        };
                        let Some(instance) = inner.instances.iter_mut().find(|i| i.object.id() == active_id)
                        else {
                            continue;
                        };
                        let Some(popup) = instance.popup_handles.get_mut(index) else {
                            continue;
                        };
                        popup.anchored_cursor_rectangle = Some(seed);
                        popup.set_position(PopupLocation {
                            anchor: seed,
                            geometry,
                        });
                        configure_popups = true;
                        drop(inner);
                    }
                }
                if configure_popups {
                    self.handle.send_popup_configures(state);
                }
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
                self.handle.with_instance(|instance| {
                    self.text_input_handle.done(serial != instance.serial);
                });
            }
            Request::PerformAction { action } => {
                let serial = self.handle.with_instance(|instance| instance.serial).unwrap_or(0);
                let action = action.into_result().unwrap_or(Action::None);
                self.text_input_handle.with_active_text_input(|ti, _surface| {
                    if ti.version() >= 2 {
                        ti.action(action, serial);
                    }
                });
            }
            Request::GetInputPopupSurface {
                id,
                surface,
                positioner,
            } => {
                let inner = self.handle.inner.lock().unwrap();
                let Some(active_id) = inner.active_input_method_id.clone() else {
                    return;
                };
                let last_cursor = inner.last_cursor_rectangle;
                let fallback_cursor = inner
                    .instances
                    .iter()
                    .find(|i| i.object.id() == active_id)
                    .map(|i| i.text_input_rectangle)
                    .unwrap_or_default();
                drop(inner);

                if im.id() != active_id {
                    im.post_error(
                        zwp_input_method_v3::Error::Inactive,
                        "Popup may only be created on the active input method.",
                    );
                    return;
                }

                // Race: focus may have been lost after the client decided to create a popup.
                let Some(parent_surface) = self.text_input_handle.focus().clone() else {
                    tracing::warn!("Ignoring popup creation: no surface in text input focus (likely race)");
                    return;
                };

                if compositor::give_role(&surface, INPUT_POPUP_SURFACE_ROLE).is_err()
                    && compositor::get_role(&surface) != Some(INPUT_POPUP_SURFACE_ROLE)
                {
                    im.post_error(
                        zwp_input_method_v3::Error::SurfaceHasRole,
                        "Surface already has a role.",
                    );
                    return;
                }

                let positioner_data = *positioner
                    .data::<PositionerUserData>()
                    .unwrap()
                    .inner
                    .lock()
                    .unwrap();

                let location = state.parent_geometry(&parent_surface);
                let cursor = last_cursor.unwrap_or(fallback_cursor);
                let geometry = state.popup_geometry(&parent_surface, &cursor, &positioner_data);
                let parent = PopupParent {
                    surface: parent_surface,
                    location,
                };

                let mut inner = self.handle.inner.lock().unwrap();
                let Some(instance) = inner.instances.iter_mut().find(|i| i.object.id() == active_id) else {
                    return;
                };
                instance.text_input_rectangle = cursor;
                let popup = PopupSurface::new(
                    |data| data_init.init(id, data),
                    im.clone(),
                    parent,
                    surface,
                    cursor,
                    geometry,
                    positioner_data,
                );
                instance.popup_handles.push(popup.clone());
                drop(inner);

                state.new_popup(popup.into());
            }
            Request::Destroy => {
                // Nothing to do
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(&self, _state: &mut D, _client: ClientId, input_method: &ZwpInputMethodV3) {
        let destroyed_id = input_method.id();
        let mut inner = self.handle.inner.lock().unwrap();
        let _app_id = inner
            .instances
            .iter()
            .find(|inst| inst.object.id() == destroyed_id)
            .map(|inst| inst.app_id.clone());
        let was_active = inner.active_input_method_id.as_ref() == Some(&destroyed_id);
        if was_active {
            inner.active_input_method_id = None;
        }
        inner.instances.retain(|inst| inst.object.id() != destroyed_id);
        let _remaining = inner.instances.len();
        drop(inner);

        if was_active {
            if let Some(keyboard_filter) = self.keyboard_filter.lock().unwrap().as_ref() {
                keyboard_filter
                    .data::<KeyboardFilterUserData<D>>()
                    .unwrap()
                    .flush_pending_passthrough();
            }
            self.keyboard_handle.arc.clear_kbd_interceptor();
            // Clear stale preedit but do not send text-input leave: leave() drops
            // active_text_input_id so a reconnecting IME cannot deliver preedit until
            // the client re-enables, which chewingwl does not always do promptly.
            self.text_input_handle.with_active_text_input(|ti, _surface| {
                ti.preedit_string(None, -1, -1);
            });
            self.text_input_handle.done(false);
        }
    }
}
