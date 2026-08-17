//! Typed identities and metadata for host-owned workspace docks.

use crate::presentation::DockPresentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DockId {
    Connections,
    Inspector,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DockPlacement {
    Left,
    Right,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockDefinition {
    pub id: DockId,
    pub title: &'static str,
    pub placement: DockPlacement,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dock {
    pub id: DockId,
    pub presentation: DockPresentation,
}

impl Dock {
    pub fn definition(&self) -> &'static DockDefinition {
        DockRegistry::definition(self.id)
    }
}

pub struct DockRegistry;

impl DockRegistry {
    pub fn definition(id: DockId) -> &'static DockDefinition {
        DEFINITIONS
            .iter()
            .find(|definition| definition.id == id)
            .expect("every dock id must have one definition")
    }

    pub fn create(id: DockId, presentation: DockPresentation) -> Dock {
        Dock { id, presentation }
    }

    pub fn definitions() -> &'static [DockDefinition] {
        DEFINITIONS
    }
}

const DEFINITIONS: &[DockDefinition] = &[
    DockDefinition {
        id: DockId::Connections,
        title: "Connections",
        placement: DockPlacement::Left,
    },
    DockDefinition {
        id: DockId::Inspector,
        title: "Inspector",
        placement: DockPlacement::Right,
    },
    DockDefinition {
        id: DockId::Output,
        title: "Output",
        placement: DockPlacement::Bottom,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn built_in_docks_have_unique_ids_and_placements() {
        let mut ids = HashSet::new();
        let mut placements = HashSet::new();
        for definition in DockRegistry::definitions() {
            assert!(ids.insert(definition.id));
            assert!(placements.insert(definition.placement));
            assert_eq!(DockRegistry::definition(definition.id), definition);
        }
    }
}
