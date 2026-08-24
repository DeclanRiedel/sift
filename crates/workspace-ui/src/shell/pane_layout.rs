//! Pure recursive pane-layout rules. Runtime pane entities stay owned by the
//! shell; this tree owns only presentation geometry and stable pane ids.

use std::collections::HashSet;

use crate::presentation::{PaneAxis, PaneLayoutPresentation};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Up,
    Right,
    Down,
    Left,
}

impl SplitDirection {
    pub const fn axis(self) -> PaneAxis {
        match self {
            Self::Left | Self::Right => PaneAxis::Horizontal,
            Self::Up | Self::Down => PaneAxis::Vertical,
        }
    }

    const fn increasing(self) -> bool {
        matches!(self, Self::Right | Self::Down)
    }
}

pub fn leaf(pane_id: u64) -> PaneLayoutPresentation {
    PaneLayoutPresentation::Pane { pane_id }
}

pub fn from_legacy(pane_ids: &[u64], flexes: Vec<f32>) -> PaneLayoutPresentation {
    if pane_ids.len() == 1 {
        return leaf(pane_ids[0]);
    }
    let flexes = valid_flexes(flexes, pane_ids.len());
    PaneLayoutPresentation::Split {
        axis: PaneAxis::Horizontal,
        children: pane_ids.iter().copied().map(leaf).collect(),
        flexes,
    }
}

pub fn repair(
    layout: Option<PaneLayoutPresentation>,
    pane_ids: &[u64],
    legacy_flexes: Vec<f32>,
) -> PaneLayoutPresentation {
    let Some(layout) = layout else {
        return from_legacy(pane_ids, legacy_flexes);
    };
    let expected = pane_ids.iter().copied().collect::<HashSet<_>>();
    let mut actual = Vec::new();
    if validate_node(&layout, &mut actual)
        && actual.len() == expected.len()
        && actual.into_iter().collect::<HashSet<_>>() == expected
    {
        layout
    } else {
        from_legacy(pane_ids, legacy_flexes)
    }
}

fn validate_node(layout: &PaneLayoutPresentation, leaves: &mut Vec<u64>) -> bool {
    match layout {
        PaneLayoutPresentation::Pane { pane_id } => {
            leaves.push(*pane_id);
            true
        }
        PaneLayoutPresentation::Split {
            children, flexes, ..
        } => {
            children.len() >= 2
                && flexes.len() == children.len()
                && flexes.iter().all(|flex| flex.is_finite() && *flex > 0.0)
                && children.iter().all(|child| validate_node(child, leaves))
        }
    }
}

#[cfg(test)]
pub fn pane_ids(layout: &PaneLayoutPresentation) -> Vec<u64> {
    let mut ids = Vec::new();
    collect_pane_ids(layout, &mut ids);
    ids
}

#[cfg(test)]
fn collect_pane_ids(layout: &PaneLayoutPresentation, ids: &mut Vec<u64>) {
    match layout {
        PaneLayoutPresentation::Pane { pane_id } => ids.push(*pane_id),
        PaneLayoutPresentation::Split { children, .. } => {
            for child in children {
                collect_pane_ids(child, ids);
            }
        }
    }
}

pub fn split(
    layout: &mut PaneLayoutPresentation,
    target_id: u64,
    new_id: u64,
    direction: SplitDirection,
) -> bool {
    match layout {
        PaneLayoutPresentation::Pane { pane_id } if *pane_id == target_id => {
            let old = leaf(*pane_id);
            let new = leaf(new_id);
            let children = if direction.increasing() {
                vec![old, new]
            } else {
                vec![new, old]
            };
            *layout = PaneLayoutPresentation::Split {
                axis: direction.axis(),
                children,
                flexes: vec![1.0, 1.0],
            };
            true
        }
        PaneLayoutPresentation::Pane { .. } => false,
        PaneLayoutPresentation::Split {
            axis,
            children,
            flexes,
        } => {
            for index in 0..children.len() {
                if matches!(children[index], PaneLayoutPresentation::Pane { pane_id } if pane_id == target_id)
                {
                    if *axis == direction.axis() {
                        let insertion = if direction.increasing() {
                            index + 1
                        } else {
                            index
                        };
                        let target_flex = flexes[index] / 2.0;
                        flexes[index] = target_flex;
                        children.insert(insertion, leaf(new_id));
                        flexes.insert(insertion, target_flex);
                    } else {
                        let old = children[index].clone();
                        let new = leaf(new_id);
                        let nested_children = if direction.increasing() {
                            vec![old, new]
                        } else {
                            vec![new, old]
                        };
                        children[index] = PaneLayoutPresentation::Split {
                            axis: direction.axis(),
                            children: nested_children,
                            flexes: vec![1.0, 1.0],
                        };
                    }
                    return true;
                }
                if split(&mut children[index], target_id, new_id, direction) {
                    return true;
                }
            }
            false
        }
    }
}

/// Insert a pane at the outer edge of the complete layout. Matching root axes
/// stay flat; orthogonal directions wrap the existing tree.
pub fn split_root(layout: &mut PaneLayoutPresentation, new_id: u64, direction: SplitDirection) {
    if let PaneLayoutPresentation::Split {
        axis,
        children,
        flexes,
    } = layout
    {
        if *axis == direction.axis() {
            let index = if direction.increasing() {
                children.len()
            } else {
                0
            };
            children.insert(index, leaf(new_id));
            *flexes = vec![1.0; children.len()];
            return;
        }
    }

    let old = layout.clone();
    let new = leaf(new_id);
    let children = if direction.increasing() {
        vec![old, new]
    } else {
        vec![new, old]
    };
    *layout = PaneLayoutPresentation::Split {
        axis: direction.axis(),
        children,
        flexes: vec![1.0, 1.0],
    };
}

/// Find the visually nearest pane in one cardinal direction. Layout flexes are
/// projected into normalized rectangles, so navigation follows what the user
/// sees even through nested mixed-axis splits.
pub fn neighbor(
    layout: &PaneLayoutPresentation,
    pane_id: u64,
    direction: SplitDirection,
) -> Option<u64> {
    #[derive(Clone, Copy)]
    struct Leaf {
        id: u64,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    }

    fn collect(
        layout: &PaneLayoutPresentation,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        leaves: &mut Vec<Leaf>,
    ) {
        match layout {
            PaneLayoutPresentation::Pane { pane_id } => leaves.push(Leaf {
                id: *pane_id,
                x,
                y,
                width,
                height,
            }),
            PaneLayoutPresentation::Split {
                axis,
                children,
                flexes,
            } => {
                let total = flexes.iter().sum::<f32>().max(f32::EPSILON);
                let mut offset = 0.;
                for (child, flex) in children.iter().zip(flexes) {
                    let share = *flex / total;
                    match axis {
                        PaneAxis::Horizontal => {
                            let child_width = width * share;
                            collect(child, x + offset, y, child_width, height, leaves);
                            offset += child_width;
                        }
                        PaneAxis::Vertical => {
                            let child_height = height * share;
                            collect(child, x, y + offset, width, child_height, leaves);
                            offset += child_height;
                        }
                    }
                }
            }
        }
    }

    let mut leaves = Vec::new();
    collect(layout, 0., 0., 1., 1., &mut leaves);
    let current = leaves.iter().find(|leaf| leaf.id == pane_id)?;
    let center = |leaf: &Leaf| (leaf.x + leaf.width / 2., leaf.y + leaf.height / 2.);
    let (current_x, current_y) = center(current);
    leaves
        .iter()
        .filter(|candidate| candidate.id != pane_id)
        .filter_map(|candidate| {
            let (x, y) = center(candidate);
            let (primary, secondary) = match direction {
                SplitDirection::Left if x < current_x => (current_x - x, (current_y - y).abs()),
                SplitDirection::Right if x > current_x => (x - current_x, (current_y - y).abs()),
                SplitDirection::Up if y < current_y => (current_y - y, (current_x - x).abs()),
                SplitDirection::Down if y > current_y => (y - current_y, (current_x - x).abs()),
                _ => return None,
            };
            Some((primary + secondary * 2., primary, secondary, candidate.id))
        })
        .min_by(|left, right| {
            left.0
                .total_cmp(&right.0)
                .then(left.1.total_cmp(&right.1))
                .then(left.2.total_cmp(&right.2))
                .then(left.3.cmp(&right.3))
        })
        .map(|(_, _, _, id)| id)
}

pub fn remove(layout: &mut PaneLayoutPresentation, pane_id: u64) -> bool {
    let PaneLayoutPresentation::Split {
        children, flexes, ..
    } = layout
    else {
        return false;
    };

    for index in 0..children.len() {
        let direct = matches!(children[index], PaneLayoutPresentation::Pane { pane_id: id } if id == pane_id);
        let found = direct || remove(&mut children[index], pane_id);
        if !found {
            continue;
        }
        if direct {
            let removed_flex = flexes.remove(index);
            children.remove(index);
            let recipient = index.saturating_sub(1).min(flexes.len().saturating_sub(1));
            if let Some(flex) = flexes.get_mut(recipient) {
                *flex += removed_flex;
            }
        }
        if children.len() == 1 {
            *layout = children.remove(0);
        }
        return true;
    }
    false
}

pub fn resize(
    layout: &mut PaneLayoutPresentation,
    path: &[usize],
    boundary: usize,
    pointer: f32,
    available: f32,
    minimum: f32,
) {
    let Some(PaneLayoutPresentation::Split { flexes, .. }) = node_mut(layout, path) else {
        return;
    };
    resize_flexes(flexes, boundary, pointer, available, minimum);
}

fn node_mut<'a>(
    mut layout: &'a mut PaneLayoutPresentation,
    path: &[usize],
) -> Option<&'a mut PaneLayoutPresentation> {
    for index in path {
        let PaneLayoutPresentation::Split { children, .. } = layout else {
            return None;
        };
        layout = children.get_mut(*index)?;
    }
    Some(layout)
}

fn resize_flexes(flexes: &mut [f32], boundary: usize, pointer: f32, available: f32, minimum: f32) {
    if boundary + 1 >= flexes.len() || available <= 0.0 {
        return;
    }
    let total = flexes.iter().sum::<f32>();
    let pair_total = flexes[boundary] + flexes[boundary + 1];
    let prefix = flexes[..boundary].iter().sum::<f32>();
    let minimum = (minimum / available * total).min(pair_total / 2.0);
    let requested = pointer.clamp(0.0, available) / available * total - prefix;
    let first = requested.clamp(minimum, pair_total - minimum);
    flexes[boundary] = first;
    flexes[boundary + 1] = pair_total - first;
}

fn valid_flexes(mut flexes: Vec<f32>, count: usize) -> Vec<f32> {
    if flexes.len() != count || flexes.iter().any(|flex| !flex.is_finite() || *flex <= 0.0) {
        flexes = vec![1.0; count];
    }
    flexes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orthogonal_splits_nest_and_removal_collapses() {
        let mut layout = from_legacy(&[1, 2], vec![1.0, 1.0]);
        assert!(split(&mut layout, 1, 3, SplitDirection::Down));
        assert_eq!(pane_ids(&layout), vec![1, 3, 2]);
        assert!(matches!(
            layout,
            PaneLayoutPresentation::Split {
                axis: PaneAxis::Horizontal,
                ..
            }
        ));
        assert!(remove(&mut layout, 3));
        assert_eq!(pane_ids(&layout), vec![1, 2]);
        let PaneLayoutPresentation::Split { children, .. } = layout else {
            panic!("root split remains")
        };
        assert!(matches!(
            children[0],
            PaneLayoutPresentation::Pane { pane_id: 1 }
        ));
    }

    #[test]
    fn invalid_or_duplicate_layout_repairs_from_known_panes() {
        let invalid = PaneLayoutPresentation::Split {
            axis: PaneAxis::Vertical,
            children: vec![leaf(1), leaf(1)],
            flexes: vec![1.0, f32::NAN],
        };
        let repaired = repair(Some(invalid), &[1, 2], vec![2.0, 1.0]);
        assert_eq!(pane_ids(&repaired), vec![1, 2]);
        let PaneLayoutPresentation::Split { axis, flexes, .. } = repaired else {
            panic!("legacy panes become a split")
        };
        assert_eq!(axis, PaneAxis::Horizontal);
        assert_eq!(flexes, vec![2.0, 1.0]);
    }

    #[test]
    fn nested_resize_changes_only_target_axis_pair() {
        let mut layout = from_legacy(&[1, 2], vec![1.0, 1.0]);
        split(&mut layout, 1, 3, SplitDirection::Down);
        resize(&mut layout, &[0], 0, 75.0, 100.0, 10.0);
        let PaneLayoutPresentation::Split { children, .. } = layout else {
            panic!("root split")
        };
        let PaneLayoutPresentation::Split { flexes, .. } = &children[0] else {
            panic!("nested split")
        };
        assert_eq!(flexes, &vec![1.5, 0.5]);
    }

    #[test]
    fn directional_neighbor_follows_nested_visual_geometry() {
        let mut layout = from_legacy(&[1, 2], vec![1.0, 1.0]);
        assert!(split(&mut layout, 1, 3, SplitDirection::Down));

        assert_eq!(neighbor(&layout, 1, SplitDirection::Down), Some(3));
        assert_eq!(neighbor(&layout, 3, SplitDirection::Up), Some(1));
        assert_eq!(neighbor(&layout, 1, SplitDirection::Right), Some(2));
        assert_eq!(neighbor(&layout, 3, SplitDirection::Right), Some(2));
        assert_eq!(neighbor(&layout, 2, SplitDirection::Left), Some(1));
        assert_eq!(neighbor(&layout, 1, SplitDirection::Up), None);
    }
}
