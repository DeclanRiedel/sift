//! Filter borrowed schema objects before allocating visible tree rows.

use sift_protocol::ObjectKind;
use std::fmt::Write as _;

pub(super) fn matches_object(
    catalog: &str,
    schema: &str,
    object: &str,
    kind: ObjectKind,
    query: &str,
    buffer: &mut String,
) -> bool {
    if query.is_empty() {
        return true;
    }
    // Without a separator, a match cannot span path components. Common
    // lowercase identifiers use str's substring search without formatting.
    if !query.contains(['.', ' ']) {
        if [catalog, schema, object]
            .into_iter()
            .any(|part| contains_folded(part, query))
        {
            return true;
        }
        buffer.clear();
        write!(buffer, "{kind:?}").expect("writing to a String");
    } else {
        buffer.clear();
        write!(buffer, "{catalog}.{schema}.{object} {kind:?}").expect("writing to a String");
    }
    contains_folded(buffer, query)
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
        for query in ["", "warehouse.public.orders", "orders table", "public.or"] {
            assert!(matches_object(
                "Warehouse",
                "Public",
                "Orders",
                ObjectKind::Table,
                query,
                &mut buffer
            ));
        }
        assert!(!matches_object(
            "Warehouse",
            "Public",
            "Orders",
            ObjectKind::Table,
            "missing",
            &mut buffer
        ));
        assert!(matches_object(
            "Warehouse",
            "Public",
            "Événements",
            ObjectKind::Table,
            "événements",
            &mut buffer
        ));
    }
}
