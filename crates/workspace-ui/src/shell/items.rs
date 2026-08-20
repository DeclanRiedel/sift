//! Host-owned pane-item registry.
//!
//! Registry centralizes runtime ownership and fallback presentation for each
//! persisted item kind. Adding first-party item kinds requires an exhaustive
//! Rust change; extensions cannot inject renderers.

use crate::presentation::ItemKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemRuntimeKind {
    Query,
    Configuration,
    ReadOnlyText,
    Placeholder,
}

impl ItemRuntimeKind {
    pub const fn is_editor(self) -> bool {
        matches!(self, Self::Query | Self::Configuration | Self::ReadOnlyText)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemDefinition {
    pub kind: ItemKind,
    pub runtime: ItemRuntimeKind,
    pub placeholder_prefix: Option<&'static str>,
    pub empty_message: &'static str,
}

pub struct ItemRegistry;

impl ItemRegistry {
    pub fn definition(kind: &ItemKind) -> &'static ItemDefinition {
        match kind {
            ItemKind::Query => &QUERY,
            ItemKind::Configuration => &CONFIGURATION,
            ItemKind::Problems => &PROBLEMS,
            ItemKind::Schema => &SCHEMA,
            ItemKind::Welcome => &WELCOME,
        }
    }

    pub fn definitions() -> &'static [ItemDefinition] {
        &DEFINITIONS
    }
}

const QUERY: ItemDefinition = ItemDefinition {
    kind: ItemKind::Query,
    runtime: ItemRuntimeKind::Query,
    placeholder_prefix: Some("Query editor"),
    empty_message: "Query editor is unavailable",
};

const CONFIGURATION: ItemDefinition = ItemDefinition {
    kind: ItemKind::Configuration,
    runtime: ItemRuntimeKind::Configuration,
    placeholder_prefix: Some("TOML editor"),
    empty_message: "Configuration editor is unavailable",
};

const PROBLEMS: ItemDefinition = ItemDefinition {
    kind: ItemKind::Problems,
    runtime: ItemRuntimeKind::ReadOnlyText,
    placeholder_prefix: Some("Problems"),
    empty_message: "Problems feed is unavailable",
};

const SCHEMA: ItemDefinition = ItemDefinition {
    kind: ItemKind::Schema,
    runtime: ItemRuntimeKind::Placeholder,
    placeholder_prefix: Some("Schema view"),
    empty_message: "Schema view is unavailable",
};

const WELCOME: ItemDefinition = ItemDefinition {
    kind: ItemKind::Welcome,
    runtime: ItemRuntimeKind::Placeholder,
    placeholder_prefix: None,
    empty_message: "Open a connection or create a query to begin.",
};

const DEFINITIONS: [ItemDefinition; 5] = [QUERY, CONFIGURATION, PROBLEMS, SCHEMA, WELCOME];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_persisted_item_kind_has_runtime_metadata() {
        for kind in [
            ItemKind::Query,
            ItemKind::Configuration,
            ItemKind::Problems,
            ItemKind::Schema,
            ItemKind::Welcome,
        ] {
            assert_eq!(ItemRegistry::definition(&kind).kind, kind);
        }
        assert_eq!(ItemRegistry::definitions().len(), 5);
    }
}
