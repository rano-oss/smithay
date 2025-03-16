use super::{InputMethodManagerState, BUFFERSIZE};
use crate::{
    input::{
        keyboard::{KeyboardHandle, ModifiersState},
        SeatHandler,
    },
    wayland::{
        compositor, seat::WaylandFocus, shell::xdg::XdgPopupSurfaceData, text_input::v3_2::TextInputHandle,
    },
};
use std::{
    collections::VecDeque,
    fmt::{self, Debug},
    sync::{Arc, Mutex},
};
use wayland_protocols::{
    wp::input_method::v3::server::wp_input_method_v3::{self, WpInputMethodV3},
    xdg::shell::server::xdg_popup::XdgPopup,
};
use wayland_server::{
    backend::ClientId,
    protocol::{wl_keyboard::KeyState, wl_surface::WlSurface},
};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, Resource};

#[derive(Default, Debug)]
pub(crate) struct InputMethod {
    pub instances: Vec<Instance>,
    pub current: Option<String>,
    pub keys: VecDeque<(u32, KeyState, crate::input::keyboard::ModifiersState, u32, u32)>,
}

#[derive(Debug)]
pub(crate) struct Instance {
    pub object: WpInputMethodV3,
    pub serial: u32,
    pub app_id: String,
    pub popup: Option<XdgPopup>,
}

impl Instance {
    /// Send the done incrementing the serial.
    pub(crate) fn done(&mut self) {
        self.object.done();
        self.serial += 1;
    }
}

/// Handle to an input method instance
#[derive(Default, Debug, Clone)]
pub struct InputMethodHandle {
    pub(crate) inner: Arc<Mutex<InputMethod>>,
}

impl InputMethodHandle {
    pub(super) fn add_instance(&self, instance: &WpInputMethodV3, app_id: String) {
        let mut inner = self.inner.lock().unwrap();
        inner.keys = VecDeque::with_capacity(BUFFERSIZE);
        inner.instances.push(Instance {
            object: instance.clone(),
            serial: 0,
            app_id: app_id.clone(),
            popup: None,
        });
        if inner.current.is_none() {
            inner.current = Some(app_id);
        }
    }

    /// Whether there is any instance of input-method.
    pub(crate) fn has_instance(&self) -> bool {
        !self.inner.lock().unwrap().instances.is_empty()
    }

    /// Callback function to access the input method object
    pub(crate) fn with_instance<F>(&self, app_id: String, mut f: F)
    where
        F: FnMut(&mut Instance),
    {
        let mut inner = self.inner.lock().unwrap();
        if let Some(instance) = inner.instances.iter_mut().find(|inst| inst.app_id == app_id) {
            f(instance);
        }
    }

    /// Activate input method on the given surface.
    pub(crate) fn activate_input_method(&self, surface: &WlSurface, app_id: String) {
        let mut inner = self.inner.lock().unwrap();
        inner.current = Some(app_id.clone());
        println!("{:?}", inner.instances);
        let instance = inner.instances.iter().find(|instance| instance.app_id == app_id);
        if let Some(instance) = instance {
            println!("We sent activation");
            instance.object.activate(app_id.clone());
        }
        //     if let Some(popup) = instance.popup.as_mut() {
        //         // Remove old popup.
        //         popup.popup_done();

        //         let data = popup
        //             .data::<crate::wayland::shell::xdg::XdgShellSurfaceUserData>()
        //             .unwrap();

        //         compositor::with_states(&data.wl_surface, move |states| {
        //             states
        //                 .data_map
        //                 .get::<XdgPopupSurfaceData>()
        //                 .unwrap()
        //                 .lock()
        //                 .unwrap()
        //                 .parent = Some(surface.clone());
        //         });
        //     }
    }

    /// Deactivate the active input method.
    ///
    /// The `done` is required in cases where the change in state is initiated not by text-input.
    pub(crate) fn deactivate_input_method(&self, done: bool, app_id: String) {
        let mut inner = self.inner.lock().unwrap();
        let instance = inner.instances.iter().find(|instance| instance.app_id == app_id);
        if let Some(instance) = instance {
            instance.object.deactivate();
            if done {
                instance.object.done();
            }
            if let Some(popup) = &instance.popup {
                popup.popup_done();

                let data = popup
                    .data::<crate::wayland::shell::xdg::XdgShellSurfaceUserData>()
                    .unwrap();

                compositor::with_states(&data.wl_surface, move |states| {
                    states
                        .data_map
                        .get::<XdgPopupSurfaceData>()
                        .unwrap()
                        .lock()
                        .unwrap()
                        .parent = None;
                });
            }
        }
        inner.current = None;
    }

    /// Gets the currently set input method
    pub fn currently_set_input_method(&self) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        inner.current.clone()
    }

    /// input method keyboard intercept
    pub fn input_intercept(
        &self,
        keycode: u32,
        state: KeyState,
        serial: u32,
        time: u32,
        modifiers: &ModifiersState,
    ) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if let Some(current) = inner.current.clone() {
            if let Some(instance) = inner.instances.iter().find(|inst| inst.app_id == current) {
                instance.object.key(serial, time, keycode, state);
                let serialized = modifiers.serialized;
                instance.object.modifiers(
                    serial,
                    serialized.depressed,
                    serialized.latched,
                    serialized.locked,
                    serialized.layout_effective,
                );
                if inner.keys.len() == BUFFERSIZE {
                    inner.keys.pop_front();
                }
                inner.keys.push_back((keycode, state, *modifiers, serial, time));
                true
            } else {
                false
            }
        } else {
            false
        }
    }
}

/// User data of WpInputMethodV3 object
pub struct InputMethodUserData<D: SeatHandler> {
    pub(super) handle: InputMethodHandle,
    pub(crate) text_input_handle: TextInputHandle,
    pub(crate) keyboard_handle: KeyboardHandle<D>,
}

impl<D: SeatHandler> fmt::Debug for InputMethodUserData<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputMethodUserData")
            .field("handle", &self.handle)
            .field("text_input_handle", &self.text_input_handle)
            .field("keyboard_handle", &self.keyboard_handle)
            .finish()
    }
}

impl<D> Dispatch<WpInputMethodV3, InputMethodUserData<D>, D> for InputMethodManagerState
where
    D: Dispatch<WpInputMethodV3, InputMethodUserData<D>>,
    D: SeatHandler,
    <D as SeatHandler>::KeyboardFocus: WaylandFocus,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        input_method: &WpInputMethodV3,
        request: wp_input_method_v3::Request,
        data: &InputMethodUserData<D>,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        // data.text_input_handle.with_focused_instance(|instance, _surface| {
        //     if data.handle.currently_set_input_method()
        // });
        match request {
            wp_input_method_v3::Request::SetString { text } => {
                data.text_input_handle
                    .with_focused_instance(|instance, _surface| {
                        instance.object.commit_string(Some(text.clone()));
                    });
            }
            wp_input_method_v3::Request::SetPreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                data.text_input_handle
                    .with_focused_instance(|instance, _surface| {
                        instance
                            .object
                            .preedit_string(Some(text.clone()), cursor_begin, cursor_end);
                    });
            }
            wp_input_method_v3::Request::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                data.text_input_handle
                    .with_focused_instance(|instance, _surface| {
                        instance
                            .object
                            .delete_surrounding_text(before_length, after_length);
                    });
            }
            wp_input_method_v3::Request::Commit { serial } => {
                let current_serial = data
                    .handle
                    .inner
                    .lock()
                    .unwrap()
                    .instances
                    .iter()
                    .find(|im| im.object.id() == input_method.id())
                    .as_ref()
                    .map(|i| i.serial)
                    .unwrap_or(0);

                data.text_input_handle.done(serial != current_serial);
            }
            wp_input_method_v3::Request::GetInputMethodPopup { popup } => {
                let Some(parent_surface) = data.text_input_handle.focus() else {
                    return;
                };
                let mut input_method_handle = data.handle.inner.lock().unwrap();
                let Some(im) = input_method_handle
                    .instances
                    .iter_mut()
                    .find(|inst| inst.object.id() == input_method.id())
                else {
                    return;
                };
                im.popup = Some(popup.clone());
                let data = popup
                    .data::<crate::wayland::shell::xdg::XdgShellSurfaceUserData>()
                    .unwrap();

                compositor::with_states(&data.wl_surface, move |states| {
                    states
                        .data_map
                        .get::<XdgPopupSurfaceData>()
                        .unwrap()
                        .lock()
                        .unwrap()
                        .parent = Some(parent_surface);
                });
            }
            wp_input_method_v3::Request::SetAction { action } => data
                .text_input_handle
                .with_focused_instance(|instance, _surface| {
                    instance
                        .object
                        .action(action.into_result().unwrap(), instance.serial);
                }),
            wp_input_method_v3::Request::SetLanguage { language } => data
                .text_input_handle
                .with_focused_instance(|instance, _surface| instance.object.language(language.clone())),
            wp_input_method_v3::Request::SetPreeditCommitMode { mode } => data
                .text_input_handle
                .with_focused_instance(|instance, _surface| {
                    instance.object.preedit_commit_mode(mode.into_result().unwrap())
                }),
            wp_input_method_v3::Request::SetPreeditStyle {
                begin,
                end,
                underline,
                style,
                color,
            } => data
                .text_input_handle
                .with_focused_instance(|instance, _surface| {
                    instance.object.preedit_style(
                        begin,
                        end,
                        underline.into_result().unwrap(),
                        style.into_result().unwrap(),
                        color.into_result().unwrap(),
                    )
                }),
            wp_input_method_v3::Request::Destroy => {} // Nothing to do
            _ => unreachable!(),
        }
    }

    fn destroyed(
        _state: &mut D,
        _client: ClientId,
        input_method: &WpInputMethodV3,
        data: &InputMethodUserData<D>,
    ) {
        let mut inner = data.handle.inner.lock().unwrap();
        let instance = inner
            .instances
            .iter()
            .find(|inst| inst.object.id() == input_method.id());
        if let Some(instance) = instance.as_deref() {
            let im_app_id = instance.app_id.clone();
            data.text_input_handle.input_method_destroyed(im_app_id.clone());
            inner.current.take_if(|app_id| *app_id == im_app_id);
        }
        inner
            .instances
            .retain(|inst| inst.object.id() != input_method.id());
    }
}
