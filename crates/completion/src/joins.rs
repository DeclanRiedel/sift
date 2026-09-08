//! Bounded, catalog-proven JOIN paths. No guessed keys or SQL definitions.
use std::collections::{HashMap, VecDeque};

use sift_protocol::completion::{CompletionCandidate, CompletionKind};
use sift_protocol::{
    CatalogEdgeCertainty, CatalogEdgeKind, CatalogGraphData, CatalogNode, CatalogNodeKind, Engine,
};

use crate::ContextResult;

struct Edge<'a> {
    from: &'a CatalogNode,
    to: &'a CatalogNode,
    pairs: Vec<(&'a str, &'a str)>,
}

/// Return direct and multi-hop JOIN snippets, ordered by path length.
pub fn join_candidates(
    ctx: &ContextResult,
    graph: &CatalogGraphData,
    engine: Engine,
    limit: usize,
) -> Vec<CompletionCandidate> {
    if !ctx.join_slot || limit == 0 {
        return Vec::new();
    }
    let nodes = graph
        .nodes
        .iter()
        .map(|node| (&node.id, node))
        .collect::<HashMap<_, _>>();
    let mut edges = Vec::new();
    for edge in &graph.edges {
        if edge.kind != CatalogEdgeKind::ForeignKey
            || edge.certainty != CatalogEdgeCertainty::CatalogProven
            || edge.column_pairs.is_empty()
        {
            continue;
        }
        let Some(constraint) = nodes.get(&edge.from) else {
            continue;
        };
        let Some(from) = constraint
            .parent_id
            .as_ref()
            .and_then(|id| nodes.get(id))
            .copied()
        else {
            continue;
        };
        let Some(to) = edge.to.as_ref().and_then(|id| nodes.get(id)).copied() else {
            continue;
        };
        let pairs = edge
            .column_pairs
            .iter()
            .map(|pair| {
                let source = nodes.get(&pair.from)?;
                let target = nodes.get(&pair.to)?;
                (source.kind == CatalogNodeKind::Column
                    && target.kind == CatalogNodeKind::Column
                    && source.parent_id.as_ref() == Some(&from.id)
                    && target.parent_id.as_ref() == Some(&to.id))
                .then_some((source.name.as_str(), target.name.as_str()))
            })
            .collect::<Option<Vec<_>>>();
        let Some(pairs) = pairs else {
            continue;
        };
        edges.push(Edge {
            from,
            to,
            pairs: pairs.clone(),
        });
        edges.push(Edge {
            from: to,
            to: from,
            pairs: pairs.into_iter().map(|(a, b)| (b, a)).collect(),
        });
    }
    edges.sort_by(|a, b| {
        a.from
            .qualified_name
            .cmp(&b.from.qualified_name)
            .then_with(|| a.to.qualified_name.cmp(&b.to.qualified_name))
            .then_with(|| a.pairs.cmp(&b.pairs))
    });
    let mut queue = VecDeque::new();
    for relation in &ctx.relations {
        let Some(reference) = relation.target.as_deref() else {
            continue;
        };
        if !relation.is_alias
            && ctx
                .relations
                .iter()
                .any(|r| r.is_alias && r.target.as_deref() == Some(reference))
        {
            continue;
        }
        let matches = graph
            .nodes
            .iter()
            .filter(|node| {
                matches!(
                    node.kind,
                    CatalogNodeKind::Table
                        | CatalogNodeKind::PartitionedTable
                        | CatalogNodeKind::ForeignTable
                ) && (node.qualified_name.eq_ignore_ascii_case(reference)
                    || node
                        .parent_id
                        .as_ref()
                        .and_then(|id| nodes.get(id))
                        .is_some_and(|schema| {
                            format!("{}.{}", schema.name, node.name).eq_ignore_ascii_case(reference)
                        })
                    || (!reference.contains('.') && node.name.eq_ignore_ascii_case(reference)))
            })
            .collect::<Vec<_>>();
        if let [node] = matches.as_slice() {
            queue.push_back((
                *node,
                quote(&relation.name, engine),
                vec![node.id.clone()],
                String::new(),
            ));
        }
    }
    let mut out = Vec::new();
    let mut work = 0;
    while let Some((node, qualifier, visited, sql)) = queue.pop_front() {
        if visited.len() > ctx.join_max_hops.min(3) {
            continue;
        }
        for edge in edges.iter().filter(|edge| edge.from.id == node.id) {
            work += 1;
            if work > 2048 {
                return out;
            }
            if visited.contains(&edge.to.id) && !(visited.len() == 1 && edge.to.id == node.id) {
                continue;
            }
            let mut alias = format!("sift_join_{}", visited.len());
            while ctx
                .relations
                .iter()
                .any(|relation| relation.name.eq_ignore_ascii_case(&alias))
            {
                alias.push('_');
            }
            let Some(table) = qualified(edge.to, &nodes, engine) else {
                continue;
            };
            let on = edge
                .pairs
                .iter()
                .map(|(from, to)| {
                    format!(
                        "{qualifier}.{} = {alias}.{}",
                        quote(from, engine),
                        quote(to, engine)
                    )
                })
                .collect::<Vec<_>>()
                .join(" AND ");
            let insert = format!(
                "{sql}{}{table} AS {alias} ON {on}",
                if sql.is_empty() { "" } else { " JOIN " }
            );
            if edge
                .to
                .name
                .to_ascii_lowercase()
                .contains(&ctx.prefix_lower)
            {
                out.push(CompletionCandidate {
                    label: format!("{} ({}-hop JOIN)", edge.to.qualified_name, visited.len())
                        .into(),
                    insert: insert.clone().into(),
                    // This is literal catalog SQL, not a user snippet. In
                    // particular, `$1` inside an identifier is not a tabstop.
                    kind: CompletionKind::Table,
                    detail: Some("Catalog-proven foreign key; review JOIN before execution".into()),
                    qualified_name: Some(edge.to.qualified_name.clone()),
                    score: 2000 - visited.len() as i32 * 100,
                });
                if out.len() >= limit.min(50) {
                    return out;
                }
            }
            let mut next = visited.clone();
            if edge.to.id == node.id {
                continue;
            }
            next.push(edge.to.id.clone());
            queue.push_back((edge.to, alias, next, insert));
        }
    }
    out
}

fn quote(name: &str, engine: Engine) -> String {
    match engine {
        Engine::SqlServer => format!("[{}]", name.replace(']', "]]")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

fn qualified(
    node: &CatalogNode,
    nodes: &HashMap<&sift_protocol::CatalogObjectId, &CatalogNode>,
    engine: Engine,
) -> Option<String> {
    let schema = nodes.get(node.parent_id.as_ref()?)?;
    if schema.kind != CatalogNodeKind::Schema {
        return None;
    }
    Some(format!(
        "{}.{}",
        quote(&schema.name, engine),
        quote(&node.name, engine)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::*;

    fn graph() -> CatalogGraphData {
        let mut graph = CatalogGraphData {
            coverage: CatalogCoverage::complete(),
            nodes: vec![],
            edges: vec![],
        };
        for (id, kind, parent, name) in [
            ("s", CatalogNodeKind::Schema, None, "public"),
            ("a", CatalogNodeKind::Table, Some("s"), "orders"),
            ("b", CatalogNodeKind::Table, Some("s"), "users"),
            ("c", CatalogNodeKind::Table, Some("s"), "teams"),
            ("a1", CatalogNodeKind::Column, Some("a"), "user_id"),
            ("a2", CatalogNodeKind::Column, Some("a"), "tenant"),
            ("b1", CatalogNodeKind::Column, Some("b"), "id"),
            ("b2", CatalogNodeKind::Column, Some("b"), "tenant"),
            ("c1", CatalogNodeKind::Column, Some("c"), "id"),
            (
                "fk1",
                CatalogNodeKind::Constraint,
                Some("a"),
                "orders_users",
            ),
            ("fk2", CatalogNodeKind::Constraint, Some("b"), "users_teams"),
        ] {
            graph.nodes.push(CatalogNode {
                id: CatalogObjectId(id.into()),
                native_id: None,
                kind,
                name: name.into(),
                qualified_name: format!("db.public.{name}"),
                parent_id: parent.map(|id| CatalogObjectId(id.into())),
                ordinal: None,
                definition_digest: None,
                completeness: CatalogCompleteness::Complete,
                details: CatalogNodeDetails::None,
                extra: Default::default(),
            });
        }
        for (from, to, pairs) in [
            ("fk1", "b", vec![("a1", "b1"), ("a2", "b2")]),
            ("fk2", "c", vec![("b1", "c1")]),
        ] {
            graph.edges.push(CatalogEdge {
                from: CatalogObjectId(from.into()),
                to: Some(CatalogObjectId(to.into())),
                kind: CatalogEdgeKind::ForeignKey,
                certainty: CatalogEdgeCertainty::CatalogProven,
                referenced_path: None,
                column_pairs: pairs
                    .into_iter()
                    .map(|(a, b)| CatalogColumnPair {
                        from: CatalogObjectId(a.into()),
                        to: CatalogObjectId(b.into()),
                    })
                    .collect(),
            });
        }
        graph
    }

    fn suggestions(
        sql: &str,
        graph: &CatalogGraphData,
        engine: Engine,
    ) -> Vec<CompletionCandidate> {
        join_candidates(
            &crate::detect_context(sql, sql.len(), engine),
            graph,
            engine,
            50,
        )
    }

    #[test]
    fn direct_and_multi_hop_preserve_aliases_composite_keys_and_prefix() {
        let graph = graph();
        let candidates = suggestions(
            "SELECT * FROM public.orders o JOIN ",
            &graph,
            Engine::Postgres,
        );
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].kind, CompletionKind::Table);
        assert_eq!(candidates[0].insert, "\"public\".\"users\" AS sift_join_1 ON \"o\".\"user_id\" = sift_join_1.\"id\" AND \"o\".\"tenant\" = sift_join_1.\"tenant\"");
        assert!(candidates[1].insert.contains(
            " JOIN \"public\".\"teams\" AS sift_join_2 ON sift_join_1.\"id\" = sift_join_2.\"id\""
        ));
        assert_eq!(
            suggestions("SELECT * FROM orders o JOIN tea", &graph, Engine::Postgres).len(),
            1
        );
        let reverse = suggestions("SELECT * FROM teams t JOIN ", &graph, Engine::SqlServer);
        assert!(reverse[0].insert.contains("[t].[id] = sift_join_1.[id]"));
    }

    #[test]
    fn excludes_unproven_edges_wrong_slots_and_previous_statements() {
        let mut graph = graph();
        let upper = suggestions("SELECT * FROM orders O JOIN ", &graph, Engine::Postgres);
        assert!(upper[0].insert.contains("\"o\".\"user_id\""));
        assert_eq!(
            suggestions(
                "SELECT * FROM orders O LEFT JOIN ",
                &graph,
                Engine::Postgres
            )
            .len(),
            1
        );
        assert!(suggestions(
            "SELECT * FROM orders o CROSS JOIN ",
            &graph,
            Engine::Postgres
        )
        .is_empty());
        assert!(suggestions(
            "SELECT * FROM orders; SELECT * FROM ",
            &graph,
            Engine::Postgres
        )
        .is_empty());
        assert!(suggestions(
            "SELECT * FROM orders; SELECT * FROM missing JOIN ",
            &graph,
            Engine::Postgres
        )
        .is_empty());
        graph.edges[0].certainty = CatalogEdgeCertainty::Parsed;
        assert!(suggestions("SELECT * FROM orders JOIN ", &graph, Engine::Postgres).is_empty());
    }
}
