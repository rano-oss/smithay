use wayland_server::protocol::wl_surface::WlSurface;

use crate::utils::{IsAlive, Logical, Point, Rectangle};

use super::v2::PopupParent;
use super::{v2, v3};

/// Input-method popup surface from either protocol version.
#[derive(Debug, Clone, PartialEq)]
pub enum PopupSurface {
    /// input-method v2 popup surface
    V2(v2::PopupSurface),
    /// input-method v3 popup surface
    V3(v3::PopupSurface),
}

impl IsAlive for PopupSurface {
    #[inline]
    fn alive(&self) -> bool {
        match self {
            PopupSurface::V2(popup) => popup.alive(),
            PopupSurface::V3(popup) => popup.alive(),
        }
    }
}

impl PopupSurface {
    #[inline]
    /// Access to the underlying wl_surface of this popup
    pub fn wl_surface(&self) -> &WlSurface {
        match self {
            PopupSurface::V2(popup) => popup.wl_surface(),
            PopupSurface::V3(popup) => popup.wl_surface(),
        }
    }

    /// Access to the parent surface associated with this popup.
    pub fn get_parent(&self) -> Option<PopupParent> {
        match self {
            PopupSurface::V2(popup) => popup.get_parent().cloned(),
            PopupSurface::V3(popup) => {
                let parent = popup.get_parent();
                Some(PopupParent {
                    surface: parent.surface.clone(),
                    location: parent.location,
                })
            }
        }
    }

    pub(crate) fn parent(&self) -> Option<WlSurface> {
        self.get_parent().map(|parent| parent.surface)
    }

    pub(crate) fn geometry(&self) -> Rectangle<i32, Logical> {
        match self {
            PopupSurface::V2(popup) => popup
                .get_parent()
                .map(|parent| parent.location)
                .unwrap_or_default(),
            PopupSurface::V3(popup) => popup.get_parent().location,
        }
    }

    pub(crate) fn location(&self) -> Point<i32, Logical> {
        match self {
            PopupSurface::V2(popup) => popup.location(),
            PopupSurface::V3(popup) => popup.location(),
        }
    }
}

impl From<v2::PopupSurface> for PopupSurface {
    fn from(popup: v2::PopupSurface) -> Self {
        PopupSurface::V2(popup)
    }
}

impl From<v3::PopupSurface> for PopupSurface {
    fn from(popup: v3::PopupSurface) -> Self {
        PopupSurface::V3(popup)
    }
}
