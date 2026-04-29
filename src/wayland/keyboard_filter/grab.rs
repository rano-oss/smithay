use std::sync::{Arc, Mutex};

use wayland_server::protocol::{wl_keyboard::WlKeyboard, wl_surface::WlSurface};
use xkbcommon::xkb::Keycode;

use crate::{backend::input::KeyEvent, input::{keyboard::{GrabStartData, KeyboardGrab, KeyboardHandle, KeyboardInnerHandle, KeymapFile, ModifiersState, XkbConfig}, SeatHandler}, utils::Serial, wayland::keyboard_filter::{queue::KeyboardEvent, DispatchQueue}};


/// Keyboard filtering grab
pub struct KeyboardFilterGrab<D: SeatHandler> {
    inner: DispatchQueue,
    keyboard: WlKeyboard,
    /// Shared with the actual keyboard.
    /// This is racy: updates get applied immediately even when events get delayed.
    keymap: Arc<Mutex<KeymapFile>>,
    target_surface: WlSurface,
    /// This function must come all the way from KeyboardFilter.
    /// It is only called from InputMethod, which does not carry the
    /// `<D as SeatHandler>::KeyboardFocus: WaylandFocus,` bound,
    /// so that must not be in the call signatures.
    /// Instead, the `fn` here is filled in KeyboardFilter::request where D does carry that bound. In effect, the bound is erased for external callers.
    register_kbd: fn(&KeyboardHandle<D>, &WlKeyboard, Option<&WlSurface>),
}

impl<D: SeatHandler> KeyboardFilterGrab<D> {
    pub fn new(
        register_kbd: fn(
            &KeyboardHandle<D>,
            &WlKeyboard,
            Option<&WlSurface>,
        ),
        keyboard: WlKeyboard,
        target_surface: WlSurface,
        keymap: Arc<Mutex<KeymapFile>>,
        inner: DispatchQueue,
    ) -> Self {
        Self { keyboard, inner, target_surface, keymap, register_kbd: register_kbd }
    }
}

impl<D> KeyboardGrab<D> for KeyboardFilterGrab<D>
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
        let q = &self.inner;
        q.push_event(KeyboardEvent {
            keycode, key_event, modifiers, serial, time,
        });
        // The keyboard is recreated every time to keep code simple, even if it prints on the console and calls xkb code. Recreating means the demo doesn't have to worry about stale values.
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
        (self.register_kbd)(&kb, &self.keyboard, Some(&self.target_surface));
        
        let inner = kb.arc.internal.lock().unwrap();
        let inner = inner.xkb_related_state();

        KeyboardInnerHandle::<D>::input_generic(
            inner,
            Some(&self.target_surface),
            &fake_seat,
            keycode, key_event, modifiers, serial, time,
            true,
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

    fn unset(&mut self, _data: &mut D) {}
}
