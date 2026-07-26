//! Where the screen-layout editor puts things.
//!
//! Pure geometry, deliberately free of any drawing: these are the functions
//! the layout tests exercise directly.

use std::collections::HashMap;

use eframe::egui::{Pos2, Rect, Vec2};
use lan_mouse_ipc::{ClientHandle, Position};

use crate::app::dialogs::PendingPosition;
use crate::model::UiState;

pub(crate) const SCREEN_LAYOUT_DESIGN_WIDTH: f32 = 574.0;
pub(crate) const SLOT_DROP_TOLERANCE: f32 = 12.0;

pub(crate) fn screen_layout_geometry(canvas: Rect) -> (Rect, [(Position, Rect); 4]) {
    let scale = (canvas.width() / SCREEN_LAYOUT_DESIGN_WIDTH).min(1.0);
    let center = Rect::from_center_size(canvas.center(), Vec2::new(190.0, 104.0) * scale);
    (center, screen_slots(center, scale))
}

pub(crate) fn screen_slots(center: Rect, scale: f32) -> [(Position, Rect); 4] {
    [
        (
            Position::Left,
            Rect::from_center_size(
                Pos2::new(center.left() - 108.0 * scale, center.center().y),
                Vec2::new(168.0, 92.0) * scale,
            ),
        ),
        (
            Position::Right,
            Rect::from_center_size(
                Pos2::new(center.right() + 108.0 * scale, center.center().y),
                Vec2::new(168.0, 92.0) * scale,
            ),
        ),
        (
            Position::Top,
            Rect::from_center_size(
                Pos2::new(center.center().x, center.top() - 52.0 * scale),
                Vec2::new(168.0, 82.0) * scale,
            ),
        ),
        (
            Position::Bottom,
            Rect::from_center_size(
                Pos2::new(center.center().x, center.bottom() + 52.0 * scale),
                Vec2::new(168.0, 82.0) * scale,
            ),
        ),
    ]
}

pub(crate) fn screen_slot_at(slots: &[(Position, Rect); 4], point: Pos2) -> Option<Position> {
    slots.iter().find_map(|(position, rect)| {
        rect.expand(SLOT_DROP_TOLERANCE)
            .contains(point)
            .then_some(*position)
    })
}

pub(crate) fn screen_handle_rect(rect: Rect) -> Rect {
    Rect::from_center_size(
        Pos2::new(rect.left() + 14.0, rect.center().y),
        Vec2::new(20.0, 32.0),
    )
}

pub(crate) fn screen_content_rect(rect: Rect) -> Rect {
    Rect::from_min_max(
        Pos2::new(rect.left() + 30.0, rect.top() + 8.0),
        Pos2::new(rect.right() - 28.0, rect.bottom() - 8.0),
    )
}

pub(crate) fn screen_status_rect(rect: Rect) -> Rect {
    Rect::from_center_size(
        Pos2::new(rect.right() - 12.0, rect.top() + 12.0),
        Vec2::splat(13.0),
    )
}

pub(crate) fn position_occupied(
    state: &UiState,
    pending_positions: &HashMap<ClientHandle, PendingPosition>,
    pos: Position,
    except: Option<ClientHandle>,
) -> bool {
    state.occupied(pos, except)
        || pending_positions
            .iter()
            .any(|(handle, pending)| Some(*handle) != except && pending.target == pos)
}
