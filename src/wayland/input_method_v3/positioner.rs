use crate::utils::{Logical, Point, Rectangle, Size};
use crate::wayland::Dispatch2;
use std::cmp::min;
use std::sync::Mutex;
use wayland_protocols_experimental::input_method::v1::server::xx_input_popup_positioner_v1::{
    self, Anchor, ConstraintAdjustment, Gravity, XxInputPopupPositionerV1,
};
use wayland_server::{Resource, WEnum};

/// User data for the positioner protocol object.
#[derive(Default, Debug)]
pub struct PositionerUserData {
    pub(crate) inner: Mutex<PositionerState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// The state of a positioner, as set by the client
pub struct PositionerState {
    /// Requested size of the rectangle to position.
    ///
    /// This is treated as the preferred size to aim for, even if it can't always be reached (e.g. due to output too small).
    pub rect_size: Size<u32, Logical>,
    /// Edges defining the anchor point
    pub anchor_edges: Anchor,
    /// Gravity direction for positioning the child surface
    /// relative to its anchor point
    pub gravity: Gravity,
    /// Adjustments to do if previous criteria constrain the
    /// surface
    pub constraint_adjustment: ConstraintAdjustment,
    /// Offset placement relative to the anchor point
    pub offset: Point<i32, Logical>,
}

impl Default for PositionerState {
    fn default() -> Self {
        PositionerState {
            anchor_edges: Anchor::None,
            constraint_adjustment: ConstraintAdjustment::empty(),
            gravity: Gravity::None,
            offset: Default::default(),
            rect_size: Default::default(),
        }
    }
}

impl PositionerState {
    pub(crate) fn anchor_has_edge(&self, edge: Anchor) -> bool {
        match edge {
            Anchor::Top => matches!(
                self.anchor_edges,
                Anchor::Top | Anchor::TopLeft | Anchor::TopRight
            ),
            Anchor::Bottom => matches!(
                self.anchor_edges,
                Anchor::Bottom | Anchor::BottomLeft | Anchor::BottomRight
            ),
            Anchor::Left => matches!(
                self.anchor_edges,
                Anchor::Left | Anchor::TopLeft | Anchor::BottomLeft
            ),
            Anchor::Right => matches!(
                self.anchor_edges,
                Anchor::Right | Anchor::TopRight | Anchor::BottomRight
            ),
            _ => unreachable!(),
        }
    }

    /// Get the anchor point for a popup as defined by this positioner.
    pub fn get_anchor_point(&self, anchor_rect: Rectangle<i32, Logical>) -> Point<i32, Logical> {
        let y = anchor_rect.loc.y
            + if self.anchor_has_edge(Anchor::Top) {
                0
            } else if self.anchor_has_edge(Anchor::Bottom) {
                anchor_rect.size.h
            } else {
                anchor_rect.size.h / 2
            };

        let x = anchor_rect.loc.x
            + if self.anchor_has_edge(Anchor::Left) {
                0
            } else if self.anchor_has_edge(Anchor::Right) {
                anchor_rect.size.w
            } else {
                anchor_rect.size.w / 2
            };

        (x, y).into()
    }

    pub(crate) fn gravity_has_edge(&self, edge: Gravity) -> bool {
        match edge {
            Gravity::Top => matches!(self.gravity, Gravity::Top | Gravity::TopLeft | Gravity::TopRight),
            Gravity::Bottom => matches!(
                self.gravity,
                Gravity::Bottom | Gravity::BottomLeft | Gravity::BottomRight
            ),
            Gravity::Left => matches!(
                self.gravity,
                Gravity::Left | Gravity::TopLeft | Gravity::BottomLeft
            ),
            Gravity::Right => matches!(
                self.gravity,
                Gravity::Right | Gravity::TopRight | Gravity::BottomRight
            ),
            _ => unreachable!(),
        }
    }

    /// Get initial popup geometry from anchor_rect, before constraint adjustment.
    fn get_geometry(&self, anchor_rect: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
        let mut loc = self.offset;
        let size = self.rect_size;

        loc += self.get_anchor_point(anchor_rect);

        loc.y = if self.gravity_has_edge(Gravity::Top) {
            loc.y.saturating_sub_unsigned(size.h)
        } else if !self.gravity_has_edge(Gravity::Bottom) {
            loc.y.saturating_sub_unsigned(size.h / 2)
        } else {
            loc.y
        };

        loc.x = if self.gravity_has_edge(Gravity::Left) {
            loc.x.saturating_sub_unsigned(size.w)
        } else if !self.gravity_has_edge(Gravity::Right) {
            loc.x.saturating_sub_unsigned(size.w / 2)
        } else {
            loc.x
        };

        let size = (
            0i32.saturating_add_unsigned(self.rect_size.w),
            0i32.saturating_add_unsigned(self.rect_size.h),
        )
            .into();

        Rectangle { loc, size }
    }

    /// Get popup geometry after applying constraint adjustments to fit within `target`.
    pub fn get_unconstrained_geometry(
        mut self,
        anchor_rect: Rectangle<i32, Logical>,
        target: Rectangle<i32, Logical>,
    ) -> Rectangle<i32, Logical> {
        // Adjustment order: flip, slide, resize. Flips are reverted if they don't help.
        let mut geo = self.get_geometry(anchor_rect);
        let (mut off_left, mut off_right, mut off_top, mut off_bottom) = compute_offsets(target, geo);

        // Try to flip horizontally.
        if (off_left > 0 || off_right > 0) && self.constraint_adjustment.contains(ConstraintAdjustment::FlipX)
        {
            let mut new = self;
            new.anchor_edges = invert_anchor_x(new.anchor_edges);
            new.gravity = invert_gravity_x(new.gravity);
            let new_geo = new.get_geometry(anchor_rect);
            let (new_off_left, new_off_right, _, _) = compute_offsets(target, new_geo);

            if new_off_left <= 0 && new_off_right <= 0 {
                self = new;
                geo = new_geo;
                off_left = 0;
                off_right = 0;
            }
        }

        // Try to flip vertically.
        if (off_top > 0 || off_bottom > 0) && self.constraint_adjustment.contains(ConstraintAdjustment::FlipY)
        {
            let mut new = self;
            new.anchor_edges = invert_anchor_y(new.anchor_edges);
            new.gravity = invert_gravity_y(new.gravity);
            let new_geo = new.get_geometry(anchor_rect);
            let (_, _, new_off_top, new_off_bottom) = compute_offsets(target, new_geo);

            if new_off_top <= 0 && new_off_bottom <= 0 {
                self = new;
                geo = new_geo;
                off_top = 0;
                off_bottom = 0;
            }
        }

        // Try to slide horizontally.
        if (off_left > 0 || off_right > 0)
            && self.constraint_adjustment.contains(ConstraintAdjustment::SlideX)
        {
            if off_left > 0 {
                geo.loc.x += off_left;
            } else if off_right > 0 {
                geo.loc.x -= min(off_right, -off_left);
            }
            (_, off_right, _, _) = compute_offsets(target, geo);
        }

        // Try to slide vertically.
        if (off_top > 0 || off_bottom > 0)
            && self.constraint_adjustment.contains(ConstraintAdjustment::SlideY)
        {
            if off_top > 0 {
                geo.loc.y += off_top;
            } else if off_bottom > 0 {
                geo.loc.y -= min(off_bottom, -off_top);
            }
            (_, _, _, off_bottom) = compute_offsets(target, geo);
        }

        // Try to resize horizontally.
        if off_right > 0
            && off_right < geo.size.w
            && self.constraint_adjustment.contains(ConstraintAdjustment::ResizeX)
        {
            geo.size.w -= off_right;
        }

        // Try to resize vertically.
        if off_bottom > 0
            && off_bottom < geo.size.h
            && self.constraint_adjustment.contains(ConstraintAdjustment::ResizeY)
        {
            geo.size.h -= off_bottom;
        }

        geo
    }
}

impl<D> Dispatch2<XxInputPopupPositionerV1, D> for PositionerUserData {
    fn request(
        &self,
        _state: &mut D,
        _client: &wayland_server::Client,
        positioner: &XxInputPopupPositionerV1,
        request: xx_input_popup_positioner_v1::Request,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, D>,
    ) {
        let mut state = self.inner.lock().unwrap();
        use xx_input_popup_positioner_v1::Request;
        match request {
            Request::SetSize { width, height } => {
                if width < 1 || height < 1 {
                    positioner.post_error(
                        xx_input_popup_positioner_v1::Error::InvalidInput,
                        "Invalid size for positioner.",
                    );
                } else {
                    state.rect_size = (width, height).into();
                }
            }
            Request::SetAnchor { anchor } => {
                if let WEnum::Value(anchor) = anchor {
                    state.anchor_edges = anchor;
                }
            }
            Request::SetGravity { gravity } => {
                if let WEnum::Value(gravity) = gravity {
                    state.gravity = gravity;
                }
            }
            Request::SetConstraintAdjustment {
                constraint_adjustment,
            } => {
                if let WEnum::Value(constraint_adjustment) = constraint_adjustment {
                    state.constraint_adjustment = constraint_adjustment;
                }
            }
            Request::SetOffset { x, y } => {
                state.offset = (x, y).into();
            }
            Request::SetReactive => {} // IM popups positioned by cursor_rectangle, not parent movement
            Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

fn compute_offsets(target: Rectangle<i32, Logical>, popup: Rectangle<i32, Logical>) -> (i32, i32, i32, i32) {
    let off_left = target.loc.x - popup.loc.x;
    let off_right = (popup.loc.x + popup.size.w) - (target.loc.x + target.size.w);
    let off_top = target.loc.y - popup.loc.y;
    let off_bottom = (popup.loc.y + popup.size.h) - (target.loc.y + target.size.h);
    (off_left, off_right, off_top, off_bottom)
}

fn invert_anchor_x(anchor: Anchor) -> Anchor {
    match anchor {
        Anchor::Left => Anchor::Right,
        Anchor::Right => Anchor::Left,
        Anchor::TopLeft => Anchor::TopRight,
        Anchor::TopRight => Anchor::TopLeft,
        Anchor::BottomLeft => Anchor::BottomRight,
        Anchor::BottomRight => Anchor::BottomLeft,
        x => x,
    }
}

fn invert_anchor_y(anchor: Anchor) -> Anchor {
    match anchor {
        Anchor::Top => Anchor::Bottom,
        Anchor::Bottom => Anchor::Top,
        Anchor::TopLeft => Anchor::BottomLeft,
        Anchor::TopRight => Anchor::BottomRight,
        Anchor::BottomLeft => Anchor::TopLeft,
        Anchor::BottomRight => Anchor::TopRight,
        x => x,
    }
}

fn invert_gravity_x(gravity: Gravity) -> Gravity {
    match gravity {
        Gravity::Left => Gravity::Right,
        Gravity::Right => Gravity::Left,
        Gravity::TopLeft => Gravity::TopRight,
        Gravity::TopRight => Gravity::TopLeft,
        Gravity::BottomLeft => Gravity::BottomRight,
        Gravity::BottomRight => Gravity::BottomLeft,
        x => x,
    }
}

fn invert_gravity_y(gravity: Gravity) -> Gravity {
    match gravity {
        Gravity::Top => Gravity::Bottom,
        Gravity::Bottom => Gravity::Top,
        Gravity::TopLeft => Gravity::BottomLeft,
        Gravity::TopRight => Gravity::BottomRight,
        Gravity::BottomLeft => Gravity::TopLeft,
        Gravity::BottomRight => Gravity::TopRight,
        x => x,
    }
}
