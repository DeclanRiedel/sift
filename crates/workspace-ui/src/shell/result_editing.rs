use super::*;
use std::sync::OnceLock;

pub(super) fn read_only_message(kind: sift_protocol::ObjectKind) -> &'static str {
    match kind {
        sift_protocol::ObjectKind::View | sift_protocol::ObjectKind::MaterializedView => {
            "Cannot edit rows in a view"
        }
        _ => "Cannot edit rows for this object type",
    }
}

// Deliberately narrow: no aliases, joins, expressions, CTEs or multiple statements.
// Qualified names avoid guessing PostgreSQL's connection-local search_path.
fn postgres_select_all_target(sql: &str) -> Option<(String, String)> {
    if sql.len() > 256 * 1024 {
        return None;
    }
    static SELECT: OnceLock<Regex> = OnceLock::new();
    let regex = SELECT.get_or_init(|| Regex::new(
        r#"(?i)^\s*SELECT\s+\*\s+FROM\s+("(?:[^"]|"")+"|[a-z_][a-z0-9_$]*)\s*\.\s*("(?:[^"]|"")+"|[a-z_][a-z0-9_$]*)(?:\s+LIMIT\s+[0-9]+(?:\s+OFFSET\s+[0-9]+)?)?\s*;?\s*$"#
    ).expect("static select pattern"));
    let capture = regex.captures(sql)?;
    let identifier = |value: &str| {
        if value.starts_with('"') {
            value[1..value.len() - 1].replace("\"\"", "\"")
        } else {
            value.to_ascii_lowercase()
        }
    };
    Some((identifier(&capture[1]), identifier(&capture[2])))
}

impl WorkspaceShell {
    pub(super) fn executed_result_source(
        &self,
        item_id: u64,
        sql: &str,
        cx: &App,
    ) -> Option<DatabaseObjectSource> {
        let existing = self.database_source(item_id, cx);
        let profile = self.query_profile_id(item_id, cx)?;
        let target = self
            .sourced_semantic_target(item_id, cx)
            .or_else(|| self.query_semantic_targets.get(&item_id).cloned())
            .or_else(|| self.semantic_target_for_profile(profile))?;
        if target.instance_id != self.selected_instance_id.as_deref().unwrap_or("local") {
            return None;
        }
        if target.provider_id != sift_protocol::Engine::Postgres.provider_id() {
            return existing;
        }
        let (schema, object) = postgres_select_all_target(sql)?;
        if let Some(source) =
            existing.filter(|source| source.schema == schema && source.object == object)
        {
            return Some(source);
        }
        let ConnectionSchemaState::Ready {
            profile_id,
            snapshot,
        } = &self.connection_schema
        else {
            return None;
        };
        if *profile_id != profile {
            return None;
        }
        let mut matches = snapshot
            .trees
            .iter()
            .flat_map(|catalog| {
                catalog
                    .schemas
                    .iter()
                    .filter(|entry| entry.name == schema)
                    .flat_map(move |schema| {
                        schema.objects.iter().map(move |object| (catalog, object))
                    })
            })
            .filter(|(_, candidate)| {
                candidate.name == object
                    && matches!(
                        candidate.kind,
                        sift_protocol::ObjectKind::Table
                            | sift_protocol::ObjectKind::View
                            | sift_protocol::ObjectKind::MaterializedView
                    )
            });
        let (catalog, entry) = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(DatabaseObjectSource {
            instance_id: target.instance_id,
            tenant_id: target.tenant_id,
            profile_id: target.profile_id,
            profile_name: target.profile_name,
            provider_id: target.provider_id,
            catalog: Some(catalog.name.clone()),
            schema,
            object,
            object_kind: entry.kind,
            last_refreshed_at_ms: None,
        })
    }

    pub(super) fn result_edit_source(
        &self,
        pane: &Entity<Pane>,
        item_id: u64,
        cx: &App,
    ) -> Option<DatabaseObjectSource> {
        // A newly dispatched run can briefly coexist with the previous grid.
        // Never apply its new target to those old rows or to a partial stream.
        if self.running_queries.contains_key(&item_id) {
            return None;
        }
        if pane
            .read(cx)
            .results
            .get(&item_id)
            .is_some_and(|results| results.read(cx).result_set_count() != 1)
        {
            return None;
        }
        self.result_edit_sources
            .get(&item_id)
            .cloned()
            .unwrap_or_else(|| pane.read(cx).database_source(item_id))
            .filter(|source| {
                source.instance_id == self.selected_instance_id.as_deref().unwrap_or("local")
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn qualified_select_all_is_conservative_and_preserves_quoted_names() {
        for sql in [
            "SELECT * FROM \"lab\".\"orders\" LIMIT 100;",
            "select * from LAB.ORDERS",
            "SELECT * FROM lab.orders LIMIT 50 OFFSET 10",
        ] {
            assert_eq!(
                postgres_select_all_target(sql),
                Some(("lab".into(), "orders".into()))
            );
        }
        assert_eq!(
            postgres_select_all_target("SELECT * FROM \"Lab\".\"odd\"\"name\";"),
            Some(("Lab".into(), "odd\"name".into()))
        );
        for sql in [
            "SELECT * FROM orders",
            "SELECT id + 1 FROM lab.orders",
            "SELECT * FROM lab.orders JOIN lab.items USING(id)",
            "SELECT * FROM lab.orders; SELECT * FROM lab.items",
            "SELECT DISTINCT * FROM lab.orders",
            "WITH x AS (SELECT 1) SELECT * FROM lab.orders",
            "SELECT * FROM lab.orders UNION SELECT * FROM lab.items",
            "SELECT * FROM lab.orders WHERE false",
        ] {
            assert!(postgres_select_all_target(sql).is_none(), "{sql}");
        }
    }
}
