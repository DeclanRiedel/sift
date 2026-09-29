//! Filter borrowed schema objects before allocating visible tree rows.

use sift_protocol::ObjectKind;
use std::fmt::Write as _;

#[derive(Clone, Copy)]
pub(super) enum FilterMode {
    Qualified,
    Unqualified { scope_matches: bool },
}

pub(super) fn matches_object(
    catalog: &str,
    schema: &str,
    object: &str,
    kind: ObjectKind,
    query: &str,
    mode: FilterMode,
    buffer: &mut String,
) -> bool {
    if query.is_empty() {
        return true;
    }
    // Without a separator, a match cannot span path components. Common
    // lowercase identifiers use str's substring search without formatting.
    match mode {
        FilterMode::Unqualified { scope_matches } => {
            scope_matches || contains_folded(object, query) || kind_name(kind).contains(query)
        }
        FilterMode::Qualified => {
            buffer.clear();
            write!(buffer, "{catalog}.{schema}.{object} {kind:?}").expect("writing to a String");
            contains_folded(buffer, query)
        }
    }
}

pub(super) fn is_unqualified_query(query: &str) -> bool {
    !query.contains(['.', ' '])
}

pub(super) fn filter_mode(
    catalog: &str,
    schema: &str,
    query: &str,
    unqualified_query: bool,
) -> FilterMode {
    if unqualified_query {
        FilterMode::Unqualified {
            scope_matches: contains_folded(catalog, query) || contains_folded(schema, query),
        }
    } else {
        FilterMode::Qualified
    }
}

const fn kind_name(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Table => "table",
        ObjectKind::View => "view",
        ObjectKind::MaterializedView => "materializedview",
        ObjectKind::ForeignTable => "foreigntable",
        ObjectKind::PartitionedTable => "partitionedtable",
        ObjectKind::TableValuedFunction => "tablevaluedfunction",
        ObjectKind::ScalarFunction => "scalarfunction",
        ObjectKind::Procedure => "procedure",
        ObjectKind::Synonym => "synonym",
        ObjectKind::Sequence => "sequence",
        ObjectKind::Index => "index",
        ObjectKind::Trigger => "trigger",
        ObjectKind::Type => "type",
        ObjectKind::Extension => "extension",
    }
}

fn contains_folded(value: &str, query: &str) -> bool {
    if value.contains(query) {
        return true;
    }
    if value.is_ascii() {
        value.bytes().any(|byte| byte.is_ascii_uppercase())
            && value
                .as_bytes()
                .windows(query.len())
                .any(|part| part.eq_ignore_ascii_case(query.as_bytes()))
    } else {
        value.to_lowercase().contains(query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_preserves_qualified_paths_kinds_and_unicode() {
        let mut buffer = String::new();
        let mut matches = |object: &str, kind: ObjectKind, query: &str| {
            matches_object(
                "Warehouse",
                "Public",
                object,
                kind,
                query,
                filter_mode("Warehouse", "Public", query, is_unqualified_query(query)),
                &mut buffer,
            )
        };
        for query in ["", "warehouse.public.orders", "orders table", "public.or"] {
            assert!(matches("Orders", ObjectKind::Table, query));
        }
        assert!(!matches("Orders", ObjectKind::Table, "missing"));
        assert!(matches("Événements", ObjectKind::Table, "événements"));
        assert!(matches(
            "Orders",
            ObjectKind::MaterializedView,
            "materializedview"
        ));
        assert!(matches("Other", ObjectKind::Table, "warehouse"));
    }
}
