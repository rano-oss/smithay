use crate::utils::{Serial, SERIAL_COUNTER};

use super::input_method_popup_surface::PopupSurfaceState;

/// A configure event sent to the client, waiting to be acknowledged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupConfigure {
    pub state: PopupSurfaceState,
    pub serial: Serial,
}

/// Tracks popup configure state using the same fields as xdg-shell popups.
#[derive(Debug, Default)]
pub struct PopupConfigureAttributes {
    pub initial_configure_sent: bool,
    pending_configures: Vec<PopupConfigure>,
    pub server_pending: Option<PopupSurfaceState>,
    pub last_acked: Option<PopupConfigure>,
}

impl PopupConfigureAttributes {
    pub fn with_server_pending(state: PopupSurfaceState) -> Self {
        Self {
            initial_configure_sent: false,
            pending_configures: Vec::new(),
            server_pending: Some(state),
            last_acked: None,
        }
    }

    pub fn ack_configure(&mut self, serial: Serial) -> Option<PopupSurfaceState> {
        let configure = self
            .pending_configures
            .iter()
            .find(|configure| configure.serial == serial)?
            .clone();

        self.last_acked = Some(configure.clone());
        self.pending_configures.retain(|configure| configure.serial > serial);
        Some(configure.state)
    }

    pub fn current_server_state(&self) -> PopupSurfaceState {
        self.pending_configures
            .last()
            .map(|configure| &configure.state)
            .or_else(|| self.last_acked.as_ref().map(|configure| &configure.state))
            .cloned()
            .unwrap_or_default()
    }

    pub fn has_pending_changes(&self) -> bool {
        self.server_pending
            .as_ref()
            .map(|state| *state != self.current_server_state())
            .unwrap_or(false)
    }

    pub fn with_pending_state<F>(&mut self, f: F)
    where
        F: FnOnce(&mut PopupSurfaceState),
    {
        if self.server_pending.is_none() {
            self.server_pending = Some(self.current_server_state());
        }
        f(self.server_pending.as_mut().unwrap());
    }

    pub fn send_pending_configure<F>(&mut self, mut send: F)
    where
        F: FnMut(PopupSurfaceState, PopupSurfaceState, Serial),
    {
        if !self.has_pending_changes() {
            return;
        }

        let mut new_state = self
            .server_pending
            .take()
            .expect("has_pending_changes implies server_pending");
        new_state.set_configured();

        let sent_state = self.current_server_state();
        if new_state == sent_state {
            return;
        }

        let serial = SERIAL_COUNTER.next_serial();
        self.pending_configures
            .push(PopupConfigure {
                state: new_state.clone(),
                serial,
            });
        self.initial_configure_sent = true;
        send(new_state, sent_state, serial);
    }
}
