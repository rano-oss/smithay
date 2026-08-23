/// Configuration for loading a keyboard layout through WKB.
///
/// For fields left empty (`""` or `None`, as in the [`Default`] impl), WKB uses its
/// built-in defaults when resolving the keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct XkbConfig<'a> {
    /// Rules to compose the keymap from
    pub rules: &'a str,
    /// Model to compose the keymap from
    pub model: &'a str,
    /// Layout to compose the keymap from
    pub layout: &'a str,
    /// Variant to compose the keymap from
    pub variant: &'a str,
    /// Options to compose the keymap from
    pub options: Option<&'a str>,
}

impl Default for XkbConfig<'_> {
    fn default() -> Self {
        XkbConfig {
            rules: "evdev",
            model: "pc105",
            layout: "us",
            variant: "",
            options: None,
        }
    }
}
