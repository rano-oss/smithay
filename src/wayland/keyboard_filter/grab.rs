use std::sync::{Arc, Mutex};

use wayland_server::protocol::wl_surface::WlSurface;
use xkbcommon::xkb::Keycode;

use crate::{backend::input::KeyEvent, input::{keyboard::{GrabStartData, KbdInternal, KeyboardGrab, KeyboardInnerHandle, ModifiersState, XkbConfig}, Seat, SeatHandler}, utils::{Serial, SERIAL_COUNTER}, wayland::seat::WaylandFocus};


fn kbi<D: SeatHandler + 'static>(
    target: D::KeyboardFocus,
    queue: (),
    (repeat_delay, repeat_rate): (i32, i32),
) -> KbdInternal<D>
    where <D as SeatHandler>::KeyboardFocus: WaylandFocus
{
    let mut internal = KbdInternal::new(XkbConfig::default(), repeat_rate, repeat_delay).unwrap();
    //internal.focus = Some((target, SERIAL_COUNTER.next_serial()));
    //internal.set
    internal
}

/// Keyboard filtering grab
#[derive(Clone)]
pub struct KeyboardFilterGrab {
    pub(crate) inner: Arc<Mutex<GrabInner>>,
    target_surface: WlSurface,
}

struct GrabInner {
    queue: (),
}

impl KeyboardFilterGrab {
    pub fn new(target_surface: WlSurface) -> Self {
        Self {
            inner: Arc::new(Mutex::new(GrabInner { queue: () })),
            target_surface,
        }
    }
    /// Send the input to the focused keyboards
    pub fn input_dupe<D: SeatHandler + 'static>(
        inner: &mut KbdInternal<D>,
        seat: &Seat<D>,
        data: &mut D,
        keycode: Keycode,
        key_state: KeyEvent,
        modifiers: Option<ModifiersState>,
        serial: Serial,
        time: u32,
    ) {
// TODO: put interception here. FIXME: what to call to replay the event?
        dbg!(key_state);
        let (focus, _) = match inner.focus.as_mut() {
            Some(focus) => focus,
            None => return,
        };

        // Ensure keymap is up to date.
        #[cfg(feature = "wayland_frontend")]
        if let Some(keyboard_handle) = seat.get_keyboard() {
            let keymap_file = keyboard_handle.arc.keymap.lock().unwrap();
            let mods = inner.mods_state;
            keyboard_handle.send_keymap(data, &Some(focus), &keymap_file, mods);
        }
/*
        // key event must be sent before modifiers event for libxkbcommon
        // to process them correctly
        let key = KeysymHandle {
            xkb: &inner.xkb,
            keycode,
        };

        focus.key(seat, data, key, key_state, serial, time);
        if let Some(mods) = modifiers {
            focus.modifiers(seat, data, mods, serial);
        }*/
    }
}

impl<D> KeyboardGrab<D> for KeyboardFilterGrab
where
    D: SeatHandler + 'static,
{
    fn input(
        &mut self,
        data: &mut D,
        handle: &mut KeyboardInnerHandle<'_, D>,
        keycode: Keycode,
        key_event: KeyEvent,
        modifiers: Option<ModifiersState>,
        serial: Serial,
        time: u32,
    ) {
        
        let (repeat_delay, repeat_rate) = handle.repeat_info();
        let q = self.inner.lock().unwrap();
        /*let mut keyboard_inner = kbi(
            &self.target_surface,
            q.queue,
            (repeat_delay, repeat_rate),
        );*/
        // FIXME: default config
        let fake_seat = super::fake_seat::fake_seat(
            XkbConfig::default(),
            repeat_delay,
            repeat_rate,
        );
        //fake_seat.set_
        let kb = fake_seat.get_keyboard().unwrap();
        kb.arc.internal.lock().unwrap().focus;
        //fake_seat.get_keyboard().unwrap().set_grab(data, , SERIAL_COUNTER.next_serial());
        //.arc.internal.lock().unwrap().
        KeyboardInnerHandle::<D>::input_generic(
        //Self::input_dupe(
            &mut keyboard_inner,
            &fake_seat,
            data,
            keycode, key_event, modifiers, serial, time,
        )
    }

    fn set_focus(
        &mut self,
        data: &mut D,
        handle: &mut KeyboardInnerHandle<'_, D>,
        focus: Option<<D as SeatHandler>::KeyboardFocus>,
        serial: crate::utils::Serial,
    ) {
        unimplemented!();
        handle.set_focus(data, focus, serial)
    }
    
    fn start_data(&self) -> &GrabStartData<D> {
        &GrabStartData { focus: None }
    }

    fn unset(&mut self, _data: &mut D) {
        unimplemented!()
    }
}
