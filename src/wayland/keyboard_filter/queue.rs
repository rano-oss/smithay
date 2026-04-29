use std::{cell::RefCell, collections::VecDeque, rc::Rc, sync::{Arc, Mutex}};

use wayland_server::protocol::{wl_keyboard, wl_surface::WlSurface};
use xkbcommon::xkb::Keycode;

use crate::{backend::input::KeyEvent, input::{keyboard::{KeyboardHandle, KeyboardInnerHandle, KeyboardTargetWithData, ModifiersState}, SeatHandler}, utils::Serial};

#[derive(Clone, Copy)]
pub enum Action {
    Passthrough,
    Consume,
}

#[derive(Debug)]
pub struct KeyboardEvent {
    pub keycode: Keycode,
    pub key_event: KeyEvent,
    pub modifiers: Option<ModifiersState>,
    pub serial: Serial,
    pub time: u32,
}

#[derive(Debug)]
pub(crate) enum EventState {
    /// Delayed until confirmation arrives
    Delaying,
}

/// Filters key events to arrive to text input.
///
/// Cloning creates another reference
#[derive(Clone, Debug)]
pub(crate) struct DispatchQueue {
    //filter: super::Filter,
    /// Client keyboards to which events can be forwarded
    //client_keyboards: std::sync::Weak<Mutex<Vec<wayland_server::Weak<wl_keyboard::WlKeyboard>>>>,
    /// Events waiting for filter decision from the input method client.
    /// This queue begins with 0 to N of sent events and contains delayed ones after that.
    events_to_filter: Arc<Mutex<VecDeque<(EventState, KeyboardEvent)>>>,
    // Surface to which events should be sent after filtering
    //focused_surface: WlSurface,
}

impl DispatchQueue {
    /// Creates a new instance of key filter and initializes it.
    pub(crate) fn new(
        //filter: &super::Filter,
        /*client_keyboards: &Arc<Mutex<
            Vec<wayland_server::Weak<wl_keyboard::WlKeyboard>>
        >>,
        surface: &WlSurface,*/
    ) -> Self {
        Self {
            //filter: filter.clone(),
            //client_keyboards: Arc::downgrade(client_keyboards),
            events_to_filter: Arc::new(Mutex::new(VecDeque::new())),
            //focused_surface: surface.clone(),
        }
    }
    
    pub(crate) fn push_event(&self, event: KeyboardEvent) {
        // TODO: unnecessary (?) Sync requirement causes the need to lock
        let mut events = self.events_to_filter.lock().unwrap();
        events.push_front((EventState::Delaying, event));
    }

    /// Applies event to the text input application, if event was not already applied.
    pub(crate) fn apply<D: SeatHandler + 'static>(
        &self,
        (_, event): (EventState, KeyboardEvent),
        action: Action,
        keyboard: &KeyboardHandle<D>,
        data: &mut D,
    ) {
        let seat = &keyboard.get_seat(data);
        let focus = keyboard.current_focus();
        let focus = focus.as_ref()
            .map(|f| KeyboardTargetWithData {
                target: f,
                data: Rc::new(RefCell::new(data)),
            });
        let inner = keyboard.arc.internal.lock().unwrap();
        KeyboardInnerHandle::<D>::input_generic(
            inner.xkb_related_state(),
            focus.as_ref(),
            seat,
            event.keycode,
            event.key_event,
            event.modifiers,
            event.serial,
            event.time,
            match action {
                Action::Passthrough => true,
                Action::Consume => false,
            },
        );
    }
    
    pub(crate) fn events(&self) -> &Arc<Mutex<VecDeque<(EventState, KeyboardEvent)>>> {
        &self.events_to_filter
    }
}