use wayland_server::Client;

/// Data associated with an input method manager global.
#[allow(missing_debug_implementations)]
pub struct InputMethodManagerGlobalData {
    pub(crate) filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

impl InputMethodManagerGlobalData {
    pub(crate) fn new<F>(filter: F) -> Self
    where
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        Self {
            filter: Box::new(filter),
        }
    }
}
