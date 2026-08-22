use wkb::WKB;
use xkbcommon::xkb;

/// Represents the current state of the keyboard modifiers
///
/// Each field of this struct represents a modifier and is `true` if this modifier is active.
///
/// For some modifiers, this means that the key is currently pressed, others are toggled/locked
/// (like caps lock).
///
/// **Note:** The XKB state should usually be the single source of truth, and the
/// serialization is lossy and will not survive round trips. This is documented in
/// [`xkb::State::update_mask`].
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
    /// Note that this may have outdated information compared to the other fields, and that
    /// this is not updated in [`ModifiersState::serialize_back`].
    pub serialized: SerializedMods,
}

impl ModifiersState {
    /// Updates the high-level modifiers state from an XKB state.
    ///
    /// **Note:** The XKB state should usually be the single source of truth, and the
    /// serialization is lossy and will not survive round trips. This is documented in
    /// [`xkb::State::update_mask`].
    pub fn update_with(&mut self, wkb: &WKB) {
        self.ctrl = wkb.ctrl();
        self.alt = wkb.alt();
        self.shift = wkb.shift();
        self.caps_lock = wkb.caps_lock();
        self.logo = wkb.logo();
        self.num_lock = wkb.num_lock();
        self.iso_level3_shift = wkb.mod3();
        self.iso_level5_shift = wkb.mod5();
        self.serialized = serialize_modifiers(wkb);
    }

    /// Serializes the high-level modifiers state to be sent to XKB e.g. in
    /// `wl_keyboard.modifiers`.
    ///
    /// **Note:** The XKB state should usually be the single source of truth, and the
    /// serialization is lossy and will not survive round trips. This is documented in
    /// [`xkb::State::update_mask`].
    ///
    /// Note that cached serialized state is stored in [`ModifiersState::serialized`], but it may
    /// have outdated information. This function ignores that field. You should update the cached
    /// serialized state after using this function, like so:
    ///
    /// ```no_run
    /// use smithay::input::keyboard::ModifiersState;
    ///
    /// let mut mods_state: ModifiersState;
    /// # mods_state = todo!();
    /// # let xkb_state = todo!();
    ///
    /// // Update the information
    /// mods_state.ctrl = true;
    ///
    /// // Serialize e.g. for sending in `wl_keyboard.modifiers`
    /// let serialized = mods_state.serialize_back(&xkb_state);
    ///
    /// // Update the cached serialized state
    /// mods_state.serialized = serialized;
    /// ```
    pub fn serialize_back(&self, state: &xkb::State) -> SerializedMods {
        let keymap = state.get_keymap();

        let mut locked: u32 = 0;
        let mut depressed: u32 = 0;

        if self.caps_lock {
            let index = keymap.mod_get_index(&xkb::MOD_NAME_CAPS);
            if index != xkb::MOD_INVALID {
                locked |= 1 << index;
            }
        }
        if self.num_lock {
            let index = keymap.mod_get_index(&xkb::MOD_NAME_NUM);
            if index != xkb::MOD_INVALID {
                locked |= 1 << index;
            }
        }
        if self.ctrl {
            let index = keymap.mod_get_index(&xkb::MOD_NAME_CTRL);
            if index != xkb::MOD_INVALID {
                depressed |= 1 << index;
            }
        }
        if self.alt {
            let index = keymap.mod_get_index(&xkb::MOD_NAME_ALT);
            if index != xkb::MOD_INVALID {
                depressed |= 1 << index;
            }
        }
        if self.shift {
            let index = keymap.mod_get_index(&xkb::MOD_NAME_SHIFT);
            if index != xkb::MOD_INVALID {
                depressed |= 1 << index;
            }
        }
        if self.logo {
            let index = keymap.mod_get_index(&xkb::MOD_NAME_LOGO);
            if index != xkb::MOD_INVALID {
                depressed |= 1 << index;
            }
        }
        if self.iso_level3_shift {
            let index = keymap.mod_get_index(&xkb::MOD_NAME_ISO_LEVEL3_SHIFT);
            if index != xkb::MOD_INVALID {
                depressed |= 1 << index;
            }
        }
        if self.iso_level5_shift {
            let index = keymap.mod_get_index(&xkb::MOD_NAME_MOD3);
            if index != xkb::MOD_INVALID {
                depressed |= 1 << index;
            }
        }

        let layout_effective = state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE);

        SerializedMods {
            depressed,
            latched: 0,
            locked,
            layout_effective,
        }
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
    let depressed = raw_mods.depressed;
    let latched = raw_mods.latched;
    let locked = raw_mods.locked;
    let layout_effective = raw_mods.layout;
    SerializedMods {
        depressed,
        latched,
        locked,
        layout_effective,
    }
}
