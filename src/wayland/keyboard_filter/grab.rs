use std::sync::{Arc, Mutex};

use wayland_server::protocol::{wl_keyboard::WlKeyboard, wl_surface::WlSurface};
use xkbcommon::xkb::Keycode;

use crate::{backend::input::KeyEvent, input::{keyboard::{GrabStartData, KbdInternal, KeyboardGrab, KeyboardInnerHandle, KeymapFile, ModifiersState, XkbConfig}, SeatHandler}, utils::Serial, wayland::seat::WaylandFocus};


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
pub struct KeyboardFilterGrab {
    inner: Arc<Mutex<GrabInner>>,
    keyboard: WlKeyboard,
    /// Shared with the actual keyboard.
    /// This is racy: updates get applied immediately even when events get delayed.
    keymap: Arc<Mutex<KeymapFile>>,
    target_surface: WlSurface,
}

struct GrabInner {
    queue: (),
}

impl KeyboardFilterGrab {
    pub fn new(keyboard: WlKeyboard, target_surface: WlSurface, keymap: Arc<Mutex<KeymapFile>>) -> Self {
        Self {
            keyboard,
            inner: Arc::new(Mutex::new(GrabInner { queue: () })),
            target_surface,
            keymap,
        }
    }
}

impl<D> KeyboardGrab<D> for KeyboardFilterGrab
where
    D: SeatHandler + 'static,
    <D as SeatHandler>::KeyboardFocus: WaylandFocus,
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
            self.keymap.clone(),
        );
        let kb = fake_seat.get_keyboard().unwrap();
        // Keyboard does NOT need focus because it's passed explicitly.
        // It *can't* carry the correct (redirected) focus anyway because focus is of a generic type defined by the compositor.
        // Alternatively, we could modify KeyboardTarget to have a From<WlSurface>...
        kb.register_kbd(&self.keyboard, Some(&self.target_surface));
        
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
