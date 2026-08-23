use wkb::WKB;

/// Represents the current state of the keyboard modifiers
///
/// Each field of this struct represents a modifier and is `true` if this modifier is active.
///
/// For some modifiers, this means that the key is currently pressed, others are toggled/locked
/// (like caps lock).
///
/// **Note:** The WKB state should usually be the single source of truth. The serialized
/// modifier masks in [`SerializedMods`] are authoritative when feeding state back into WKB;
/// high-level fields are always rebuilt from WKB after an update.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ModifiersState {
    /// The "control" key
    pub ctrl: bool,
    /// The "alt" key
    pub alt: bool,
    /// The "shift" key
    pub shift: bool,
    /// The "Caps lock" key
    pub caps_lock: bool,
    /// The "logo" key
    ///
    /// Also known as the "windows" key on most keyboards
    pub logo: bool,
    /// The "Num lock" key
    pub num_lock: bool,
    /// The "ISO level 3 shift" key
    ///
    /// Also known as the "AltGr" key
    pub iso_level3_shift: bool,

    /// The "ISO level 5 shift" key
    pub iso_level5_shift: bool,

    /// Cached serialized modifier state, e.g. for sending in `wl_keyboard.modifiers`.
    ///
    /// This is populated by [`ModifiersState::update_with`] from WKB and should be treated as
    /// authoritative for wire protocol serialization.
    pub serialized: SerializedMods,
}

impl ModifiersState {
    /// Updates the high-level modifiers state from a WKB instance.
    ///
    /// ISO Level3 maps to WKB Level3 (XKB Mod5 / AltGr). ISO Level5 maps to WKB Level5
    /// (XKB Mod3). Prefer [`wkb::WKB::level3`] and [`wkb::WKB::level5`] when available.
    pub fn update_with(&mut self, wkb: &WKB) {
        self.ctrl = wkb.ctrl();
        self.alt = wkb.alt();
        self.shift = wkb.shift();
        self.caps_lock = wkb.caps_lock();
        self.logo = wkb.logo();
        self.num_lock = wkb.num_lock();
        self.iso_level3_shift = wkb.level3();
        self.iso_level5_shift = wkb.level5();
        self.serialized = serialize_modifiers(wkb);
    }
}

/// Serialized modifier state
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct SerializedMods {
    /// Depressed modifiers
    pub depressed: u32,
    /// Latched modifiers
    pub latched: u32,
    /// Locked modifiers
    pub locked: u32,
    /// Effective keyboard layout
    pub layout_effective: u32,
}

fn serialize_modifiers(wkb: &WKB) -> SerializedMods {
    let raw_mods = wkb.raw_modifiers();
    SerializedMods {
        depressed: raw_mods.depressed,
        latched: raw_mods.latched,
        locked: raw_mods.locked,
        layout_effective: raw_mods.layout,
    }
}
