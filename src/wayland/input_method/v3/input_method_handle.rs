use std::{
    fmt,
    sync::{Arc, Mutex},
};

use tracing::{debug, warn};

use wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3::Action;
use wayland_protocols_experimental::{
    input_method::v1::server::{
        xx_input_method_v1::{self, XxInputMethodV1},
        xx_input_popup_surface_v2::{PopupPositionMode, XxInputPopupSurfaceV2},
    },
    keyboard_filter::v3::server::xx_keyboard_filter_v1::XxKeyboardFilterV1,
    text_input::v3::server::xx_text_input_v3 as xx_ti,
};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, Resource};
use wayland_server::{
    backend::{ClientId, ObjectId},
    protocol::wl_surface::WlSurface,
};

use crate::{
    input::{SeatHandler, keyboard::KeyboardHandle},
    utils::{Logical, Rectangle, Serial},
    wayland::{
        Dispatch2, compositor, keyboard_filter::KeyboardFilterUserData, seat::WaylandFocus,
        text_input::TextInputHandle,
    },
};

use super::super::{PopupParent, PopupSurface as ImPopupSurface};
use super::{
    INPUT_POPUP_SURFACE_ROLE, InputMethodPopupSurfaceUserData, PopupSurfaceState,
    input_method_popup_surface::{PopupLocation, PopupSurface},
    positioner::{PositionerState, PositionerUserData},
};

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
#[derive(Debug)]
pub(crate) struct InputMethod {
    pub object: XxInputMethodV1,
    pub serial: u32,
    pub app_id: String,
    pub popup_handles: Vec<PopupSurface>,
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
    pub(super) fn add_instance(&self, instance: &XxInputMethodV1, app_id: String) {
        let mut inner = self.inner.lock().unwrap();
        inner.instances.retain(|i| i.app_id != app_id);
        if inner
            .active_input_method_id
            .as_ref()
            .is_some_and(|id| !inner.instances.iter().any(|i| i.object.id() == *id))
        {
            inner.active_input_method_id = None;
        }
        inner.instances.push(InputMethod {
            object: instance.clone(),
            serial: 0,
            app_id,
            popup_handles: vec![],
        });
    }

    pub(crate) fn has_active_instance(&self) -> bool {
        self.with_instance(|_| ()).is_some()
    }

    pub(crate) fn with_instance<R>(&self, f: impl FnOnce(&mut InputMethod) -> R) -> Option<R> {
        let mut inner = self.inner.lock().unwrap();
        let active_id = inner.active_input_method_id.clone()?;
        inner
            .instances
            .iter_mut()
            .find(|i| i.object.id() == active_id)
            .map(f)
    }

    pub(crate) fn active_app_id(&self) -> Option<String> {
        self.with_instance(|i| i.app_id.clone())
    }

    /// Select instance by `app_id`. Returns `false` if no matching instance exists.
    pub(crate) fn set_active_instance<D: SeatHandler + 'static>(&self, state: &mut D, app_id: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        let Some(target_id) = inner
            .instances
            .iter()
            .find(|i| i.app_id == app_id)
            .map(|i| i.object.id())
        else {
            return false;
        };
        let old_active = inner.active_input_method_id.clone();
        let last_cursor = inner.last_cursor_rectangle;
        drop(inner);

        if old_active.as_ref() == Some(&target_id) {
            return true;
        }
        if old_active.is_some() {
            self.deactivate_input_method(state);
        }
        self.inner.lock().unwrap().active_input_method_id = Some(target_id);
        if let Some(cursor) = last_cursor {
            self.set_text_input_rectangle(state, cursor);
        }
        true
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
        let data = instance.object.data::<InputMethodUserData<D>>().unwrap();
        let popup_geometry = data.popup_geometry;
        let ime_popup_configure_sent = data.ime_popup_configure_sent;

        let mut pending = Vec::new();
        for (index, popup) in instance.popup_handles.iter().enumerate() {
            let awaiting = popup.position_mode == PopupPositionMode::StartOfPreedit && popup.awaiting_anchor;
            if popup.position_mode == PopupPositionMode::FollowCursor
                || (awaiting && popup.anchored_cursor_rectangle != Some(cursor))
            {
                pending.push((
                    index,
                    popup.get_parent().surface.clone(),
                    popup.positioner(),
                    awaiting,
                ));
            }
        }
        drop(inner);

        let mut changed = false;
        for (index, parent, positioner, set_anchor) in pending {
            let loc = PopupLocation {
                anchor: cursor,
                geometry: popup_geometry(state, &parent, &cursor, &positioner),
            };
            let mut inner = self.inner.lock().unwrap();
            let Some(popup) = inner
                .instances
                .iter_mut()
                .find(|i| i.object.id() == active_id)
                .and_then(|i| i.popup_handles.get_mut(index))
            else {
                continue;
            };
            if set_anchor {
                // Lock on the first cursor while awaiting so later client rects
                // (e.g. GTK end-of-preedit) cannot overwrite the start anchor.
                popup.anchored_cursor_rectangle = Some(cursor);
                popup.awaiting_anchor = false;
            }
            if popup.current_location() != loc {
                popup.set_position(loc);
                changed = true;
            }
        }

        if !changed {
            return;
        }
        let popups = self
            .with_instance(|im| {
                for p in &mut im.popup_handles {
                    p.send_pending_configure();
                }
                im.popup_handles
                    .iter()
                    .cloned()
                    .map(ImPopupSurface::V3)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for popup in popups {
            ime_popup_configure_sent(state, popup);
        }
    }

    pub(crate) fn clear_active_instance<D: SeatHandler + 'static>(&self, state: &mut D) {
        self.deactivate_input_method(state);
        self.inner.lock().unwrap().active_input_method_id = None;
    }

    pub(crate) fn done(&self) {
        self.with_instance(|instance| {
            for popup in &mut instance.popup_handles {
                popup.send_pending_configure();
            }
            instance.done();
        });
    }

    pub(crate) fn activate_input_method<D: SeatHandler + 'static>(
        &self,
        _state: &mut D,
        surface: &WlSurface,
    ) {
        self.with_instance(|im| {
            im.object.activate();
            im.object
                .data::<InputMethodUserData<D>>()
                .unwrap()
                .with_filter(|f| f.activate_interceptor(surface));
        });
    }

    pub(crate) fn deactivate_input_method<D: SeatHandler + 'static>(&self, state: &mut D) {
        self.with_instance(|im| {
            im.object.deactivate();
            im.done();
            let data = im.object.data::<InputMethodUserData<D>>().unwrap();
            data.text_input_handle.with_active_text_input(|ti, _| {
                ti.preedit_string(None, -1, -1);
            });
            data.text_input_handle.done(false);
            for popup in im.popup_handles.drain(..) {
                (data.dismiss_popup)(state, popup.into());
            }
            data.with_filter(|f| f.deactivate_interceptor());
        });
    }
}

/// User data of XxInputMethodV1 object
pub struct InputMethodUserData<D: SeatHandler> {
    pub(crate) handle: InputMethodV3Handle,
    pub(crate) text_input_handle: TextInputHandle,
    pub(crate) keyboard_handle: KeyboardHandle<D>,
    pub(crate) keyboard_filter: Arc<Mutex<Option<XxKeyboardFilterV1>>>,
    pub(crate) dismiss_popup: fn(&mut D, ImPopupSurface),
    pub(crate) popup_geometry:
        fn(&D, &WlSurface, &Rectangle<i32, Logical>, &PositionerState) -> Rectangle<i32, Logical>,
    pub(crate) ime_popup_configure_sent: fn(&mut D, ImPopupSurface),
    pub(crate) parent_geometry: fn(&D, &WlSurface) -> Rectangle<i32, Logical>,
    pub(crate) popup_repositioned: fn(&mut D, ImPopupSurface),
    pub(crate) new_popup: fn(&mut D, ImPopupSurface),
    pub(crate) popup_ack_configure: fn(&mut D, &WlSurface, Serial, PopupSurfaceState),
}

impl<D: SeatHandler + 'static> InputMethodUserData<D> {
    fn with_filter<R>(&self, f: impl FnOnce(&KeyboardFilterUserData<D>) -> R) -> Option<R> {
        self.keyboard_filter
            .lock()
            .unwrap()
            .as_ref()
            .map(|kf| f(kf.data::<KeyboardFilterUserData<D>>().unwrap()))
    }
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
    D: Dispatch<XxInputPopupSurfaceV2, InputMethodPopupSurfaceUserData>,
    D: SeatHandler,
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
                self.handle.with_instance(|instance| {
                    for popup in &mut instance.popup_handles {
                        if popup.position_mode == PopupPositionMode::StartOfPreedit {
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
                let mut seed_cursor = None;
                for popup in instance.popup_handles.iter_mut() {
                    if popup.position_mode != PopupPositionMode::StartOfPreedit {
                        continue;
                    }
                    if text.is_empty() {
                        popup.awaiting_anchor = true;
                        popup.anchored_cursor_rectangle = None;
                    } else if cursor_begin == 0 && cursor_end == 0 {
                        popup.awaiting_anchor = true;
                        if popup.anchored_cursor_rectangle.is_none() {
                            seed_cursor = last_cursor;
                        }
                    } else {
                        popup.awaiting_anchor = false;
                    }
                }
                drop(inner);
                if let Some(cursor) = seed_cursor {
                    self.handle.set_text_input_rectangle(state, cursor);
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
                let action = match action.into_result().unwrap_or(xx_ti::Action::Finish) {
                    xx_ti::Action::Finish => Action::Submit,
                    _ => Action::None,
                };
                self.text_input_handle.with_active_text_input(|ti, _surface| {
                    if ti.version() >= 2 {
                        ti.action(action, serial);
                    }
                });
            }
            Request::MoveCursor { cursor: _, anchor: _ } => {
                debug!("move_cursor ignored: no zwp_text_input_v3 equivalent");
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
                let cursor = inner.last_cursor_rectangle.unwrap_or_default();
                drop(inner);

                if im.id() != active_id {
                    im.post_error(
                        xx_input_method_v1::Error::Inactive,
                        "Popup may only be created on the active input method.",
                    );
                    return;
                }

                let Some(parent_surface) = self.text_input_handle.focus().clone() else {
                    warn!("ignoring popup creation without text-input focus");
                    return;
                };

                if compositor::give_role(&surface, INPUT_POPUP_SURFACE_ROLE).is_err()
                    && compositor::get_role(&surface) != Some(INPUT_POPUP_SURFACE_ROLE)
                {
                    im.post_error(
                        xx_input_method_v1::Error::SurfaceHasRole,
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

                let location = (self.parent_geometry)(state, &parent_surface);
                let geometry = (self.popup_geometry)(state, &parent_surface, &cursor, &positioner_data);
                let parent = PopupParent {
                    surface: parent_surface,
                    location,
                };

                let mut inner = self.handle.inner.lock().unwrap();
                let Some(instance) = inner.instances.iter_mut().find(|i| i.object.id() == active_id) else {
                    return;
                };
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

                (self.new_popup)(state, popup.into());
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
        let was_active = inner.active_input_method_id.as_ref() == Some(&destroyed_id);
        if was_active {
            inner.active_input_method_id = None;
        }
        inner.instances.retain(|inst| inst.object.id() != destroyed_id);
        drop(inner);

        if was_active {
            self.with_filter(|f| {
                f.flush_pending_passthrough();
                f.deactivate_interceptor();
            });
            // Prefer clearing preedit over text_input.leave(), which drops the
            // active text-input id and blocks further preedit until re-enabled.
            self.text_input_handle.with_active_text_input(|ti, _surface| {
                ti.preedit_string(None, -1, -1);
            });
            self.text_input_handle.done(false);
        }
    }
}
