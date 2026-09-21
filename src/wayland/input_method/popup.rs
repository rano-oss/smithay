use wayland_server::protocol::wl_surface::WlSurface;

use crate::utils::{IsAlive, Logical, Point, Rectangle};

use super::{v2, v3};
use v3::PositionerState;

/// Parent surface and location for an input method popup.
#[derive(Debug, Clone)]
pub struct PopupParent {
    /// The surface over which the IME popup is shown.
    pub surface: WlSurface,
    /// The location of the parent surface in compositor space.
    pub location: Rectangle<i32, Logical>,
}

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
            PopupSurface::V3(popup) => Some(popup.get_parent().clone()),
        }
    }

    pub(crate) fn parent(&self) -> Option<WlSurface> {
        self.get_parent().map(|parent| parent.surface)
    }

    /// Geometry hook used by generic popup rendering.
    ///
    /// Returns the parent location rectangle so compositors can use
    /// `window_loc + popup_offset - geometry().loc` for both v2 and v3.
    pub(crate) fn geometry(&self) -> Rectangle<i32, Logical> {
        self.get_parent()
            .map(|parent| parent.location)
            .unwrap_or_default()
    }

    /// Location of the popup relative to its parent surface
    pub fn location(&self) -> Point<i32, Logical> {
        match self {
            PopupSurface::V2(popup) => popup.location(),
            PopupSurface::V3(popup) => popup.location(),
        }
    }

    /// Anchor rectangle relative to the parent surface.
    pub fn anchor_rectangle(&self) -> Rectangle<i32, Logical> {
        match self {
            PopupSurface::V2(popup) => popup.text_input_rectangle(),
            PopupSurface::V3(popup) => popup.anchor_rectangle(),
        }
    }

    /// Set popup location relative to the parent (v2 only; no-op for v3).
    pub fn set_location(&self, location: Point<i32, Logical>) {
        match self {
            PopupSurface::V2(popup) => popup.set_location(location),
            PopupSurface::V3(_) => {}
        }
    }

    /// Positioner state for this popup (v3 only; v2 returns default).
    pub fn positioner(&self) -> PositionerState {
        match self {
            PopupSurface::V2(_) => PositionerState::default(),
            PopupSurface::V3(popup) => popup.positioner(),
        }
    }

    /// Whether repositioning this popup requires a configure flush via `InputMethodHandle::done`.
    pub fn repositions_via_configure(&self) -> bool {
        matches!(self, PopupSurface::V3(_))
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
