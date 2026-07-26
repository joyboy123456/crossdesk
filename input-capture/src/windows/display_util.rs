use windows::Win32::Foundation::RECT;

use crate::Position;

fn is_within_dp_region(point: (i32, i32), display: &RECT) -> bool {
    [
        Position::Left,
        Position::Right,
        Position::Top,
        Position::Bottom,
    ]
    .iter()
    .all(|&pos| is_within_dp_boundary(point, display, pos))
}

fn is_within_dp_boundary(point: (i32, i32), display: &RECT, pos: Position) -> bool {
    let (x, y) = point;
    match pos {
        Position::Left => display.left <= x,
        Position::Right => display.right > x,
        Position::Top => display.top <= y,
        Position::Bottom => display.bottom > y,
    }
}

/// returns whether the given position is within the display bounds with respect to the given
/// barrier position
///
/// # Arguments
///
/// * `x`:
/// * `y`:
/// * `displays`:
/// * `pos`:
///
/// returns: bool
///
fn in_bounds(point: (i32, i32), displays: &[RECT], pos: Position) -> bool {
    displays
        .iter()
        .any(|d| is_within_dp_boundary(point, d, pos))
}

fn in_display_region(point: (i32, i32), displays: &[RECT]) -> bool {
    displays.iter().any(|d| is_within_dp_region(point, d))
}

fn moved_across_boundary(
    prev_pos: (i32, i32),
    curr_pos: (i32, i32),
    displays: &[RECT],
    pos: Position,
) -> bool {
    /* was within bounds, but is not anymore */
    in_display_region(prev_pos, displays) && !in_bounds(curr_pos, displays, pos)
}

pub(crate) fn entered_barrier(
    prev_pos: (i32, i32),
    curr_pos: (i32, i32),
    displays: &[RECT],
) -> Option<Position> {
    [
        Position::Left,
        Position::Right,
        Position::Top,
        Position::Bottom,
    ]
    .into_iter()
    .find(|&pos| moved_across_boundary(prev_pos, curr_pos, displays, pos))
}

///
/// clamp point to display bounds
///
/// # Arguments
///
/// * `prev_point`: coordinates, the cursor was before entering, within bounds of a display
/// * `entry_point`: point to clamp
///
/// returns: (i32, i32), the corrected entry point
///
pub(crate) fn clamp_to_display_bounds(
    display_regions: &[RECT],
    prev_point: (i32, i32),
    point: (i32, i32),
) -> (i32, i32) {
    /* find display where movement came from */
    let display = display_regions
        .iter()
        .find(|&d| is_within_dp_region(prev_point, d))
        .unwrap();

    /* clamp to bounds (inclusive) */
    let (x, y) = point;
    let (min_x, max_x) = (display.left, display.right - 1);
    let (min_y, max_y) = (display.top, display.bottom - 1);
    (x.clamp(min_x, max_x), y.clamp(min_y, max_y))
}

/// bounding box of all displays
fn bounding_box(displays: &[RECT]) -> RECT {
    displays.iter().fold(
        RECT {
            left: i32::MAX,
            top: i32::MAX,
            right: i32::MIN,
            bottom: i32::MIN,
        },
        |bbox, d| RECT {
            left: bbox.left.min(d.left),
            top: bbox.top.min(d.top),
            right: bbox.right.max(d.right),
            bottom: bbox.bottom.max(d.bottom),
        },
    )
}

/// normalized position of `point` along the barrier edge `pos`, relative to
/// the desktop bounding box (top/left = 0.0)
pub(crate) fn edge_ratio(displays: &[RECT], pos: Position, point: (i32, i32)) -> f64 {
    let bbox = bounding_box(displays);
    let (coord, min, max) = match pos {
        Position::Left | Position::Right => (point.1, bbox.top, bbox.bottom - 1),
        Position::Top | Position::Bottom => (point.0, bbox.left, bbox.right - 1),
    };
    if max <= min {
        return 0.0;
    }
    ((coord - min) as f64 / (max - min) as f64).clamp(0.0, 1.0)
}

/// the point at `ratio` along the barrier edge `pos` of the desktop bounding
/// box, moved 1px inward from the edge (so it cannot immediately re-trigger
/// the barrier) and snapped into the nearest display actually touching that
/// edge
pub(crate) fn point_on_edge(displays: &[RECT], pos: Position, ratio: f64) -> (i32, i32) {
    let bbox = bounding_box(displays);
    let ratio = ratio.clamp(0.0, 1.0);
    let along = |min: i32, max: i32| min + ((max - min) as f64 * ratio).round() as i32;
    let point = match pos {
        Position::Left => (bbox.left + 1, along(bbox.top, bbox.bottom - 1)),
        Position::Right => (bbox.right - 2, along(bbox.top, bbox.bottom - 1)),
        Position::Top => (along(bbox.left, bbox.right - 1), bbox.top + 1),
        Position::Bottom => (along(bbox.left, bbox.right - 1), bbox.bottom - 2),
    };

    /* the mapped point may fall on a stretch of the bounding box edge where
     * no display exists; snap into the display touching that edge whose
     * along-edge range is closest */
    let touches_edge = |d: &&RECT| match pos {
        Position::Left => d.left == bbox.left,
        Position::Right => d.right == bbox.right,
        Position::Top => d.top == bbox.top,
        Position::Bottom => d.bottom == bbox.bottom,
    };
    let along_coord = match pos {
        Position::Left | Position::Right => point.1,
        Position::Top | Position::Bottom => point.0,
    };
    let distance = |d: &&RECT| {
        let (min, max) = match pos {
            Position::Left | Position::Right => (d.top, d.bottom - 1),
            Position::Top | Position::Bottom => (d.left, d.right - 1),
        };
        (along_coord - along_coord.clamp(min, max)).abs()
    };
    let display = displays
        .iter()
        .filter(touches_edge)
        .min_by_key(distance)
        .or_else(|| displays.iter().min_by_key(distance));
    match display {
        Some(d) => (
            point.0.clamp(d.left, d.right - 1),
            point.1.clamp(d.top, d.bottom - 1),
        ),
        None => point,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIMARY: RECT = RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };

    #[test]
    fn edge_ratio_single_display() {
        let displays = [PRIMARY];
        assert_eq!(edge_ratio(&displays, Position::Right, (1919, 0)), 0.0);
        assert_eq!(edge_ratio(&displays, Position::Right, (1919, 1079)), 1.0);
        let mid = edge_ratio(&displays, Position::Right, (1919, 540));
        assert!((mid - 0.5).abs() < 0.01);
        assert_eq!(edge_ratio(&displays, Position::Bottom, (0, 1079)), 0.0);
        assert_eq!(edge_ratio(&displays, Position::Bottom, (1919, 1079)), 1.0);
    }

    #[test]
    fn point_on_edge_is_inside_and_round_trips() {
        let displays = [PRIMARY];
        for pos in [
            Position::Left,
            Position::Right,
            Position::Top,
            Position::Bottom,
        ] {
            for ratio in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let point = point_on_edge(&displays, pos, ratio);
                assert!(
                    in_display_region(point, &displays),
                    "{pos:?} @ {ratio} -> {point:?} outside display"
                );
                // the placed point must not re-trigger any barrier when the
                // cursor moves onto it
                assert_eq!(entered_barrier(point, point, &displays), None);
                let recovered = edge_ratio(&displays, pos, point);
                assert!(
                    (recovered - ratio).abs() < 0.01,
                    "{pos:?}: {ratio} -> {point:?} -> {recovered}"
                );
            }
        }
    }

    #[test]
    fn point_on_edge_snaps_into_display_touching_the_edge() {
        // secondary display is shorter and offset: right edge of the bbox is
        // only covered by the secondary display for y in 0..600
        let secondary = RECT {
            left: 1920,
            top: 0,
            right: 3840,
            bottom: 600,
        };
        let displays = [PRIMARY, secondary];

        // ratio 1.0 maps to bbox bottom-right, where no display touches the
        // right edge -> must snap into the secondary display
        let point = point_on_edge(&displays, Position::Right, 1.0);
        assert!(in_display_region(point, &displays), "snapped to {point:?}");
        assert_eq!(point, (3838, 599));
    }

    #[test]
    fn out_of_range_ratio_is_clamped() {
        let displays = [PRIMARY];
        assert_eq!(
            point_on_edge(&displays, Position::Right, -1.0),
            point_on_edge(&displays, Position::Right, 0.0)
        );
        assert_eq!(
            point_on_edge(&displays, Position::Right, 2.0),
            point_on_edge(&displays, Position::Right, 1.0)
        );
    }
}
