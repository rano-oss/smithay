use crate::input::{keyboard::{KeyboardHandle, KeymapFile, XkbConfig}, Seat, SeatHandler, SeatState};

use std::sync::{Arc, Mutex};

pub(crate) fn fake_seat<D: SeatHandler + 'static>(
    xkb_config: XkbConfig<'_>,
    repeat_delay: i32,
    repeat_rate: i32,
    keymap: Arc<Mutex<KeymapFile>>,
) -> Seat<D> {
    let mut state = SeatState::new();
    let seat = state.new_seat("fake keyboard filter seat");
    {
        let arc = &seat.arc;
        let mut inner = arc.inner.lock().unwrap();
        
        let mut kb = KeyboardHandle::new(xkb_config, repeat_delay, repeat_rate).unwrap();
        Arc::get_mut(&mut kb.arc).unwrap().keymap = keymap;
        inner.keyboard = Some(kb)
    }
    seat
}