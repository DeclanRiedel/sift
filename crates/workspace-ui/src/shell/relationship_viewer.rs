//! Scoped, read-only relationship browser. Catalog truth remains server-owned.

use super::*;

#[derive(Debug)]
pub(super) struct RelationshipViewerState {
    pub source: DatabaseObjectSource,
    pub depth: u8,
    pub request_id: u64,
    pub loading: bool,
    pub error: Option<String>,
    pub diagram: Option<Box<sift_protocol::CatalogDiagram>>,
    pub selected: Option<sift_protocol::CatalogObjectId>,
    pub anchor: Option<sift_protocol::CatalogObjectId>,
    pub incoming: bool,
    pub outgoing: bool,
    pub search: String,
    pub search_input: Entity<TextInput>,
    pub available_tables: Vec<DatabaseObjectSource>,
    pub scope_picker_open: bool,
    pub zoom: f32,
    pub scroll: ScrollHandle,
}

impl RelationshipViewerState {
    pub fn new(source: DatabaseObjectSource, search_input: Entity<TextInput>) -> Self {
        Self {
            source,
            depth: 1,
            request_id: 0,
            loading: false,
            error: None,
            diagram: None,
            selected: None,
            anchor: None,
            incoming: true,
            outgoing: true,
            search: String::new(),
            search_input,
            available_tables: Vec::new(),
            scope_picker_open: false,
            zoom: 1.0,
            scroll: ScrollHandle::new(),
        }
    }

    pub fn start(&mut self) -> u64 {
        self.request_id = self.request_id.wrapping_add(1);
        self.loading = true;
        self.error = None;
        self.request_id
    }

    pub fn finish(
        &mut self,
        request_id: u64,
        result: Result<
            (
                sift_protocol::CatalogObjectId,
                Box<sift_protocol::CatalogDiagram>,
                Vec<DatabaseObjectSource>,
            ),
            String,
        >,
    ) {
        if self.request_id != request_id {
            return;
        }
        self.loading = false;
        match result {
            Ok((anchor, diagram, available_tables)) => {
                if self
                    .selected
                    .as_ref()
                    .is_none_or(|selected| !diagram.nodes.iter().any(|node| &node.id == selected))
                {
                    self.selected = Some(anchor.clone());
                }
                self.anchor = Some(anchor);
                self.diagram = Some(diagram);
                self.available_tables = available_tables;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}

fn table_like(kind: sift_protocol::CatalogNodeKind) -> bool {
    matches!(
        kind,
        sift_protocol::CatalogNodeKind::Table
            | sift_protocol::CatalogNodeKind::PartitionedTable
            | sift_protocol::CatalogNodeKind::ForeignTable
            | sift_protocol::CatalogNodeKind::View
            | sift_protocol::CatalogNodeKind::MaterializedView
    )
}

fn owner<'a>(
    nodes: &'a HashMap<sift_protocol::CatalogObjectId, &'a sift_protocol::CatalogNode>,
    id: &sift_protocol::CatalogObjectId,
) -> Option<&'a sift_protocol::CatalogNode> {
    let mut node = nodes.get(id).copied()?;
    while !table_like(node.kind) {
        node = nodes.get(node.parent_id.as_ref()?).copied()?;
    }
    Some(node)
}

fn pair_label(
    nodes: &HashMap<sift_protocol::CatalogObjectId, &sift_protocol::CatalogNode>,
    edge: &sift_protocol::CatalogEdge,
) -> String {
    if edge.column_pairs.is_empty() {
        return "Column mapping unavailable".into();
    }
    edge.column_pairs
        .iter()
        .map(|pair| {
            let from = nodes.get(&pair.from).map_or("?", |node| node.name.as_str());
            let to = nodes.get(&pair.to).map_or("?", |node| node.name.as_str());
            format!("{from} → {to}")
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn source_for_table(
    diagram: &sift_protocol::CatalogDiagram,
    base: &DatabaseObjectSource,
    table_id: &sift_protocol::CatalogObjectId,
) -> Option<DatabaseObjectSource> {
    let nodes = diagram
        .nodes
        .iter()
        .map(|node| (&node.id, node))
        .collect::<HashMap<_, _>>();
    let table = nodes.get(table_id)?;
    let kind = match table.kind {
        sift_protocol::CatalogNodeKind::Table => sift_protocol::ObjectKind::Table,
        sift_protocol::CatalogNodeKind::PartitionedTable => {
            sift_protocol::ObjectKind::PartitionedTable
        }
        sift_protocol::CatalogNodeKind::ForeignTable => sift_protocol::ObjectKind::ForeignTable,
        sift_protocol::CatalogNodeKind::View => sift_protocol::ObjectKind::View,
        sift_protocol::CatalogNodeKind::MaterializedView => {
            sift_protocol::ObjectKind::MaterializedView
        }
        _ => return None,
    };
    let schema = nodes.get(table.parent_id.as_ref()?)?;
    let catalog = schema.parent_id.as_ref().and_then(|id| nodes.get(id));
    Some(DatabaseObjectSource {
        catalog: catalog.map(|node| node.name.clone()),
        schema: schema.name.clone(),
        object: table.name.clone(),
        object_kind: kind,
        ..base.clone()
    })
}

fn diagram_mermaid(diagram: &sift_protocol::CatalogDiagram) -> String {
    let ids = diagram
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.clone(), format!("n{index}")))
        .collect::<HashMap<_, _>>();
    let mut output = String::from("flowchart LR\n");
    if diagram.partial {
        output.push_str(
            "  %% Partial catalog projection; omitted objects or relationships may exist.\n",
        );
    }
    for (index, node) in diagram.nodes.iter().enumerate() {
        if !table_like(node.kind) {
            continue;
        }
        let label = node
            .qualified_name
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace(['\r', '\n'], " ");
        output.push_str(&format!("  n{index}[\"{label}\"]\n"));
    }
    let nodes = diagram
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect::<HashMap<_, _>>();
    for edge in diagram
        .edges
        .iter()
        .filter(|edge| edge.kind == sift_protocol::CatalogEdgeKind::ForeignKey)
    {
        let Some(from) = owner(&nodes, &edge.from).and_then(|node| ids.get(&node.id)) else {
            continue;
        };
        let Some(to) = edge
            .to
            .as_ref()
            .and_then(|id| owner(&nodes, id))
            .and_then(|node| ids.get(&node.id))
        else {
            continue;
        };
        output.push_str(&format!("  {from} -->|FK| {to}\n"));
    }
    output
}

impl Pane {
    pub(super) fn open_relationship_viewer(
        &mut self,
        item: ItemPresentation,
        source: DatabaseObjectSource,
        cx: &mut Context<Self>,
    ) -> u64 {
        if let Some(index) = self.items.iter().position(|candidate| {
            self.relationship_viewers
                .get(&candidate.id)
                .is_some_and(|viewer| {
                    viewer.source.instance_id == source.instance_id
                        && viewer.source.tenant_id == source.tenant_id
                        && viewer.source.profile_id == source.profile_id
                        && viewer.source.catalog == source.catalog
                        && viewer.source.schema == source.schema
                        && viewer.source.object == source.object
                })
        }) {
            let item_id = self.items[index].id;
            self.activate_item(index, true);
            cx.notify();
            return item_id;
        }
        if !self.replace_new_pane_placeholder() {
            if let Some(current) = self.active_item().map(|active| active.id) {
                self.backward_items.push(current);
                self.forward_items.clear();
            }
        }
        let item_id = item.id;
        self.items.insert(0, item);
        self.active_item = 0;
        let search_input = cx.new(|cx| TextInput::new("", "Search tables…", cx));
        self.relationship_viewers
            .insert(item_id, RelationshipViewerState::new(source, search_input));
        self.subscribe_relationship_viewer_search(item_id, cx);
        cx.notify();
        item_id
    }

    pub(super) fn subscribe_relationship_viewer_search(
        &mut self,
        item_id: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(viewer) = self.relationship_viewers.get(&item_id) else {
            return;
        };
        let search_input = viewer.search_input.clone();
        let subscription = cx.subscribe(
            &search_input,
            move |pane, input, event: &TextInputEvent, cx| {
                if *event == TextInputEvent::Changed {
                    if let Some(viewer) = pane.relationship_viewers.get_mut(&item_id) {
                        viewer.search = input.read(cx).text().to_lowercase();
                        cx.notify();
                    }
                }
            },
        );
        self.editor_subscriptions.insert(item_id, subscription);
    }

    pub(super) fn render_relationship_viewer(
        &self,
        item_id: u64,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let colors = cx.theme().colors;
        let Some(viewer) = self.relationship_viewers.get(&item_id) else {
            return div()
                .child("Relationship viewer unavailable")
                .into_any_element();
        };
        let mut table_cards = Vec::new();
        let mut link_rows = Vec::new();
        let mut relation_rows = Vec::new();
        let mut detail_rows = Vec::new();
        let mut auxiliary_rows = Vec::new();
        let mut scene_edges = Vec::new();
        let mut scene_height = 240.0_f32;
        let zoom = viewer.zoom;
        let mut summary = if viewer.loading {
            "Loading relationships…".to_owned()
        } else {
            "No catalog data".to_owned()
        };
        if let Some(diagram) = viewer.diagram.as_ref() {
            let nodes = diagram
                .nodes
                .iter()
                .map(|node| (node.id.clone(), node))
                .collect::<HashMap<_, _>>();
            let selected = viewer.selected.as_ref();
            let selected_table = selected.and_then(|id| nodes.get(id).copied());
            let fk_edges = diagram
                .edges
                .iter()
                .filter(|edge| edge.kind == sift_protocol::CatalogEdgeKind::ForeignKey)
                .collect::<Vec<_>>();
            let mut incoming_ids = HashSet::new();
            let mut outgoing_ids = HashSet::new();
            if let Some(anchor) = viewer.anchor.as_ref() {
                for edge in &fk_edges {
                    let Some(from) = owner(&nodes, &edge.from) else {
                        continue;
                    };
                    let to = edge.to.as_ref().and_then(|id| owner(&nodes, id));
                    if &from.id == anchor {
                        if let Some(to) = to {
                            outgoing_ids.insert(to.id.clone());
                        }
                    }
                    if to.is_some_and(|to| &to.id == anchor) {
                        incoming_ids.insert(from.id.clone());
                    }
                }
            }
            let mut positions = HashMap::new();
            let mut lane_counts = [0_u32; 4];
            for table in diagram.nodes.iter().filter(|node| {
                table_like(node.kind) && node.qualified_name.to_lowercase().contains(&viewer.search)
            }) {
                let lane = if viewer.anchor.as_ref() == Some(&table.id) {
                    1
                } else if incoming_ids.contains(&table.id) {
                    0
                } else if outgoing_ids.contains(&table.id) {
                    2
                } else {
                    3
                };
                let position = (
                    20.0 + lane as f32 * 300.0,
                    28.0 + lane_counts[lane] as f32 * 172.0,
                );
                lane_counts[lane] += 1;
                positions.insert(table.id.clone(), position);
            }
            scene_height =
                (lane_counts.into_iter().max().unwrap_or(0) as f32 * 172.0 + 56.0).max(240.0);
            summary = format!(
                "{} tables · {} foreign keys · revision {}{}",
                diagram
                    .nodes
                    .iter()
                    .filter(|node| table_like(node.kind))
                    .count(),
                fk_edges.len(),
                diagram.catalog_revision.0,
                if diagram.partial { " · partial" } else { "" }
            );
            for table in diagram.nodes.iter().filter(|node| {
                table_like(node.kind) && node.qualified_name.to_lowercase().contains(&viewer.search)
            }) {
                let table_id = table.id.clone();
                let key_table_id = table_id.clone();
                let card_index = table_cards.len();
                let Some((x, y)) = positions.get(&table.id).copied() else {
                    continue;
                };
                let columns = diagram
                    .nodes
                    .iter()
                    .filter(|node| {
                        node.kind == sift_protocol::CatalogNodeKind::Column
                            && node.parent_id.as_ref() == Some(&table.id)
                    })
                    .take(7)
                    .map(|node| {
                        let label = match &node.details {
                            sift_protocol::CatalogNodeDetails::Column { column } => {
                                let key = if column.primary_key {
                                    "PK"
                                } else if fk_edges.iter().any(|edge| {
                                    edge.column_pairs.iter().any(|pair| pair.from == node.id)
                                }) {
                                    "FK"
                                } else {
                                    "  "
                                };
                                format!(
                                    "{key}  {}  {}",
                                    node.name,
                                    type_ref_label(&column.type_ref)
                                )
                            }
                            _ => node.name.clone(),
                        };
                        div()
                            .truncate()
                            .text_size(px(11. * zoom))
                            .text_color(colors.muted_text)
                            .child(label)
                    })
                    .collect::<Vec<_>>();
                table_cards.push(
                    div()
                        .id(("relationship-card", card_index))
                        .debug_selector(move || format!("relationship-card-{card_index}"))
                        .absolute()
                        .left(px(x * zoom))
                        .top(px(y * zoom))
                        .w(px(238. * zoom))
                        .h(px(145. * zoom))
                        .p(px(12. * zoom))
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(px(4. * zoom))
                        .text_size(px(12. * zoom))
                        .rounded_sm()
                        .border_1()
                        .border_color(if selected == Some(&table.id) {
                            colors.accent
                        } else {
                            colors.subtle_border
                        })
                        .bg(if selected == Some(&table.id) {
                            colors.active_surface
                        } else {
                            colors.elevated_surface
                        })
                        .cursor_pointer()
                        .tab_index(0)
                        .role(Role::Button)
                        .aria_label(format!("Inspect table {}", table.qualified_name))
                        .focus(|card| card.bg(colors.hovered_surface))
                        .hover(|card| card.bg(colors.hovered_surface))
                        .on_key_down(cx.listener(move |pane, event: &gpui::KeyDownEvent, _, cx| {
                            if event.keystroke.modifiers.modified() {
                                return;
                            }
                            if let Some(viewer) = pane.relationship_viewers.get_mut(&item_id) {
                                match event.keystroke.key.as_str() {
                                    "enter" | "space" => {
                                        viewer.selected = Some(key_table_id.clone())
                                    }
                                    "j" | "k" => {
                                        if let Some(diagram) = viewer.diagram.as_ref() {
                                            let ids = diagram
                                                .nodes
                                                .iter()
                                                .filter(|node| {
                                                    table_like(node.kind)
                                                        && node
                                                            .qualified_name
                                                            .to_lowercase()
                                                            .contains(&viewer.search)
                                                })
                                                .map(|node| node.id.clone())
                                                .collect::<Vec<_>>();
                                            if !ids.is_empty() {
                                                let current = viewer
                                                    .selected
                                                    .as_ref()
                                                    .and_then(|id| {
                                                        ids.iter()
                                                            .position(|candidate| candidate == id)
                                                    })
                                                    .unwrap_or(0);
                                                let next = if event.keystroke.key == "j" {
                                                    (current + 1).min(ids.len() - 1)
                                                } else {
                                                    current.saturating_sub(1)
                                                };
                                                viewer.selected = Some(ids[next].clone());
                                            }
                                        }
                                    }
                                    _ => return,
                                }
                                cx.notify();
                            }
                            cx.stop_propagation();
                        }))
                        .on_click(cx.listener(move |pane, _, _, cx| {
                            if let Some(viewer) = pane.relationship_viewers.get_mut(&item_id) {
                                viewer.selected = Some(table_id.clone());
                                cx.notify();
                            }
                        }))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .truncate()
                                .child(table.qualified_name.clone()),
                        )
                        .children(columns),
                );
            }
            for (index, edge) in fk_edges.iter().enumerate() {
                let Some(from) = owner(&nodes, &edge.from) else {
                    continue;
                };
                let to = edge.to.as_ref().and_then(|id| owner(&nodes, id));
                if let Some(to) = to {
                    if let (Some(&from_position), Some(&to_position)) =
                        (positions.get(&from.id), positions.get(&to.id))
                    {
                        scene_edges.push((from_position, to_position));
                    }
                }
                if selected.is_some_and(|id| {
                    !(viewer.outgoing && &from.id == id
                        || viewer.incoming && to.is_some_and(|table| &table.id == id))
                }) {
                    continue;
                }
                let target_name =
                    if edge.certainty == sift_protocol::CatalogEdgeCertainty::Inaccessible {
                        "Restricted target"
                    } else {
                        to.map(|table| table.qualified_name.as_str())
                            .or(edge.referenced_path.as_deref())
                            .unwrap_or("Unresolved target")
                    };
                let mapping = pair_label(&nodes, edge);
                let constraint = nodes
                    .get(&edge.from)
                    .map_or("Foreign key", |node| node.name.as_str());
                link_rows.push(
                    div()
                        .id(("relationship-link", index))
                        .w_full()
                        .min_h(px(56.))
                        .px_3()
                        .py_2()
                        .flex()
                        .items_center()
                        .gap_3()
                        .rounded_sm()
                        .border_1()
                        .border_color(colors.subtle_border)
                        .bg(colors.elevated_surface)
                        .child(
                            div()
                                .w(px(180.))
                                .flex_none()
                                .truncate()
                                .text_sm()
                                .child(from.qualified_name.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .items_center()
                                .child(div().w_full().h(px(1.)).bg(colors.accent))
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child(format!("{constraint} · {mapping}")),
                                ),
                        )
                        .child(
                            div()
                                .w(px(180.))
                                .flex_none()
                                .truncate()
                                .text_sm()
                                .child(format!("→ {target_name}")),
                        ),
                );
            }
            if let Some(table) = selected_table {
                for column in diagram.nodes.iter().filter(|node| {
                    node.kind == sift_protocol::CatalogNodeKind::Column
                        && node.parent_id.as_ref() == Some(&table.id)
                }) {
                    let label = match &column.details {
                        sift_protocol::CatalogNodeDetails::Column { column: metadata } => format!(
                            "{}  {}  {}{}",
                            column.name,
                            type_ref_label(&metadata.type_ref),
                            if metadata.nullable == sift_protocol::Nullability::Unknown {
                                "nullable ?"
                            } else if metadata.nullable == sift_protocol::Nullability::Nullable {
                                "nullable"
                            } else {
                                "required"
                            },
                            if metadata.primary_key { " · PK" } else { "" },
                        ),
                        _ => column.name.clone(),
                    };
                    detail_rows.push(
                        div()
                            .py_1()
                            .border_b_1()
                            .border_color(colors.subtle_border)
                            .text_size(px(11. * zoom))
                            .child(label),
                    );
                }
                for node in diagram
                    .nodes
                    .iter()
                    .filter(|node| node.parent_id.as_ref() == Some(&table.id))
                {
                    let label = match &node.details {
                        sift_protocol::CatalogNodeDetails::Index { index } => {
                            format!(
                                "INDEX  {}  {}{}",
                                node.name,
                                index.columns.join(", "),
                                if index.unique { " · unique" } else { "" }
                            )
                        }
                        sift_protocol::CatalogNodeDetails::Constraint { constraint } => {
                            format!(
                                "{:?}  {}  {}",
                                constraint.kind,
                                node.name,
                                constraint.columns.join(", ")
                            )
                        }
                        sift_protocol::CatalogNodeDetails::Trigger { trigger } => {
                            format!("TRIGGER  {}  {:?}", node.name, trigger.timing)
                        }
                        _ => continue,
                    };
                    auxiliary_rows.push(
                        div()
                            .py_1()
                            .border_b_1()
                            .border_color(colors.subtle_border)
                            .text_xs()
                            .child(label),
                    );
                }
                for edge in fk_edges {
                    let Some(from) = owner(&nodes, &edge.from) else {
                        continue;
                    };
                    let to = edge.to.as_ref().and_then(|id| owner(&nodes, id));
                    let is_outgoing = from.id == table.id;
                    let is_incoming = to.is_some_and(|to| to.id == table.id);
                    if !(is_outgoing && viewer.outgoing || is_incoming && viewer.incoming) {
                        continue;
                    }
                    let target =
                        if edge.certainty == sift_protocol::CatalogEdgeCertainty::Inaccessible {
                            "Restricted target"
                        } else if is_outgoing {
                            to.map(|to| to.qualified_name.as_str())
                                .or(edge.referenced_path.as_deref())
                                .unwrap_or("Unavailable target")
                        } else {
                            from.qualified_name.as_str()
                        };
                    relation_rows.push(
                        div()
                            .p_2()
                            .border_b_1()
                            .border_color(colors.subtle_border)
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_sm().child(format!(
                                "{}  {}",
                                if is_outgoing {
                                    "→ References"
                                } else {
                                    "← Referenced by"
                                },
                                target
                            )))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .child(pair_label(&nodes, edge)),
                            ),
                    );
                }
            }
        }
        let depth = viewer.depth;
        let loading = viewer.loading;
        let incoming = viewer.incoming;
        let outgoing = viewer.outgoing;
        let selected_source = viewer.diagram.as_ref().and_then(|diagram| {
            viewer
                .selected
                .as_ref()
                .and_then(|id| source_for_table(diagram, &viewer.source, id))
        });
        let can_focus = selected_source.as_ref().is_some_and(|source| {
            source.catalog != viewer.source.catalog
                || source.schema != viewer.source.schema
                || source.object != viewer.source.object
        });
        let selected_title = viewer
            .diagram
            .as_ref()
            .and_then(|diagram| {
                viewer
                    .selected
                    .as_ref()
                    .and_then(|id| diagram.nodes.iter().find(|node| &node.id == id))
                    .map(|node| node.qualified_name.clone())
            })
            .unwrap_or_else(|| viewer.source.object.clone());
        let scope_choices = if viewer.scope_picker_open {
            viewer
                .available_tables
                .iter()
                .filter(|source| {
                    viewer.search.is_empty()
                        || format!(
                            "{}.{}.{}",
                            source.catalog.as_deref().unwrap_or(""),
                            source.schema,
                            source.object
                        )
                        .to_lowercase()
                        .contains(&viewer.search)
                })
                .take(40)
                .enumerate()
                .map(|(index, source)| {
                    let source = source.clone();
                    let label = format!("{}.{}", source.schema, source.object);
                    div()
                        .id(("relationship-scope-choice", index))
                        .h(px(28.))
                        .px_3()
                        .flex()
                        .items_center()
                        .text_sm()
                        .cursor_pointer()
                        .hover(|row| row.bg(colors.hovered_surface))
                        .on_click(cx.listener(move |pane, _, _, cx| {
                            if let Some(viewer) = pane.relationship_viewers.get_mut(&item_id) {
                                viewer.source = source.clone();
                                viewer.diagram = None;
                                viewer.selected = None;
                                viewer.anchor = None;
                                viewer.scope_picker_open = false;
                                cx.emit(PaneEvent::RelationshipViewerRefreshRequested { item_id });
                                cx.notify();
                            }
                        }))
                        .child(label)
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let edge_layer = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let mut path = gpui::PathBuilder::stroke(px(1.5));
                for ((from_x, from_y), (to_x, to_y)) in &scene_edges {
                    let (start_x, end_x) = if from_x < to_x {
                        (from_x + 238.0, *to_x)
                    } else if from_x > to_x {
                        (*from_x, to_x + 238.0)
                    } else {
                        (from_x + 238.0, to_x + 238.0)
                    };
                    let start =
                        bounds.origin + gpui::point(px(start_x * zoom), px((from_y + 68.0) * zoom));
                    let end =
                        bounds.origin + gpui::point(px(end_x * zoom), px((to_y + 68.0) * zoom));
                    let mid_x = if from_x == to_x {
                        start.x + px(30. * zoom)
                    } else {
                        (start.x + end.x) / 2.0
                    };
                    path.move_to(start);
                    path.line_to(gpui::point(mid_x, start.y));
                    path.line_to(gpui::point(mid_x, end.y));
                    path.line_to(end);
                    let direction = if from_x < to_x { 1.0 } else { -1.0 };
                    path.move_to(end + gpui::point(px(-6.0 * direction * zoom), px(-4.0 * zoom)));
                    path.line_to(end);
                    path.line_to(end + gpui::point(px(-6.0 * direction * zoom), px(4.0 * zoom)));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, colors.accent);
                }
            },
        )
        .w(px(1220. * zoom))
        .h(px(scene_height * zoom));
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(66.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .bg(colors.toolbar)
                    .border_b_1()
                    .border_color(colors.subtle_border)
                    .child(
                        div()
                            .h(px(33.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(SectionLabel::new("RELATIONSHIPS"))
                            .child(Button::new("relationship-scope", "Choose table…")
                                .tone(ButtonTone::Ghost)
                                .on_click(cx.listener(move |pane, _, _, cx| {
                                    if let Some(viewer) = pane.relationship_viewers.get_mut(&item_id) {
                                        viewer.scope_picker_open = !viewer.scope_picker_open;
                                        cx.notify();
                                    }
                                })))
                            .child(div().text_xs().truncate().child(format!(
                                "{}  ›  {}  ›  {}  ›  {}",
                                viewer.source.profile_name,
                                viewer.source.catalog.as_deref().unwrap_or("default"),
                                viewer.source.schema,
                                viewer.source.object
                            )))
                            .child(div().flex_1())
                            .child(div().text_xs().text_color(colors.muted_text).child(summary)),
                    )
                    .child(
                        div()
                            .id(("relationship-controls", item_id as usize))
                            .h(px(33.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_1()
                            .overflow_x_scroll()
                            .child(
                                Button::new("relationship-depth-less", "−")
                                    .tone(ButtonTone::Ghost)
                                    .disabled(depth <= 1 || loading)
                                    .on_click(cx.listener(move |pane, _, _, cx| {
                                        pane.change_relationship_depth(item_id, -1, cx)
                                    })),
                            )
                            .child(
                                div().text_xs().child(format!(
                                    "{depth} hop{}",
                                    if depth == 1 { "" } else { "s" }
                                )),
                            )
                            .child(
                                Button::new("relationship-depth-more", "+")
                                    .debug_selector("relationship-depth-more")
                                    .tone(ButtonTone::Ghost)
                                    .disabled(depth >= 3 || loading)
                                    .on_click(cx.listener(move |pane, _, _, cx| {
                                        pane.change_relationship_depth(item_id, 1, cx)
                                    })),
                            )
                            .child(
                                Button::new("relationship-incoming", "Referenced by")
                                    .tone(if incoming {
                                        ButtonTone::Accent
                                    } else {
                                        ButtonTone::Ghost
                                    })
                                    .on_click(cx.listener(move |pane, _, _, cx| {
                                        if let Some(viewer) =
                                            pane.relationship_viewers.get_mut(&item_id)
                                        {
                                            viewer.incoming = !viewer.incoming;
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(
                                Button::new("relationship-outgoing", "References")
                                    .tone(if outgoing {
                                        ButtonTone::Accent
                                    } else {
                                        ButtonTone::Ghost
                                    })
                                    .on_click(cx.listener(move |pane, _, _, cx| {
                                        if let Some(viewer) =
                                            pane.relationship_viewers.get_mut(&item_id)
                                        {
                                            viewer.outgoing = !viewer.outgoing;
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(div().w(px(180.)).flex_none().child(viewer.search_input.clone()))
                            .child(Button::new("relationship-zoom-out", "−").tone(ButtonTone::Ghost)
                                .disabled(zoom <= 0.5).on_click(cx.listener(move |pane, _, _, cx| pane.change_relationship_zoom(item_id, -0.15, cx))))
                            .child(div().text_xs().child(format!("{}%", (zoom * 100.0).round() as u32)))
                            .child(Button::new("relationship-zoom-in", "+").tone(ButtonTone::Ghost)
                                .disabled(zoom >= 1.5).on_click(cx.listener(move |pane, _, _, cx| pane.change_relationship_zoom(item_id, 0.15, cx))))
                            .child(Button::new("relationship-fit-width", "Fit width").tone(ButtonTone::Ghost)
                                .on_click(cx.listener(move |pane, _, _, cx| pane.fit_relationship_width(item_id, cx))))
                            .child(div().flex_1())
                            .child(Button::new("relationship-copy-mermaid", "Copy Mermaid")
                                .tone(ButtonTone::Ghost).disabled(viewer.diagram.is_none())
                                .on_click(cx.listener(move |pane, _, _, cx| {
                                    if let Some(diagram) = pane.relationship_viewers.get(&item_id).and_then(|viewer| viewer.diagram.as_deref()) {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(diagram_mermaid(diagram)));
                                    }
                                })))
                            .child(
                                Button::new("relationship-refresh", "Refresh")
                                    .tone(ButtonTone::Neutral)
                                    .loading(loading)
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(PaneEvent::RelationshipViewerRefreshRequested {
                                            item_id,
                                        })
                                    })),
                            ),
                    ),
            )
            .children(viewer.scope_picker_open.then(|| div()
                .id(("relationship-scope-choices", item_id as usize))
                .max_h(px(196.)).overflow_y_scroll().bg(colors.elevated_surface)
                .border_b_1().border_color(colors.subtle_border)
                .children(scope_choices)))
            .children(
                viewer
                    .error
                    .as_ref()
                    .map(|error| ErrorBanner::new(error.clone())),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .id(("relationship-cards", item_id as usize))
                            .flex_1()
                            .min_w_0()
                            .overflow_x_scroll()
                            .overflow_y_scroll()
                            .track_scroll(&viewer.scroll)
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(div().relative().w(px(1220. * zoom)).h(px(scene_height * zoom))
                                .child(edge_layer)
                                .children(table_cards))
                            .child(SectionLabel::new("FOREIGN KEYS"))
                            .children(link_rows),
                    )
                    .child(
                        div()
                            .id(("relationship-details", item_id as usize))
                            .w(px(330.))
                            .flex_none()
                            .min_h_0()
                            .overflow_y_scroll()
                            .border_l_1()
                            .border_color(colors.subtle_border)
                            .bg(colors.panel)
                            .child(
                                div()
                                    .p_3()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(selected_title),
                            )
                            .child(div().px_3().pb_2().flex().gap_1()
                                .children(selected_source.clone().map(|source| {
                                    Button::new("relationship-open-table", "Open table")
                                        .tone(ButtonTone::Neutral)
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.emit(PaneEvent::ObjectBrowserOpenRequested { source: source.clone() });
                                        }))
                                }))
                                .children(selected_source.filter(|_| can_focus).map(|source| {
                                    Button::new("relationship-focus-table", "Focus here")
                                        .tone(ButtonTone::Ghost)
                                        .on_click(cx.listener(move |pane, _, _, cx| {
                                            if let Some(viewer) = pane.relationship_viewers.get_mut(&item_id) {
                                                viewer.source = source.clone();
                                                viewer.diagram = None;
                                                viewer.selected = None;
                                                viewer.anchor = None;
                                                cx.emit(PaneEvent::RelationshipViewerRefreshRequested { item_id });
                                                cx.notify();
                                            }
                                        }))
                                })))
                            .child(
                                div()
                                    .px_3()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .child("COLUMNS"),
                            )
                            .child(div().px_3().children(detail_rows))
                            .child(
                                div()
                                    .p_3()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .child("RELATIONSHIPS"),
                            )
                            .child(div().children(relation_rows))
                            .child(div().p_3().text_xs().text_color(colors.muted_text).child("INDEXES · CONSTRAINTS · TRIGGERS"))
                            .child(div().px_3().children(auxiliary_rows)),
                    ),
            )
            .into_any_element()
    }

    fn change_relationship_depth(&mut self, item_id: u64, delta: i8, cx: &mut Context<Self>) {
        let Some(viewer) = self.relationship_viewers.get_mut(&item_id) else {
            return;
        };
        viewer.depth = (i16::from(viewer.depth) + i16::from(delta)).clamp(1, 3) as u8;
        cx.emit(PaneEvent::RelationshipViewerRefreshRequested { item_id });
        cx.notify();
    }

    fn change_relationship_zoom(&mut self, item_id: u64, delta: f32, cx: &mut Context<Self>) {
        if let Some(viewer) = self.relationship_viewers.get_mut(&item_id) {
            viewer.zoom = (viewer.zoom + delta).clamp(0.5, 1.5);
            cx.notify();
        }
    }

    fn fit_relationship_width(&mut self, item_id: u64, cx: &mut Context<Self>) {
        if let Some(viewer) = self.relationship_viewers.get_mut(&item_id) {
            let width = f32::from(viewer.scroll.bounds().size.width);
            viewer.zoom = (width / 1220.0).clamp(0.5, 1.0);
            cx.notify();
        }
    }
}
