use std::{collections::VecDeque, sync::{Arc, Mutex}};

use wayland_server::protocol::{wl_keyboard, wl_surface::WlSurface};

use crate::input::keyboard::KnownKbds;

pub struct KeyboardEvent;

impl KeyboardEvent {
    pub(crate) fn is_filterable(&self) -> bool {
        panic!()
    }
    fn apply<T>(&self, t:T) {
        panic!()
    }
    pub(crate) fn serial(&self) -> Option<u32> {
        panic!();
    }
}


#[derive(Debug)]
pub(crate) enum EventState {
    /// Event is already sent but confirmation has not arrived yet.
    /// Only for events which must be passed through.
    Sent,
    /// Delayed until confirmation arrives
    Delaying,
}

/// Filters key events to arrive to text input
pub(crate) struct DispatchQueue {
    pub filter: super::Filter,
    /// Client keyboards to which events can be forwarded
    pub client_keyboards: std::sync::Weak<Mutex<Vec<wayland_server::Weak<wl_keyboard::WlKeyboard>>>>,
    /// Events waiting for filter decision from the input method client.
    /// This queue begins with 0 to N of sent events and contains delayed ones after that.
    pub events_to_filter: Arc<Mutex<VecDeque<(EventState, KeyboardEvent)>>>,
    /// Surface to which events should be sent after filtering
    pub focused_surface: WlSurface,
}

impl DispatchQueue {
    /// Creates a new instance of key filter and initializes it.
    pub(crate) fn create(
        filter: &super::Filter,
        client_keyboards: &Arc<Mutex<
            Vec<wayland_server::Weak<wl_keyboard::WlKeyboard>>
        >>,
        surface: &WlSurface,
    ) -> Self {
        Self {
            filter: filter.clone(),
            client_keyboards: Arc::downgrade(client_keyboards),
            events_to_filter: Arc::new(Mutex::new(VecDeque::new())),
            focused_surface: surface.clone(),
        }
    }
    
    fn push_event(&self, event: KeyboardEvent) {
        // TODO: unnecessary (?) Sync requirement causes the need to lock
        let mut events = self.events_to_filter.lock().unwrap();
        let queue_empty = events.iter().position(|e| match e {
            (EventState::Delaying, _) => true,
            _ => false,
        }).is_some();

        let state = if queue_empty && !event.is_filterable() {
            // There are no outstanding delayed events to block incoming events, so if this one doesn't need to wait for a filter decision, send it immediately.
            self.forward(&event);
            EventState::Sent
        } else {
            EventState::Delaying
        };
        events.push_front((state, event));
    }

    /// Applies event to the text input application, if event was not already applied.
    pub(crate) fn apply(&self, (state, event): (EventState, KeyboardEvent)) {
        match state {
            EventState::Sent => {},
            EventState::Delaying => self.forward(&event),
        }
    }
    
    fn forward(&self, event: &KeyboardEvent) {
        let keyboards = self.client_keyboards.upgrade().unwrap();
        KnownKbds::for_each_focused_kbd(
            &*keyboards.lock().unwrap(),
            &self.focused_surface,
            |k| event.apply(k)
        );
    }
}