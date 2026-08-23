//! Keyboard-related types for smithay's input abstraction

use crate::backend::input::KeyState;
use crate::utils::{IsAlive, SERIAL_COUNTER, Serial};
use downcast_rs::{Downcast, impl_downcast};
use std::collections::{HashMap, HashSet};
#[cfg(feature = "wayland_frontend")]
use std::sync::RwLock;
use std::{
    default::Default,
    fmt, io,
    sync::{Arc, Mutex},
};
use thiserror::Error;
use tracing::{debug, info, info_span, instrument, trace};
use wkb::{NamedKey, PhysicalKey, WKB};

pub use xkbcommon::xkb::{self, ContextFlags, Keysym, keysyms};

use super::{GrabStatus, Seat, SeatHandler};

#[cfg(feature = "wayland_frontend")]
use wayland_server::{Resource, Weak};
#[cfg(feature = "wayland_frontend")]
mod keymap_file;
#[cfg(feature = "wayland_frontend")]
pub use keymap_file::{KeymapFile, KeymapFileId};

mod modifiers_state;
pub use modifiers_state::{ModifiersState, SerializedMods};

mod xkb_config;
pub use xkb_config::XkbConfig;

#[cfg(test)]
mod tests;

/// Trait representing object that can receive keyboard interactions
pub trait KeyboardTarget<D>: IsAlive + fmt::Debug + Send
where
    D: SeatHandler,
{
    /// Keyboard focus of a given seat was assigned to this handler
    fn enter(&self, seat: &Seat<D>, data: &mut D, keys: Vec<KeyHandle<'_>>, serial: Serial);
    /// The keyboard focus of a given seat left this handler
    fn leave(&self, seat: &Seat<D>, data: &mut D, serial: Serial);
    /// A key was pressed on a keyboard from a given seat
    fn key(
        &self,
        seat: &Seat<D>,
        data: &mut D,
        key: KeyHandle<'_>,
        state: KeyState,
        serial: Serial,
        time: u32,
    );
    /// Hold modifiers were changed on a keyboard from a given seat
    fn modifiers(&self, seat: &Seat<D>, data: &mut D, modifiers: ModifiersState, serial: Serial);
    /// Keyboard focus of a given seat moved from another handler to this handler
    fn replace(
        &self,
        replaced: <D as SeatHandler>::KeyboardFocus,
        seat: &Seat<D>,
        data: &mut D,
        keys: Vec<KeyHandle<'_>>,
        modifiers: ModifiersState,
        serial: Serial,
    ) {
        KeyboardTarget::<D>::leave(&replaced, seat, data, serial);
        KeyboardTarget::<D>::enter(self, seat, data, keys, serial);
        KeyboardTarget::<D>::modifiers(self, seat, data, modifiers, serial);
    }
}

/// Current state of the led when available
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub struct LedState {
    /// State of NUMLOCK led
    pub num: Option<bool>,
    /// State of CAPSLOCK led
    pub caps: Option<bool>,
    /// State of SCROLLLOCK led
    pub scroll: Option<bool>,
}

fn led_state_from_wkb(wkb: &WKB) -> LedState {
    let leds = wkb.leds_state();
    LedState {
        num: Some(leds.num_lock),
        caps: Some(leds.caps_lock),
        scroll: Some(leds.scroll_lock),
    }
}

/// Identifies which input source a key event comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyboardSource {
    /// The physical keyboard(s) driven directly by the compositor's input backend.
    Physical,
    /// An auxiliary input source (e.g. a `zwp_virtual_keyboard_v1` instance or a libei
    /// connection), distinguished by a compositor-assigned opaque id.
    Auxiliary(u64),
}

impl KeyboardSource {
    /// The default source used by [`KeyboardHandle::input`] (the physical keyboard).
    pub const MAIN: KeyboardSource = KeyboardSource::Physical;

    /// Mint a fresh, process-unique auxiliary source.
    pub fn new_auxiliary() -> Self {
        static NEXT_AUX_SOURCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        KeyboardSource::Auxiliary(NEXT_AUX_SOURCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

pub(crate) struct KbdInternal<D: SeatHandler> {
    pub(crate) focus: Option<(<D as SeatHandler>::KeyboardFocus, Serial)>,
    pending_focus: Option<<D as SeatHandler>::KeyboardFocus>,
    pub(crate) pressed_keys: HashSet<u32>,
    pub(crate) key_sources: HashMap<u32, HashSet<KeyboardSource>>,
    pub(crate) forwarded_pressed_keys: HashSet<u32>,
    pub(crate) mods_state: ModifiersState,
    wkb: Arc<Mutex<WKB>>,
    pub(crate) repeat_rate: i32,
    pub(crate) repeat_delay: i32,
    pub(crate) led_state: LedState,
    grab: GrabStatus<dyn KeyboardGrab<D>>,
}

// focus_hook does not implement debug, so we have to impl Debug manually
impl<D: SeatHandler> fmt::Debug for KbdInternal<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KbdInternal")
            .field("focus", &self.focus)
            .field("pending_focus", &self.pending_focus)
            .field("pressed_keys", &self.pressed_keys)
            .field("forwarded_pressed_keys", &self.forwarded_pressed_keys)
            .field("mods_state", &self.mods_state)
            .field("repeat_rate", &self.repeat_rate)
            .field("repeat_delay", &self.repeat_delay)
            .finish()
    }
}

impl<D: SeatHandler + 'static> KbdInternal<D> {
    fn new(xkb_config: XkbConfig<'_>, repeat_rate: i32, repeat_delay: i32) -> Result<KbdInternal<D>, ()> {
        let wkb = WKB::new_from_names(
            xkb_config.rules,
            xkb_config.model,
            xkb_config.layout,
            xkb_config.variant,
            xkb_config.options.as_deref(),
        )
        .map_err(|_| ())?;
        let led_state = led_state_from_wkb(&wkb);
        let mut mods_state = ModifiersState::default();
        mods_state.update_with(&wkb);
        Ok(KbdInternal {
            focus: None,
            pending_focus: None,
            pressed_keys: HashSet::new(),
            key_sources: HashMap::new(),
            forwarded_pressed_keys: HashSet::new(),
            mods_state,
            repeat_rate,
            repeat_delay,
            led_state,
            grab: GrabStatus::None,
            wkb: Arc::new(Mutex::new(wkb)),
        })
    }

    // Feed a key event from `source` into the shared seat state. Returns
    // `(modifiers_changed, leds_changed, is_transition)`.
    //
    // `is_transition` is `true` only when this event actually changes the combined pressed set
    // i.e. the first source to press a keycode, or the last source to release it.
    fn key_input(&mut self, source: KeyboardSource, keycode: u32, state: KeyState) -> (bool, bool, bool) {
        // track pressed keys per source, the seat WKB only follows the *combined* set
        let wkb = &mut self.wkb.lock().unwrap();
        let changes = match state {
            KeyState::Pressed => {
                let holders = self.key_sources.entry(keycode).or_default();
                let was_held = !holders.is_empty();
                holders.insert(source);
                if was_held {
                    // already down from another source: absorb
                    return (false, false, false);
                }
                self.pressed_keys.insert(keycode);
                wkb.press_key(keycode)
            }
            KeyState::Released => {
                match self.key_sources.get_mut(&keycode) {
                    Some(holders) => {
                        holders.remove(&source);
                        if !holders.is_empty() {
                            // still held by another source: absorb
                            return (false, false, false);
                        }
                        self.key_sources.remove(&keycode);
                        self.pressed_keys.remove(&keycode);
                        wkb.release_key(keycode)
                    }
                    // not tracked as held: absorb
                    None => return (false, false, false),
                }
            }
        };

        if changes.modifiers_updated {
            self.mods_state.update_with(wkb);
        }
        if changes.leds_updated {
            self.led_state = led_state_from_wkb(wkb);
        }

        (changes.modifiers_updated, changes.leds_updated, true)
    }

    /// Release every keycode currently held by `source`, as if the source sent a release for
    /// each. A keycode only actually transitions up (and gets forwarded) if no other source is
    /// still holding it. Returns the keycodes that transitioned up, so the caller can forward
    /// the releases to the focused client.
    fn release_source_keys(&mut self, source: KeyboardSource) -> Vec<u32> {
        let held: Vec<u32> = self
            .key_sources
            .iter()
            .filter(|(_, holders)| holders.contains(&source))
            .map(|(keycode, _)| *keycode)
            .collect();
        let mut transitioned = Vec::new();
        for keycode in held {
            let (_, _, is_transition) = self.key_input(source, keycode, KeyState::Released);
            if is_transition {
                transitioned.push(keycode);
            }
        }
        transitioned
    }

    fn with_grab<F>(&mut self, data: &mut D, seat: &Seat<D>, f: F)
    where
        F: FnOnce(&mut D, &mut KeyboardInnerHandle<'_, D>, &mut dyn KeyboardGrab<D>),
    {
        let mut grab = std::mem::replace(&mut self.grab, GrabStatus::Borrowed);
        match grab {
            GrabStatus::Borrowed => panic!("Accessed a keyboard grab from within a keyboard grab access."),
            GrabStatus::Active(_, ref mut handler) => {
                // If this grab is associated with a surface that is no longer alive, discard it
                if let Some(ref surface) = handler.start_data().focus {
                    if !surface.alive() {
                        handler.unset(data);
                        self.grab = GrabStatus::None;
                        f(
                            data,
                            &mut KeyboardInnerHandle { inner: self, seat },
                            &mut DefaultGrab,
                        );
                        return;
                    }
                }
                f(
                    data,
                    &mut KeyboardInnerHandle { inner: self, seat },
                    &mut **handler,
                );
            }
            GrabStatus::None => {
                f(
                    data,
                    &mut KeyboardInnerHandle { inner: self, seat },
                    &mut DefaultGrab,
                );
            }
        }

        if let GrabStatus::Borrowed = self.grab {
            // the grab has not been ended nor replaced, put it back in place
            self.grab = grab;
        }
    }
}

/// Errors that can be encountered when creating a keyboard handler
#[derive(Debug, Error)]
pub enum Error {
    /// The keymap could not be loaded.
    #[error("Failed to load the specified keymap")]
    BadKeymap,
    /// Smithay could not create a tempfile to share the keymap with clients
    #[error("Failed to create tempfile to share the keymap: {0}")]
    IoError(io::Error),
}

pub(crate) struct KbdRc<D: SeatHandler> {
    pub(crate) internal: Mutex<KbdInternal<D>>,
    #[cfg(feature = "wayland_frontend")]
    pub(crate) keymap: Mutex<KeymapFile>,
    #[cfg(feature = "wayland_frontend")]
    pub(crate) known_kbds: Mutex<Vec<Weak<wayland_server::protocol::wl_keyboard::WlKeyboard>>>,
    #[cfg(feature = "wayland_frontend")]
    pub(crate) last_enter: Mutex<Option<Serial>>,
    pub(crate) span: tracing::Span,
    #[cfg(feature = "wayland_frontend")]
    pub(crate) active_keymap: RwLock<KeymapFileId>,
}

#[cfg(not(feature = "wayland_frontend"))]
impl<D: SeatHandler> fmt::Debug for KbdRc<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KbdRc").field("internal", &self.internal).finish()
    }
}

#[cfg(feature = "wayland_frontend")]
impl<D: SeatHandler> fmt::Debug for KbdRc<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KbdRc")
            .field("internal", &self.internal)
            .field("keymap", &self.keymap)
            .field("known_kbds", &self.known_kbds)
            .field("last_enter", &self.last_enter)
            .finish()
    }
}

/// Handle to a key event's evdev keycode, used to resolve key identity and text from WKB.
pub struct KeyHandle<'a> {
    wkb: &'a Mutex<WKB>,
    evdev_code: u32,
}

impl fmt::Debug for KeyHandle<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.evdev_code)
    }
}

impl<'a> KeyHandle<'a> {
    /// Physical key position for this evdev keycode.
    pub fn physical_key(&self) -> PhysicalKey {
        self.wkb().lock().unwrap().physical_key(self.evdev_code)
    }

    /// Named non-character key identity under the current modifier state.
    pub fn named_key(&self) -> NamedKey {
        self.wkb().lock().unwrap().named_key(self.evdev_code)
    }

    /// Character produced by this key under the current modifier state.
    pub fn key_char(&self) -> Option<char> {
        self.wkb().lock().unwrap().key_char(self.evdev_code)
    }

    /// Raw Linux/evdev keycode for this key event.
    pub fn evdev_code(&self) -> u32 {
        self.evdev_code
    }

    /// Access the WKB instance backing this handle.
    pub fn wkb(&self) -> &Mutex<WKB> {
        self.wkb
    }
}

/// The currently active WKB state exposed for layout changes and other mutations.
pub struct WkbContext<'a> {
    wkb: &'a Mutex<WKB>,
    mods_state: &'a mut ModifiersState,
    mods_changed: &'a mut bool,
    leds_changed: &'a mut bool,
}

impl WkbContext<'_> {
    /// Access the WKB instance.
    pub fn wkb(&self) -> &Mutex<WKB> {
        self.wkb
    }

    /// Set layout of the keyboard to the given index.
    pub fn set_layout(&mut self, layout: Layout) {
        let changes = {
            let mut wkb = self.wkb.lock().unwrap();
            wkb.update_modifiers(
                self.mods_state.serialized.depressed,
                self.mods_state.serialized.latched,
                self.mods_state.serialized.locked,
                layout.0,
            )
        };
        if changes.modifiers_updated {
            let wkb = self.wkb.lock().unwrap();
            self.mods_state.update_with(&wkb);
        }
        *self.mods_changed |= changes.modifiers_updated;
        *self.leds_changed |= changes.leds_updated;
    }

    /// Switches layout forward cycling when it reaches the end.
    pub fn cycle_next_layout(&mut self) {
        let wkb = self.wkb.lock().unwrap();
        let next_layout = (wkb.active_layout_idx() + 1) % wkb.num_layouts();
        drop(wkb);
        self.set_layout(Layout(next_layout as u32));
    }

    /// Switches layout backward cycling when it reaches the start.
    pub fn cycle_prev_layout(&mut self) {
        let wkb = self.wkb.lock().unwrap();
        let prev_layout = (wkb.num_layouts() + wkb.active_layout_idx() - 1) % wkb.num_layouts();
        drop(wkb);
        self.set_layout(Layout(prev_layout as u32));
    }
}

impl fmt::Debug for WkbContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WkbContext")
            .field("mods_state", &self.mods_state)
            .field("mods_changed", &self.mods_changed)
            .finish()
    }
}

/// Reference to the XkbLayout in the active keymap.
///
/// The layout may become invalid after calling [`KeyboardHandle::set_xkb_config`]
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout(pub xkb::LayoutIndex);

/// Result for key input filtering (see [`KeyboardHandle::input`])
#[derive(Debug)]
pub enum FilterResult<T> {
    /// Forward the given keycode to the client
    Forward,
    /// Do not forward and return value
    Intercept(T),
}

/// Data about the event that started the grab.
pub struct GrabStartData<D: SeatHandler> {
    /// The focused surface, if any, at the start of the grab.
    pub focus: Option<<D as SeatHandler>::KeyboardFocus>,
}

impl<D: SeatHandler + 'static> fmt::Debug for GrabStartData<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrabStartData")
            .field("focus", &self.focus)
            .finish()
    }
}

impl<D: SeatHandler + 'static> Clone for GrabStartData<D> {
    fn clone(&self) -> Self {
        GrabStartData {
            focus: self.focus.clone(),
        }
    }
}

/// A trait to implement a keyboard grab
///
/// In some context, it is necessary to temporarily change the behavior of the keyboard. This is
/// typically known as a keyboard grab. A example would be, during a popup grab the keyboard focus
/// will not be changed and stay on the grabbed popup.
///
/// This trait is the interface to intercept regular keyboard events and change them as needed, its
/// interface mimics the [`KeyboardHandle`] interface.
///
/// If your logic decides that the grab should end, both [`KeyboardInnerHandle`] and [`KeyboardHandle`] have
/// a method to change it.
///
/// When your grab ends (either as you requested it or if it was forcefully cancelled by the server),
/// the struct implementing this trait will be dropped. As such you should put clean-up logic in the destructor,
/// rather than trying to guess when the grab will end.
pub trait KeyboardGrab<D: SeatHandler>: Downcast + Send {
    /// An input was reported.
    ///
    /// `modifiers` are only passed when their state actually changes. The modifier must be
    /// sent after the key event.
    #[allow(clippy::too_many_arguments)]
    fn input(
        &mut self,
        data: &mut D,
        handle: &mut KeyboardInnerHandle<'_, D>,
        keycode: u32,
        state: KeyState,
        modifiers: Option<ModifiersState>,
        serial: Serial,
        time: u32,
    );

    /// A focus change was requested.
    fn set_focus(
        &mut self,
        data: &mut D,
        handle: &mut KeyboardInnerHandle<'_, D>,
        focus: Option<<D as SeatHandler>::KeyboardFocus>,
        serial: Serial,
    );

    /// The data about the event that started the grab.
    fn start_data(&self) -> &GrabStartData<D>;

    /// The grab has been unset or replaced with another grab.
    fn unset(&mut self, data: &mut D);
}

impl_downcast!(KeyboardGrab<D> where D: SeatHandler);

/// An handle to a keyboard handler
///
/// It can be cloned and all clones manipulate the same internal state.
///
/// This handle gives you 2 main ways to interact with the keyboard handling:
///
/// - set the current focus for this keyboard: designing the surface that will receive the key inputs
///   using the [`KeyboardHandle::set_focus`] method.
/// - process key inputs from the input backend, allowing them to be caught at the compositor-level
///   or forwarded to the client. See the documentation of the [`KeyboardHandle::input`] method for
///   details.
pub struct KeyboardHandle<D: SeatHandler> {
    pub(crate) arc: Arc<KbdRc<D>>,
}

impl<D: SeatHandler> fmt::Debug for KeyboardHandle<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyboardHandle").field("arc", &self.arc).finish()
    }
}

impl<D: SeatHandler> Clone for KeyboardHandle<D> {
    #[inline]
    fn clone(&self) -> Self {
        KeyboardHandle {
            arc: self.arc.clone(),
        }
    }
}

impl<D: SeatHandler> ::std::cmp::PartialEq for KeyboardHandle<D> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.arc, &other.arc)
    }
}

impl<D: SeatHandler + 'static> KeyboardHandle<D> {
    /// Create a keyboard handler from a set of RMLVO rules
    pub(crate) fn new(xkb_config: XkbConfig<'_>, repeat_delay: i32, repeat_rate: i32) -> Result<Self, Error> {
        let span = info_span!("input_keyboard");
        let _guard = span.enter();

        info!("Initializing a keyboard handler with keymap query");
        let internal = KbdInternal::new(xkb_config, repeat_rate, repeat_delay).map_err(|_| {
            debug!("Loading keymap failed");
            Error::BadKeymap
        })?;
        let wkb = internal.wkb.try_lock().unwrap();

        info!(name = wkb.layout_name(wkb.active_layout_idx()), "Loaded Keymap");

        #[cfg(feature = "wayland_frontend")]
        let keymap_file = KeymapFile::new(wkb.as_xkb_string().unwrap()); // WKB should always have a valid keymap
        #[cfg(feature = "wayland_frontend")]
        let active_keymap = keymap_file.id();

        drop(_guard);
        drop(wkb);
        Ok(Self {
            arc: Arc::new(KbdRc {
                #[cfg(feature = "wayland_frontend")]
                keymap: Mutex::new(keymap_file),
                internal: Mutex::new(internal),
                #[cfg(feature = "wayland_frontend")]
                known_kbds: Mutex::new(Vec::new()),
                #[cfg(feature = "wayland_frontend")]
                last_enter: Mutex::new(None),
                #[cfg(feature = "wayland_frontend")]
                active_keymap: RwLock::new(active_keymap),
                span,
            }),
        })
    }

    #[cfg(feature = "wayland_frontend")]
    #[instrument(parent = &self.arc.span, skip(self, data, keymap))]
    pub(crate) fn change_keymap(
        &self,
        data: &mut D,
        focus: &Option<&mut <D as SeatHandler>::KeyboardFocus>,
        keymap: String,
        mods: ModifiersState,
    ) {
        let mut keymap_file = self.arc.keymap.lock().unwrap();
        keymap_file.change_keymap(keymap);

        self.send_keymap(data, focus, &keymap_file, mods);
    }

    /// Send a new wl_keyboard keymap, without updating the internal keymap.
    ///
    /// Returns `true` if the keymap changed from the previous keymap.
    #[cfg(feature = "wayland_frontend")]
    #[instrument(parent = &self.arc.span, skip(self, data, keymap_file))]
    pub(crate) fn send_keymap(
        &self,
        data: &mut D,
        focus: &Option<&mut <D as SeatHandler>::KeyboardFocus>,
        keymap_file: &KeymapFile,
        mods: ModifiersState,
    ) -> bool {
        use std::os::unix::io::AsFd;
        use tracing::warn;
        use wayland_server::{Resource, protocol::wl_keyboard::KeymapFormat};

        // Ignore request which do not change the keymap.
        let new_id = keymap_file.id();
        if new_id == *self.arc.active_keymap.read().unwrap() {
            return false;
        }
        *self.arc.active_keymap.write().unwrap() = new_id;

        // Update keymap for every wl_keyboard.
        let known_kbds = &self.arc.known_kbds;
        for kbd in &*known_kbds.lock().unwrap() {
            let Ok(kbd) = kbd.upgrade() else {
                continue;
            };

            let res = keymap_file.with_fd(kbd.version() >= 7, |fd, size| {
                kbd.keymap(KeymapFormat::XkbV1, fd.as_fd(), size as u32)
            });
            if let Err(e) = res {
                warn!(
                    err = ?e,
                    "Failed to send keymap to client"
                );
            }
        }

        // Send updated modifiers.
        let seat = self.get_seat(data);
        if let Some(focus) = focus {
            focus.modifiers(&seat, data, mods, SERIAL_COUNTER.next_serial());
        }

        true
    }

    fn update_wkb_state(&self, data: &mut D, mut wkb: WKB) {
        let mut internal = self.arc.internal.lock().unwrap();

        let previous_led_state = internal.led_state;
        let _previous_mods = internal.mods_state;

        for key in &internal.pressed_keys {
            wkb.press_key(*key);
        }

        let mods = {
            let mut mods_state = ModifiersState::default();
            mods_state.update_with(&wkb);
            mods_state
        };
        let led_state = led_state_from_wkb(&wkb);
        let keymap = wkb.as_xkb_string().unwrap_or_default();

        internal.wkb = Arc::new(Mutex::new(wkb));
        internal.mods_state = mods;
        internal.led_state = led_state;

        let focus = internal.focus.as_mut().map(|(focus, _)| focus);

        #[cfg(not(feature = "wayland_frontend"))]
        if let Some(focus) = focus.as_ref() {
            if mods != previous_mods {
                let seat = self.get_seat(data);
                focus.modifiers(&seat, data, mods, SERIAL_COUNTER.next_serial());
            }
        }

        #[cfg(feature = "wayland_frontend")]
        self.change_keymap(data, &focus, keymap, mods);

        if led_state != previous_led_state {
            std::mem::drop(internal);
            let seat = self.get_seat(data);
            data.led_state_changed(&seat, led_state);
        }
    }

    /// Change the [`Keymap`](xkb::Keymap) used by the keyboard.
    ///
    /// The input is a keymap in XKB_KEYMAP_FORMAT_TEXT_V1 format.
    pub fn set_keymap_from_string(&self, data: &mut D, keymap: String) -> Result<(), Error> {
        let keymap = WKB::new_from_string(&keymap).or_else(|error| {
            debug!("Loading keymap from string failed: {}", error);
            Err(Error::BadKeymap)
        })?;
        // self.arc.internal.lock().unwrap().wkb = keymap;
        self.update_wkb_state(data, keymap);
        Ok(())
    }

    /// Change the [`XkbConfig`] used by the keyboard.
    pub fn set_xkb_config(&self, data: &mut D, xkb_config: XkbConfig<'_>) -> Result<(), Error> {
        let keymap = WKB::new_from_names(
            xkb_config.rules,
            xkb_config.model,
            xkb_config.layout,
            xkb_config.variant,
            xkb_config.options.as_deref(),
        )
        .or_else(|error| {
            debug!("Loading keymap from string failed: {}", error);
            Err(Error::BadKeymap)
        })?;
        self.update_wkb_state(data, keymap);
        Ok(())
    }

    /// Access the underlying WKB state and perform mutable operations on it, like
    /// changing layouts.
    ///
    /// The changes to the state are automatically broadcasted to the focused client on exit.
    pub fn with_wkb_state<F, T>(&self, data: &mut D, mut callback: F) -> T
    where
        F: FnMut(WkbContext<'_>) -> T,
    {
        let (result, new_led_state) = {
            let internal = &mut *self.arc.internal.lock().unwrap();
            let mut mods_changed = false;
            let mut leds_changed = false;
            let state = WkbContext {
                mods_state: &mut internal.mods_state,
                wkb: &internal.wkb,
                mods_changed: &mut mods_changed,
                leds_changed: &mut leds_changed,
            };

            let result = callback(state);

            if leds_changed {
                internal.led_state = led_state_from_wkb(&internal.wkb.lock().unwrap());
            }

            if mods_changed {
                if let Some((focus, _)) = internal.focus.as_mut() {
                    let seat = self.get_seat(data);
                    focus.modifiers(&seat, data, internal.mods_state, SERIAL_COUNTER.next_serial());
                };
            }

            (result, leds_changed.then_some(internal.led_state))
        };

        if let Some(led_state) = new_led_state {
            let seat = self.get_seat(data);
            data.led_state_changed(&seat, led_state)
        }

        result
    }

    /// Change the current grab on this keyboard to the provided grab
    ///
    /// Overwrites any current grab.
    pub fn set_grab<G: KeyboardGrab<D> + 'static>(&self, data: &mut D, grab: G, serial: Serial) {
        let mut inner = self.arc.internal.lock().unwrap();
        if let GrabStatus::Active(_, handler) = &mut inner.grab {
            handler.unset(data);
        }
        inner.grab = GrabStatus::Active(serial, Box::new(grab));
    }

    /// Remove any current grab on this keyboard, resetting it to the default behavior
    pub fn unset_grab(&self, data: &mut D) {
        let mut inner = self.arc.internal.lock().unwrap();
        if let GrabStatus::Active(_, handler) = &mut inner.grab {
            handler.unset(data);
        }
        inner.grab = GrabStatus::None;
    }

    /// Check if this keyboard is currently grabbed with this serial
    pub fn has_grab(&self, serial: Serial) -> bool {
        let guard = self.arc.internal.lock().unwrap();
        match guard.grab {
            GrabStatus::Active(s, _) => s == serial,
            _ => false,
        }
    }

    /// Check if this keyboard is currently being grabbed
    pub fn is_grabbed(&self) -> bool {
        let guard = self.arc.internal.lock().unwrap();
        !matches!(guard.grab, GrabStatus::None)
    }

    /// Returns the start data for the grab, if any.
    pub fn grab_start_data(&self) -> Option<GrabStartData<D>> {
        let guard = self.arc.internal.lock().unwrap();
        match &guard.grab {
            GrabStatus::Active(_, g) => Some(g.start_data().clone()),
            _ => None,
        }
    }

    /// Calls `f` with the active grab, if any.
    pub fn with_grab<T>(&self, f: impl FnOnce(Serial, &dyn KeyboardGrab<D>) -> T) -> Option<T> {
        let guard = self.arc.internal.lock().unwrap();
        if let GrabStatus::Active(s, g) = &guard.grab {
            Some(f(*s, &**g))
        } else {
            None
        }
    }

    /// Handle a keystroke
    ///
    /// All keystrokes from the input backend should be fed _in order_ to this method of the
    /// keyboard handler. It will internally track the state of the keymap.
    ///
    /// The `filter` argument is expected to be a closure which will peek at the generated input
    /// as interpreted by the keymap before it is forwarded to the focused client. If this closure
    /// returns [`FilterResult::Forward`], the input will be sent to the client. If it returns
    /// [`FilterResult::Intercept`], a value can be passed to be returned by the whole function.
    /// This mechanism can be used to implement compositor-level key bindings for example.
    ///
    /// The module [`keysyms`] exposes definitions of all possible keysyms to be compared against.
    /// This includes non-character keysyms, such as XF86 special keys.
    #[instrument(level = "trace", parent = &self.arc.span, skip(self, data, filter))]
    pub fn input<T, F>(
        &self,
        data: &mut D,
        keycode: u32,
        state: KeyState,
        serial: Serial,
        time: u32,
        filter: F,
    ) -> Option<T>
    where
        F: FnOnce(&mut D, &ModifiersState, KeyHandle<'_>) -> FilterResult<T>,
    {
        self.input_from_source(KeyboardSource::MAIN, data, keycode, state, serial, time, filter)
    }

    /// Like [`KeyboardHandle::input`], but attributes the event to a specific [`KeyboardSource`].
    #[allow(clippy::too_many_arguments)]
    pub fn input_from_source<T, F>(
        &self,
        source: KeyboardSource,
        data: &mut D,
        keycode: u32,
        state: KeyState,
        serial: Serial,
        time: u32,
        filter: F,
    ) -> Option<T>
    where
        F: FnOnce(&mut D, &ModifiersState, KeyHandle<'_>) -> FilterResult<T>,
    {
        trace!("Handling keystroke");

        let mut guard = self.arc.internal.lock().unwrap();
        let (mods_changed, leds_changed, is_transition) = guard.key_input(source, keycode, state);
        let led_state = guard.led_state;
        let mods_state = guard.mods_state;
        let wkb = guard.wkb.clone();
        std::mem::drop(guard);

        if leds_changed {
            let seat = self.get_seat(data);
            data.led_state_changed(&seat, led_state);
        }

        // The event was absorbed because another source is holding this keycode: don't
        // double-run the filter (avoids re-triggering shortcuts) and don't forward a duplicate.
        if !is_transition {
            return None;
        }

        let key_handle = KeyHandle {
            wkb: &wkb,
            evdev_code: keycode,
        };
        trace!(mods_state = ?mods_state, sym = ?key_handle.evdev_code, "Calling input filter");
        if let FilterResult::Intercept(val) = filter(data, &mods_state, key_handle) {
            trace!("Input was intercepted by filter");
            return Some(val);
        }

        self.input_forward(data, keycode, state, serial, time, mods_changed);
        None
    }

    /// Release every key currently held by `source` (e.g. when a virtual keyboard is destroyed
    /// or a libei connection drops), forwarding the resulting releases to the focused client.
    ///
    /// This is a teardown-only path: the releases are forwarded directly and do not run
    /// the compositor's input filter, so a departing source can't trigger a key binding
    pub fn release_source(&self, data: &mut D, source: KeyboardSource) {
        let (transitioned, mods) = {
            let mut guard = self.arc.internal.lock().unwrap();
            let transitioned = guard.release_source_keys(source);
            (transitioned, guard.mods_state)
        };
        let _ = mods;
        let serial = SERIAL_COUNTER.next_serial();
        let time = 0;
        for keycode in transitioned {
            // modifiers may have changed as a modifier key was released; let input_forward
            // re-derive and send the current state.
            self.input_forward(data, keycode, KeyState::Released, serial, time, true);
        }
    }

    /// Update the state of the keyboard without forwarding the event to the focused client
    ///
    /// Useful in conjunction with [`KeyboardHandle::input_forward`] in case you want
    /// to asynchronously decide if the event should be forwarded to the focused client.
    ///
    /// Prefer using [`KeyboardHandle::input`] if this decision can be done synchronously
    /// in the `filter` closure.
    pub fn input_intercept<T, F>(&self, data: &mut D, keycode: u32, state: KeyState, filter: F) -> (T, bool)
    where
        F: FnOnce(&mut D, &ModifiersState, KeyHandle<'_>) -> T,
    {
        trace!("Handling keystroke");

        let mut guard = self.arc.internal.lock().unwrap();
        let (mods_changed, leds_changed, _is_transition) =
            guard.key_input(KeyboardSource::MAIN, keycode, state);
        let led_state = guard.led_state;
        let mods_state = guard.mods_state;
        let wkb = guard.wkb.clone();
        std::mem::drop(guard);

        let key_handle = KeyHandle {
            wkb: &wkb,
            evdev_code: keycode,
        };

        trace!(mods_state = ?mods_state, sym = ?key_handle.evdev_code, "Calling input filter");
        let filter_result = filter(data, &mods_state, key_handle);

        if leds_changed {
            let seat = self.get_seat(data);
            data.led_state_changed(&seat, led_state);
        }

        (filter_result, mods_changed)
    }

    /// Forward a key event to the focused client
    ///
    /// Useful in conjunction with [`KeyboardHandle::input_intercept`].
    pub fn input_forward(
        &self,
        data: &mut D,
        keycode: u32,
        state: KeyState,
        serial: Serial,
        time: u32,
        mods_changed: bool,
    ) {
        let mut guard = self.arc.internal.lock().unwrap();
        match state {
            KeyState::Pressed => {
                guard.forwarded_pressed_keys.insert(keycode);
            }
            KeyState::Released => {
                guard.forwarded_pressed_keys.remove(&keycode);
            }
        };

        // forward to client if no keybinding is triggered.
        // Modifiers are only sent when the shared seat state actually changed; the client
        // resolves the following key event against them, so they must precede the key (handled
        // in `KeyboardInnerHandle::input`).
        let seat = self.get_seat(data);
        let mods = guard.mods_state;
        let modifiers = mods_changed.then_some(mods);
        guard.with_grab(data, &seat, |data, handle, grab| {
            grab.input(data, handle, keycode, state, modifiers, serial, time);
        });
        if guard.focus.is_some() {
            trace!("Input forwarded to client");
        } else {
            trace!("No client currently focused");
        }
    }

    /// Set the current focus of this keyboard
    ///
    /// If the new focus is different from the previous one, any previous focus
    /// will be sent a [`wl_keyboard::Event::Leave`](wayland_server::protocol::wl_keyboard::Event::Leave)
    /// event, and if the new focus is not `None`,
    /// a [`wl_keyboard::Event::Enter`](wayland_server::protocol::wl_keyboard::Event::Enter) event will be sent.
    #[instrument(level = "debug", parent = &self.arc.span, skip(self, data, focus), fields(focus = focus.is_some()))]
    pub fn set_focus(&self, data: &mut D, focus: Option<<D as SeatHandler>::KeyboardFocus>, serial: Serial) {
        let mut guard = self.arc.internal.lock().unwrap();
        guard.pending_focus.clone_from(&focus);
        let seat = self.get_seat(data);
        guard.with_grab(data, &seat, |data, handle, grab| {
            grab.set_focus(data, handle, focus, serial);
        });
    }

    /// Return the key codes of the currently pressed keys.
    pub fn pressed_keys(&self) -> HashSet<u32> {
        let guard = self.arc.internal.lock().unwrap();
        guard.pressed_keys.clone()
    }

    /// Iterate over the keysyms of the currently pressed keys.
    pub fn with_pressed_keysyms<F, R>(&self, f: F) -> R
    where
        F: FnOnce(Vec<KeyHandle<'_>>) -> R,
        R: 'static,
    {
        let guard = self.arc.internal.lock().unwrap();
        {
            let handles = guard
                .pressed_keys
                .iter()
                .map(|keycode| KeyHandle {
                    wkb: &guard.wkb,
                    evdev_code: *keycode,
                })
                .collect::<Vec<_>>();
            f(handles)
        }
    }

    /// Get the current modifiers state.
    pub fn modifier_state(&self) -> ModifiersState {
        self.arc.internal.lock().unwrap().mods_state
    }

    /// Set modifier state from serialized masks received from `wl_keyboard.modifiers`.
    ///
    /// The serialized depressed, latched, locked, and layout fields are authoritative.
    /// High-level fields in [`ModifiersState`] are rebuilt from WKB after the update.
    ///
    /// Returns whether modifier state changed.
    pub fn set_modifier_state(&self, serialized: SerializedMods) -> bool {
        let internal = &mut self.arc.internal.lock().unwrap();

        let (modifiers_changed, new_mods, new_leds) = {
            let wkb = &mut internal.wkb.lock().unwrap();
            let changes = wkb.update_modifiers(
                serialized.depressed,
                serialized.latched,
                serialized.locked,
                serialized.layout_effective,
            );
            let new_mods = if changes.modifiers_updated {
                let mut mods_state = ModifiersState::default();
                mods_state.update_with(wkb);
                Some(mods_state)
            } else {
                None
            };
            let new_leds = if changes.leds_updated {
                Some(led_state_from_wkb(wkb))
            } else {
                None
            };
            (changes.modifiers_updated, new_mods, new_leds)
        };
        if let Some(mods_state) = new_mods {
            internal.mods_state = mods_state;
        }
        if let Some(led_state) = new_leds {
            internal.led_state = led_state;
        }

        modifiers_changed
    }

    /// Advertises changed modifier state using [`KeyboardTarget::modifiers`].
    ///
    /// Use this with [`KeyboardHandle::set_modifier_state`] when necessary.
    pub fn advertise_modifier_state(&self, data: &mut D) {
        let internal = &mut *self.arc.internal.lock().unwrap();

        if let Some((focus, _)) = internal.focus.as_mut() {
            let seat = self.get_seat(data);
            focus.modifiers(&seat, data, internal.mods_state, SERIAL_COUNTER.next_serial());
        };
    }

    /// Get the current led state
    pub fn led_state(&self) -> LedState {
        self.arc.internal.lock().unwrap().led_state
    }

    /// Check if keyboard has focus
    pub fn is_focused(&self) -> bool {
        self.arc.internal.lock().unwrap().focus.is_some()
    }

    /// Change the repeat info configured for this keyboard
    #[instrument(parent = &self.arc.span, skip(self))]
    pub fn change_repeat_info(&self, rate: i32, delay: i32) {
        let mut guard = self.arc.internal.lock().unwrap();
        guard.repeat_delay = delay;
        guard.repeat_rate = rate;
        #[cfg(feature = "wayland_frontend")]
        for kbd in &*self.arc.known_kbds.lock().unwrap() {
            let Ok(kbd) = kbd.upgrade() else {
                continue;
            };
            if kbd.version() >= 4 {
                kbd.repeat_info(rate, delay);
            }
        }
    }

    /// Access the [`Serial`] of the last `keyboard_enter` event, if that focus is still active.
    ///
    /// In other words this will return `None` again, once a `keyboard_leave` occurred.
    #[cfg(feature = "wayland_frontend")]
    pub fn last_enter(&self) -> Option<Serial> {
        *self.arc.last_enter.lock().unwrap()
    }

    fn get_seat(&self, data: &mut D) -> Seat<D> {
        let seat_state = data.seat_state();
        seat_state
            .seats
            .iter()
            .find(|seat| seat.get_keyboard().map(|h| &h == self).unwrap_or(false))
            .cloned()
            .unwrap()
    }
}

#[cfg(feature = "wayland_frontend")]
impl<D> KeyboardHandle<D>
where
    D: SeatHandler + 'static,
    <D as SeatHandler>::KeyboardFocus: crate::wayland::seat::WaylandFocus,
{
    /// Inject a batch of keysyms as text into the currently focused client (KWin-style).
    ///
    /// This is a helper/fallback: a compositor should prefer delivering text through the
    /// text-input protocol (`zwp_text_input_v3::commit_string`) when a text-input client is
    /// focused, and fall back to this for clients that aren't (terminals, games, ...).
    ///
    /// Builds a throwaway keymap that binds each keysym to its own spare keycode at base level
    /// (so no modifiers are ever needed), hands that keymap to clients, taps each keycode on the
    /// focused client, then restores the seat keymap. The compositor's own seat xkb state is
    /// **never** touched, so shortcut handling and physical-keyboard state are unaffected.
    /// `modifiers(0)` accompanies the injection keymap so any modifier the seat currently holds
    /// (e.g. a physically-held Shift) doesn't alter the injected characters.
    pub fn inject_text_keysyms(&self, data: &mut D, keysyms: &[Keysym]) {
        const FIRST: u32 = 9;
        let mut keycode_decls = String::new();
        let mut symbol_decls = String::new();
        let mut codes = Vec::with_capacity(keysyms.len());
        for (i, keysym) in keysyms.iter().enumerate() {
            let xkb_code = FIRST + i as u32;
            let evdev_code = xkb_code - 8;
            if xkb_code > 255 {
                break;
            }
            let name = xkb::keysym_get_name(*keysym);
            if name.is_empty() || name == "NoSymbol" {
                continue;
            }
            keycode_decls.push_str(&format!("    <K{xkb_code}> = {xkb_code};\n"));
            symbol_decls.push_str(&format!("    key <K{xkb_code}> {{ [ {name} ] }};\n"));
            codes.push(evdev_code);
        }
        if codes.is_empty() {
            return;
        }

        let custom = format!(
            "xkb_keymap {{\n\
             xkb_keycodes \"custom\" {{\n    minimum = 8;\n    maximum = 255;\n{keycode_decls}}};\n\
             xkb_types \"(custom)\" {{ include \"complete\" }};\n\
             xkb_compatibility \"custom\" {{ include \"complete\" }};\n\
             xkb_symbols \"custom\" {{\n{symbol_decls}}};\n\
             }};\n"
        );
        let wkb = match WKB::new_from_string(&custom) {
            Ok(wkb) => Mutex::new(wkb),
            Err(_) => return,
        };
        let injection_keymap = KeymapFile::new(custom);

        let seat = self.get_seat(data);
        let mut guard = self.arc.internal.lock().unwrap();
        if guard.focus.is_none() {
            return;
        }
        let seat_mods = guard.mods_state;

        // Switch the focused client to the injection keymap (with no modifiers), then tap keys.
        {
            let focus = guard.focus.as_mut().map(|(focus, _)| focus);
            self.send_keymap(data, &focus, &injection_keymap, ModifiersState::default());
        }
        for keycode in codes {
            let press = SERIAL_COUNTER.next_serial();
            let release = SERIAL_COUNTER.next_serial();
            if let Some((focus, _)) = guard.focus.as_mut() {
                let handle = KeyHandle {
                    wkb: &wkb,
                    evdev_code: keycode,
                };
                focus.key(&seat, data, handle, KeyState::Pressed, press, 0);
            }
            if let Some((focus, _)) = guard.focus.as_mut() {
                let handle = KeyHandle {
                    wkb: &wkb,
                    evdev_code: keycode,
                };
                focus.key(&seat, data, handle, KeyState::Released, release, 0);
            }
        }

        // Restore the seat keymap (and the seat's real modifier state) to the client.
        let seat_keymap = self.arc.keymap.lock().unwrap();
        let focus = guard.focus.as_mut().map(|(focus, _)| focus);
        self.send_keymap(data, &focus, &seat_keymap, seat_mods);
    }
}

impl<D> KeyboardHandle<D>
where
    D: SeatHandler,
    <D as SeatHandler>::KeyboardFocus: Clone,
{
    /// Retrieve the current keyboard focus
    pub fn current_focus(&self) -> Option<<D as SeatHandler>::KeyboardFocus> {
        self.arc
            .internal
            .lock()
            .unwrap()
            .focus
            .clone()
            .map(|(focus, _)| focus)
    }
}

/// This inner handle is accessed from inside a keyboard grab logic, and directly
/// sends event to the client
pub struct KeyboardInnerHandle<'a, D: SeatHandler> {
    inner: &'a mut KbdInternal<D>,
    seat: &'a Seat<D>,
}

impl<D: SeatHandler> fmt::Debug for KeyboardInnerHandle<'_, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyboardInnerHandle")
            .field("inner", &self.inner)
            .field("seat", &self.seat.arc.name)
            .finish()
    }
}

impl<D: SeatHandler + 'static> KeyboardInnerHandle<'_, D> {
    /// Change the current grab on this keyboard to the provided grab
    ///
    /// Overwrites any current grab.
    pub fn set_grab<G: KeyboardGrab<D> + 'static>(
        &mut self,
        handler: &mut dyn KeyboardGrab<D>,
        data: &mut D,
        serial: Serial,
        grab: G,
    ) {
        handler.unset(data);
        self.inner.grab = GrabStatus::Active(serial, Box::new(grab));
    }

    /// Remove any current grab on this keyboard, resetting it to the default behavior
    ///
    /// This will also restore the focus of the underlying keyboard if restore_focus
    /// is [`true`]
    pub fn unset_grab(
        &mut self,
        handler: &mut dyn KeyboardGrab<D>,
        data: &mut D,
        serial: Serial,
        restore_focus: bool,
    ) {
        handler.unset(data);
        self.inner.grab = GrabStatus::None;
        // restore the focus
        if restore_focus {
            let focus = self.inner.pending_focus.clone();
            self.set_focus(data, focus, serial);
        }
    }

    /// Access the current focus of this keyboard
    pub fn current_focus(&self) -> Option<&<D as SeatHandler>::KeyboardFocus> {
        self.inner.focus.as_ref().map(|f| &f.0)
    }

    /// Convert a given evdev keycode to a [`KeyHandle`] using this keyboard's WKB state.
    pub fn key_handle(&self, keycode: u32) -> KeyHandle<'_> {
        KeyHandle {
            evdev_code: keycode,
            wkb: &self.inner.wkb,
        }
    }

    /// Get the current modifiers state
    pub fn modifier_state(&self) -> ModifiersState {
        self.inner.mods_state
    }

    /// Send the input to the focused keyboards
    pub fn input(
        &mut self,
        data: &mut D,
        keycode: u32,
        key_state: KeyState,
        modifiers: Option<ModifiersState>,
        serial: Serial,
        time: u32,
    ) {
        let (focus, _) = match self.inner.focus.as_mut() {
            Some(focus) => focus,
            None => return,
        };

        #[cfg(feature = "wayland_frontend")]
        if let Some(keyboard_handle) = self.seat.get_keyboard() {
            let keymap_file = keyboard_handle.arc.keymap.lock().unwrap();
            let mods = self.inner.mods_state;
            keyboard_handle.send_keymap(data, &Some(focus), &keymap_file, mods);
        }

        let key = KeyHandle {
            wkb: &self.inner.wkb,
            evdev_code: keycode,
        };

        // Modifiers must be sent before the key event so the client resolves the key against the
        // updated modifier state.
        if let Some(mods) = modifiers {
            focus.modifiers(self.seat, data, mods, serial);
        }
        focus.key(self.seat, data, key, key_state, serial, time);
    }

    /// Iterate over the currently pressed keys.
    pub fn with_pressed_keysyms<F, R>(&self, f: F) -> R
    where
        F: FnOnce(Vec<KeyHandle<'_>>) -> R,
        R: 'static,
    {
        let handles = self
            .inner
            .pressed_keys
            .iter()
            .map(|code| self.key_handle(*code))
            .collect();
        f(handles)
    }

    /// The forwarded held keys and modifier state to announce to a newly-focused target on a
    /// focus change, from the seat's shared state.
    fn focus_enter_state(&self) -> (Vec<KeyHandle<'_>>, ModifiersState) {
        (
            self.inner
                .forwarded_pressed_keys
                .iter()
                .map(|keycode| KeyHandle {
                    wkb: &self.inner.wkb,
                    evdev_code: *keycode,
                })
                .collect(),
            self.inner.mods_state,
        )
    }

    /// Set the current focus of this keyboard
    ///
    /// If the new focus is different from the previous one, any previous focus
    /// will be sent a [`wl_keyboard::Event::Leave`](wayland_server::protocol::wl_keyboard::Event::Leave)
    /// event, and if the new focus is not `None`,
    /// a [`wl_keyboard::Event::Enter`](wayland_server::protocol::wl_keyboard::Event::Enter) event will be sent.
    pub fn set_focus(
        &mut self,
        data: &mut D,
        focus: Option<<D as SeatHandler>::KeyboardFocus>,
        serial: Serial,
    ) {
        if let Some(focus) = focus {
            let old_focus = self.inner.focus.replace((focus.clone(), serial));
            match (focus, old_focus) {
                (focus, Some((old_focus, _))) if focus == old_focus => {
                    trace!("Focus unchanged");
                }
                (focus, Some((old_focus, _))) => {
                    trace!("Focus set to new surface");
                    let (keys, mods) = self.focus_enter_state();

                    focus.replace(old_focus, self.seat, data, keys, mods, serial);
                    data.focus_changed(self.seat, Some(&focus));
                }
                (focus, None) => {
                    let (keys, mods) = self.focus_enter_state();

                    focus.enter(self.seat, data, keys, serial);
                    focus.modifiers(self.seat, data, mods, serial);
                    data.focus_changed(self.seat, Some(&focus));
                }
            }
        } else if let Some((old_focus, _)) = self.inner.focus.take() {
            trace!("Focus unset");
            old_focus.leave(self.seat, data, serial);
        }
    }
}

// The default grab, the behavior when no particular grab is in progress
struct DefaultGrab;

impl<D: SeatHandler + 'static> KeyboardGrab<D> for DefaultGrab {
    fn input(
        &mut self,
        data: &mut D,
        handle: &mut KeyboardInnerHandle<'_, D>,
        keycode: u32,
        state: KeyState,
        modifiers: Option<ModifiersState>,
        serial: Serial,
        time: u32,
    ) {
        handle.input(data, keycode, state, modifiers, serial, time)
    }

    fn set_focus(
        &mut self,
        data: &mut D,
        handle: &mut KeyboardInnerHandle<'_, D>,
        focus: Option<<D as SeatHandler>::KeyboardFocus>,
        serial: Serial,
    ) {
        handle.set_focus(data, focus, serial)
    }

    fn start_data(&self) -> &GrabStartData<D> {
        unreachable!()
    }

    fn unset(&mut self, _data: &mut D) {}
}
