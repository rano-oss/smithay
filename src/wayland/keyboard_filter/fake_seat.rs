use crate::input::{keyboard::{KeyboardHandle, XkbConfig}, Seat, SeatHandler, SeatState};

pub(crate) fn fake_seat<D: SeatHandler + 'static>(
    xkb_config: XkbConfig<'_>,
    repeat_delay: i32,
    repeat_rate: i32,
) -> Seat<D> {
    let mut state = SeatState::new();
    let seat = state.new_seat("fake keyboard filter seat");
    {
        let arc = &seat.arc;
        let mut inner = arc.inner.lock().unwrap();
        inner.keyboard = Some(KeyboardHandle::new(xkb_config, repeat_delay, repeat_rate).unwrap())
    }
    seat
}