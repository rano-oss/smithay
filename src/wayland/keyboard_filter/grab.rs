use std::sync::{Arc, Mutex};

use wayland_server::protocol::wl_surface::WlSurface;
use xkbcommon::xkb::Keycode;

use crate::{backend::input::KeyEvent, input::{keyboard::{GrabStartData, KbdInternal, KeyboardGrab, KeyboardInnerHandle, KeyboardTarget, KeyboardTargetSimple, KeymapFile, ModifiersState, XkbConfig}, Seat, SeatHandler}, utils::{Serial, SERIAL_COUNTER}, wayland::seat::WaylandFocus};


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

#[derive(Clone)]
struct SurfaceTarget(WlSurface);

impl<D: SeatHandler> KeyboardTargetSimple<D> for SurfaceTarget {
    fn key(
        &self,
        seat: &Seat<D>,
        key: crate::input::keyboard::KeysymHandle<'_>,
        state: KeyEvent,
        serial: Serial,
        time: u32,
    ) {
        unimplemented!()
    }
    fn modifiers(&self, seat: &Seat<D>, modifiers: ModifiersState, serial: Serial) {
        unimplemented!()
    }
}

/// Keyboard filtering grab
//#[derive(Clone)]
pub struct KeyboardFilterGrab {
    inner: Arc<Mutex<GrabInner>>,
    /// Shared with the actual keyboard.
    /// This is racy: updates get applied immediately even when events get delayed.
    keymap: Arc<Mutex<KeymapFile>>,
    target_surface: SurfaceTarget,
}

struct GrabInner {
    queue: (),
}

impl KeyboardFilterGrab {
    pub fn new(target_surface: WlSurface, keymap: Arc<Mutex<KeymapFile>>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(GrabInner { queue: () })),
            target_surface: SurfaceTarget(target_surface),
            keymap,
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
        _data: &mut D,
        handle: &mut KeyboardInnerHandle<'_, D>,
        keycode: Keycode,
        key_event: KeyEvent,
        modifiers: Option<ModifiersState>,
        serial: Serial,
        time: u32,
    ) {
        
        let (repeat_delay, repeat_rate) = handle.repeat_info();
        let q = self.inner.lock().unwrap();

        let fake_seat = super::fake_seat::fake_seat(
            XkbConfig::default(),
            repeat_delay,
            repeat_rate,
        );
        let mut kb = fake_seat.get_keyboard().unwrap();
        // Keyboard does NOT need focus because it's passed explicitly.
        // It *can't* carry the correct (redirected) focus anyway because focus is of a generic type defined by the compositor.
        // Alternatively, we could modify KeyboardTarget to have a From<WlSurface>...
        
        // Keyboard must have a correct keymap file
        //FIXME
        Arc::get_mut(&mut kb.arc).unwrap().keymap = self.keymap.clone();
        let kb = kb;
        
        let inner = kb.arc.internal.lock().unwrap();
        let inner = inner.xkb_related_state();

        KeyboardInnerHandle::<D>::input_generic(
            inner,
            Some(&self.target_surface),
            &fake_seat,
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
