use wayland_server::protocol::wl_surface::WlSurface;

use crate::utils::{IsAlive, Logical, Point, Rectangle};

use super::{v2, v3};

/// Input-method popup from either protocol version.
#[derive(Debug, Clone, PartialEq)]
pub enum InputMethodPopup {
    /// input-method v2 [`PopupSurface`](v2::PopupSurface)
    V2(v2::PopupSurface),
    /// input-method v3 [`PopupSurface`](v3::PopupSurface)
    V3(v3::PopupSurface),
}

impl IsAlive for InputMethodPopup {
    #[inline]
    fn alive(&self) -> bool {
        match self {
            InputMethodPopup::V2(popup) => popup.alive(),
            InputMethodPopup::V3(popup) => popup.alive(),
        }
    }
}

impl InputMethodPopup {
    #[inline]
    pub fn wl_surface(&self) -> &WlSurface {
        match self {
            InputMethodPopup::V2(popup) => popup.wl_surface(),
            InputMethodPopup::V3(popup) => popup.wl_surface(),
        }
    }

    pub(crate) fn parent(&self) -> Option<WlSurface> {
        match self {
            InputMethodPopup::V2(popup) => popup.get_parent().map(|parent| parent.surface.clone()),
            InputMethodPopup::V3(popup) => Some(popup.get_parent().surface.clone()),
        }
    }

    pub(crate) fn geometry(&self) -> Rectangle<i32, Logical> {
        match self {
            InputMethodPopup::V2(popup) => popup
                .get_parent()
                .map(|parent| parent.location)
                .unwrap_or_default(),
            InputMethodPopup::V3(popup) => popup.get_parent().location,
        }
    }

    pub(crate) fn location(&self) -> Point<i32, Logical> {
        match self {
            InputMethodPopup::V2(popup) => popup.location(),
            InputMethodPopup::V3(popup) => popup.location(),
        }
    }
}

impl From<v2::PopupSurface> for InputMethodPopup {
    fn from(popup: v2::PopupSurface) -> Self {
        InputMethodPopup::V2(popup)
    }
}

impl From<v3::PopupSurface> for InputMethodPopup {
    fn from(popup: v3::PopupSurface) -> Self {
        InputMethodPopup::V3(popup)
    }
}
