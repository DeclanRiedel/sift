//! Pure dock sizing rules shared by drag handling and window-size recovery.

use super::DockId;

pub(super) const MIN_SIDE_DOCK_SIZE: f32 = 160.0;
pub(super) const MAX_SIDE_DOCK_SIZE: f32 = 480.0;
pub(super) const MIN_CENTER_WIDTH: f32 = 320.0;
pub(super) const MIN_BOTTOM_DOCK_SIZE: f32 = 96.0;
pub(super) const MAX_BOTTOM_DOCK_SIZE: f32 = 480.0;
pub(super) const MIN_EDITOR_HEIGHT: f32 = 180.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SideDockSizes {
    pub left: f32,
    pub right: f32,
}

fn clamp_side(size: f32) -> f32 {
    size.clamp(MIN_SIDE_DOCK_SIZE, MAX_SIDE_DOCK_SIZE)
}

/// Fit restored dock sizes into the viewport while preserving a useful center.
/// When both docks are open, any necessary reduction is distributed according
/// to how much each dock exceeds its minimum rather than penalizing one side.
pub(super) fn fit_side_docks(
    total_width: f32,
    left: f32,
    right: f32,
    left_open: bool,
    right_open: bool,
) -> SideDockSizes {
    let mut sizes = SideDockSizes {
        left: clamp_side(left),
        right: clamp_side(right),
    };
    let available = (total_width - MIN_CENTER_WIDTH).max(0.0);

    match (left_open, right_open) {
        (true, true) if sizes.left + sizes.right > available => {
            let minimum_total = MIN_SIDE_DOCK_SIZE * 2.0;
            if available <= minimum_total {
                let each = available / 2.0;
                sizes.left = each;
                sizes.right = each;
            } else {
                let left_extra = sizes.left - MIN_SIDE_DOCK_SIZE;
                let right_extra = sizes.right - MIN_SIDE_DOCK_SIZE;
                let extra_total = left_extra + right_extra;
                let extra_budget = available - minimum_total;
                if extra_total > 0.0 {
                    sizes.left = MIN_SIDE_DOCK_SIZE + extra_budget * (left_extra / extra_total);
                    sizes.right = MIN_SIDE_DOCK_SIZE + extra_budget * (right_extra / extra_total);
                }
            }
        }
        (true, false) => sizes.left = sizes.left.min(available),
        (false, true) => sizes.right = sizes.right.min(available),
        _ => {}
    }

    sizes
}

/// Resize one side dock. If it grows into the reserved center area, the
/// opposite open dock yields down to its minimum before the dragged dock is
/// itself constrained.
pub(super) fn resize_side_dock(
    total_width: f32,
    current: SideDockSizes,
    left_open: bool,
    right_open: bool,
    dragged: DockId,
    requested_size: f32,
) -> SideDockSizes {
    let mut sizes = fit_side_docks(
        total_width,
        current.left,
        current.right,
        left_open,
        right_open,
    );
    let available = (total_width - MIN_CENTER_WIDTH).max(0.0);
    let requested = clamp_side(requested_size);

    match dragged {
        DockId::Left if left_open => {
            sizes.left = requested;
            if right_open && sizes.left + sizes.right > available {
                sizes.right = (available - sizes.left).max(MIN_SIDE_DOCK_SIZE);
            }
            sizes.left = sizes
                .left
                .min((available - if right_open { sizes.right } else { 0.0 }).max(0.0));
        }
        DockId::Inspector if right_open => {
            sizes.right = requested;
            if left_open && sizes.left + sizes.right > available {
                sizes.left = (available - sizes.right).max(MIN_SIDE_DOCK_SIZE);
            }
            sizes.right = sizes
                .right
                .min((available - if left_open { sizes.left } else { 0.0 }).max(0.0));
        }
        DockId::Bottom | DockId::Left | DockId::Inspector => {}
    }

    sizes
}

pub(super) fn fit_bottom_dock(available_height: f32, requested_size: f32) -> f32 {
    let maximum = MAX_BOTTOM_DOCK_SIZE.min((available_height - MIN_EDITOR_HEIGHT).max(0.0));
    if maximum < MIN_BOTTOM_DOCK_SIZE {
        maximum
    } else {
        requested_size.clamp(MIN_BOTTOM_DOCK_SIZE, maximum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growing_a_side_dock_makes_the_opposite_dock_yield() {
        let sizes = resize_side_dock(
            800.0,
            SideDockSizes {
                left: 220.0,
                right: 220.0,
            },
            true,
            true,
            DockId::Left,
            400.0,
        );

        assert_eq!(sizes.left, 320.0);
        assert_eq!(sizes.right, MIN_SIDE_DOCK_SIZE);
        assert_eq!(800.0 - sizes.left - sizes.right, MIN_CENTER_WIDTH);
    }

    #[test]
    fn fitting_a_narrower_window_reduces_both_side_docks_proportionally() {
        let sizes = fit_side_docks(760.0, 300.0, 220.0, true, true);

        assert_eq!(sizes.left + sizes.right, 440.0);
        assert!(sizes.left > sizes.right);
        assert_eq!(760.0 - sizes.left - sizes.right, MIN_CENTER_WIDTH);
    }

    #[test]
    fn bottom_dock_preserves_the_editor_height() {
        assert_eq!(fit_bottom_dock(600.0, 470.0), 420.0);
        assert_eq!(fit_bottom_dock(600.0, 40.0), MIN_BOTTOM_DOCK_SIZE);
    }
}
