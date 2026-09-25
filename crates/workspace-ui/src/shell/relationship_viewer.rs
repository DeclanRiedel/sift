//! Scoped, read-only relationship browser. Catalog truth remains server-owned.

use super::*;

const CARD_WIDTH: f32 = 284.0;
const CARD_HEIGHT: f32 = 206.0;
const CARD_HEADER_HEIGHT: f32 = 39.0;
const CARD_ROW_HEIGHT: f32 = 24.0;
const CARD_VERTICAL_STEP: f32 = 244.0;
const LANE_STEP: f32 = 368.0;
const SCENE_WIDTH: f32 = 1472.0;
const CARD_MIDPOINT_Y: f32 = CARD_HEIGHT / 2.0;
const RELATION_ROW_STEP: f32 = 56.0;

fn relationship_fit_zoom(width: f32, height: f32, scene_height: f32) -> f32 {
    let width_zoom = ((width - 32.0) / SCENE_WIDTH).max(0.0);
    let height_zoom = if height > 0.0 {
        ((height - 32.0) / scene_height).max(0.0)
    } else {
        1.0
    };
    width_zoom.min(height_zoom).clamp(0.2, 1.0)
}

#[derive(Debug, Clone)]
struct CachedCardBody {
    text_style: gpui::TextStyle,
    colors: ThemeColors,
    zoom: f32,
    row_count: usize,
    lines: Vec<(ShapedLine, gpui::Point<Pixels>, Pixels)>,
}

#[derive(Debug, Clone)]
struct CachedRelationRow {
    text_style: gpui::TextStyle,
    colors: ThemeColors,
    texts: [String; 4],
    lines: [ShapedLine; 4],
}

#[derive(Debug)]
pub(super) struct RelationshipViewerState {
    pub source: DatabaseObjectSource,
    pub depth: u8,
    pub request_id: u64,
    pub loading: bool,
    pub error: Option<String>,
    pub diagram: Option<Box<sift_protocol::CatalogDiagram>>,
    index: Option<RelationshipViewerIndex>,
    pub selected: Option<sift_protocol::CatalogObjectId>,
    pub anchor: Option<sift_protocol::CatalogObjectId>,
    pub incoming: bool,
    pub outgoing: bool,
    pub search: String,
    pub search_input: Entity<TextInput>,
    pub available_tables: Vec<DatabaseObjectSource>,
    pub scope_picker_open: bool,
    pub zoom: f32,
    details_open: bool,
    fit_active: bool,
    auto_fit_pending: bool,
    pub scroll: ScrollHandle,
    details_scroll: ScrollHandle,
    focus_handle: FocusHandle,
    card_text_cache: Rc<RefCell<HashMap<sift_protocol::CatalogObjectId, CachedCardBody>>>,
    relation_text_cache: Rc<RefCell<HashMap<usize, CachedRelationRow>>>,
}

impl RelationshipViewerState {
    pub fn new(
        source: DatabaseObjectSource,
        search_input: Entity<TextInput>,
        focus_handle: FocusHandle,
    ) -> Self {
        Self {
            source,
            depth: 1,
            request_id: 0,
            loading: false,
            error: None,
            diagram: None,
            index: None,
            selected: None,
            anchor: None,
            incoming: true,
            outgoing: true,
            search: String::new(),
            search_input,
            available_tables: Vec::new(),
            scope_picker_open: false,
            zoom: 1.0,
            details_open: false,
            fit_active: false,
            auto_fit_pending: true,
            scroll: ScrollHandle::new(),
            details_scroll: ScrollHandle::new(),
            focus_handle,
            card_text_cache: Rc::new(RefCell::new(HashMap::new())),
            relation_text_cache: Rc::new(RefCell::new(HashMap::new())),
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
                let index = RelationshipViewerIndex::new(&diagram, &self.search, &anchor);
                if self
                    .selected
                    .as_ref()
                    .is_none_or(|selected| !index.visible_positions.contains_key(selected))
                {
                    self.selected = if index.visible_positions.contains_key(&anchor) {
                        Some(anchor.clone())
                    } else {
                        index.visible_tables.first().cloned()
                    };
                }
                self.anchor = Some(anchor);
                self.diagram = Some(diagram);
                self.index = Some(index);
                self.card_text_cache.borrow_mut().clear();
                self.relation_text_cache.borrow_mut().clear();
                self.available_tables = available_tables;
                self.error = None;
                if self.auto_fit_pending {
                    let bounds = self.scroll.bounds().size;
                    let width = f32::from(bounds.width);
                    if width > 0.0 {
                        self.zoom = relationship_fit_zoom(
                            width,
                            f32::from(bounds.height),
                            self.index
                                .as_ref()
                                .map_or(280.0, |index| index.scene_height),
                        );
                        self.fit_active = true;
                        self.auto_fit_pending = false;
                    }
                }
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn move_selection(&mut self, forward: bool) -> bool {
        let Some(index) = self.index.as_ref() else {
            return false;
        };
        let ids = &index.visible_tables;
        if ids.is_empty() {
            return false;
        }
        let next = match self
            .selected
            .as_ref()
            .and_then(|id| index.visible_positions.get(id).copied())
        {
            Some(current) if forward => (current + 1).min(ids.len() - 1),
            Some(current) => current.saturating_sub(1),
            None => 0,
        };
        if self.selected.as_ref() == Some(&ids[next]) {
            return false;
        }
        self.selected = Some(ids[next].clone());
        self.details_scroll.set_offset(gpui::point(px(0.), px(0.)));
        self.reveal_selection();
        true
    }

    fn reveal_selection(&self) {
        let Some((x, y)) = self
            .index
            .as_ref()
            .and_then(|index| {
                self.selected
                    .as_ref()
                    .and_then(|id| index.positions.get(id))
            })
            .copied()
        else {
            return;
        };
        let viewport = self.scroll.bounds().size;
        if viewport.width <= px(0.) || viewport.height <= px(0.) {
            return;
        }
        let mut offset = self.scroll.offset();
        let margin = 24.0;
        let x = x * self.zoom + 12.0;
        let y = y * self.zoom + 12.0;
        let right = x + CARD_WIDTH * self.zoom;
        let bottom = y + CARD_HEIGHT * self.zoom;
        let left_visible = -f32::from(offset.x);
        let top_visible = -f32::from(offset.y);
        let width = f32::from(viewport.width);
        let height = f32::from(viewport.height);
        let next_x = if x < left_visible + margin {
            (x - margin).max(0.0)
        } else if right > left_visible + width - margin {
            (right + margin - width).max(0.0)
        } else {
            left_visible
        };
        let next_y = if y < top_visible + margin {
            (y - margin).max(0.0)
        } else if bottom > top_visible + height - margin {
            (bottom + margin - height).max(0.0)
        } else {
            top_visible
        };
        offset.x = px(-next_x).max(-self.scroll.max_offset().x);
        offset.y = px(-next_y).max(-self.scroll.max_offset().y);
        self.scroll.set_offset(offset);
    }
}

#[derive(Debug)]
struct RelationshipViewerIndex {
    nodes: HashMap<sift_protocol::CatalogObjectId, usize>,
    columns_by_table: HashMap<sift_protocol::CatalogObjectId, Vec<usize>>,
    children_by_table: HashMap<sift_protocol::CatalogObjectId, Vec<usize>>,
    table_search: Vec<(sift_protocol::CatalogObjectId, String)>,
    visible_tables: Vec<sift_protocol::CatalogObjectId>,
    visible_positions: HashMap<sift_protocol::CatalogObjectId, usize>,
    fk_edges: Vec<usize>,
    fk_columns: HashSet<sift_protocol::CatalogObjectId>,
    edge_cardinalities: Vec<(Cardinality, Cardinality)>,
    anchor: sift_protocol::CatalogObjectId,
    incoming_ids: HashSet<sift_protocol::CatalogObjectId>,
    outgoing_ids: HashSet<sift_protocol::CatalogObjectId>,
    positions: HashMap<sift_protocol::CatalogObjectId, (f32, f32)>,
    scene_height: f32,
}

impl RelationshipViewerIndex {
    fn new(
        diagram: &sift_protocol::CatalogDiagram,
        search: &str,
        anchor: &sift_protocol::CatalogObjectId,
    ) -> Self {
        let mut nodes = HashMap::with_capacity(diagram.nodes.len());
        let mut columns_by_table = HashMap::<_, Vec<_>>::new();
        let mut children_by_table = HashMap::<_, Vec<_>>::new();
        let mut table_search = Vec::new();
        for (index, node) in diagram.nodes.iter().enumerate() {
            nodes.insert(node.id.clone(), index);
            if let Some(parent) = &node.parent_id {
                children_by_table
                    .entry(parent.clone())
                    .or_default()
                    .push(index);
            }
            if table_like(node.kind) {
                table_search.push((node.id.clone(), node.qualified_name.to_lowercase()));
            } else if node.kind == sift_protocol::CatalogNodeKind::Column {
                if let Some(parent) = &node.parent_id {
                    columns_by_table
                        .entry(parent.clone())
                        .or_default()
                        .push(index);
                }
            }
        }
        let node_refs = diagram
            .nodes
            .iter()
            .map(|node| (&node.id, node))
            .collect::<HashMap<_, _>>();
        let mut fk_edges = Vec::new();
        let mut fk_columns = HashSet::new();
        let mut edge_cardinalities = Vec::new();
        let mut incoming_ids = HashSet::new();
        let mut outgoing_ids = HashSet::new();
        for (index, edge) in diagram.edges.iter().enumerate() {
            if edge.kind == sift_protocol::CatalogEdgeKind::ForeignKey {
                fk_edges.push(index);
                fk_columns.extend(edge.column_pairs.iter().map(|pair| pair.from.clone()));
                edge_cardinalities.push(fk_cardinality(&node_refs, edge));
                if let Some(from) = owner(&node_refs, &edge.from) {
                    let to = edge.to.as_ref().and_then(|id| owner(&node_refs, id));
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
        }
        let mut result = Self {
            nodes,
            columns_by_table,
            children_by_table,
            table_search,
            visible_tables: Vec::new(),
            visible_positions: HashMap::new(),
            fk_edges,
            fk_columns,
            edge_cardinalities,
            anchor: anchor.clone(),
            incoming_ids,
            outgoing_ids,
            positions: HashMap::new(),
            scene_height: 280.0,
        };
        result.filter(search);
        result
    }

    fn filter(&mut self, search: &str) {
        self.visible_tables.clear();
        self.visible_positions.clear();
        self.positions.clear();
        let mut lane_counts = [0_u32; 4];
        self.visible_tables.extend(
            self.table_search
                .iter()
                .filter(|(_, name)| name.contains(search))
                .map(|(id, _)| id.clone()),
        );
        for (position, id) in self.visible_tables.iter().enumerate() {
            self.visible_positions.insert(id.clone(), position);
        }
        for id in &self.visible_tables {
            let lane = if id == &self.anchor {
                1
            } else if self.incoming_ids.contains(id) {
                0
            } else if self.outgoing_ids.contains(id) {
                2
            } else {
                3
            };
            self.positions.insert(
                id.clone(),
                (
                    24.0 + lane as f32 * LANE_STEP,
                    30.0 + lane_counts[lane] as f32 * CARD_VERTICAL_STEP,
                ),
            );
            lane_counts[lane] += 1;
        }
        self.scene_height =
            (lane_counts.into_iter().max().unwrap_or(0) as f32 * CARD_VERTICAL_STEP + 42.0)
                .max(280.0);
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

type NodeRefs<'a> = HashMap<&'a sift_protocol::CatalogObjectId, &'a sift_protocol::CatalogNode>;

fn owner<'a>(
    nodes: &NodeRefs<'a>,
    id: &sift_protocol::CatalogObjectId,
) -> Option<&'a sift_protocol::CatalogNode> {
    let mut node = nodes.get(id).copied()?;
    while !table_like(node.kind) {
        node = nodes.get(node.parent_id.as_ref()?).copied()?;
    }
    Some(node)
}

fn pair_label(nodes: &NodeRefs<'_>, edge: &sift_protocol::CatalogEdge) -> String {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cardinality {
    One,
    ZeroOrOne,
    ZeroOrMany,
    Unknown,
}

fn fk_cardinality(
    nodes: &NodeRefs<'_>,
    edge: &sift_protocol::CatalogEdge,
) -> (Cardinality, Cardinality) {
    if edge.column_pairs.is_empty()
        || edge.certainty != sift_protocol::CatalogEdgeCertainty::CatalogProven
    {
        return (Cardinality::Unknown, Cardinality::Unknown);
    }
    let columns = edge
        .column_pairs
        .iter()
        .filter_map(|pair| nodes.get(&pair.from))
        .collect::<Vec<_>>();
    if columns.len() != edge.column_pairs.len() {
        return (Cardinality::Unknown, Cardinality::Unknown);
    }
    let parent = if columns.iter().all(|node| matches!(&node.details, sift_protocol::CatalogNodeDetails::Column { column } if column.nullable == sift_protocol::Nullability::NotNullable)) {
        Cardinality::One
    } else if columns.iter().any(|node| matches!(&node.details, sift_protocol::CatalogNodeDetails::Column { column } if column.nullable == sift_protocol::Nullability::Nullable)) {
        Cardinality::ZeroOrOne
    } else {
        Cardinality::Unknown
    };
    let Some(table) = owner(nodes, &edge.from) else {
        return (Cardinality::Unknown, parent);
    };
    let fk_names = columns
        .iter()
        .map(|node| node.name.as_str())
        .collect::<HashSet<_>>();
    let unique = nodes.values().any(|node| {
        if node.parent_id.as_ref() != Some(&table.id) {
            return false;
        }
        let names: &[String] = match &node.details {
            sift_protocol::CatalogNodeDetails::Index { index }
                if index.unique && index.partial_predicate.is_none() =>
            {
                &index.columns
            }
            sift_protocol::CatalogNodeDetails::Constraint { constraint }
                if matches!(
                    constraint.kind,
                    sift_protocol::ConstraintKind::PrimaryKey
                        | sift_protocol::ConstraintKind::Unique
                ) =>
            {
                &constraint.columns
            }
            _ => return false,
        };
        names.len() == fk_names.len() && names.iter().all(|name| fk_names.contains(name.as_str()))
    });
    (
        if unique {
            Cardinality::ZeroOrOne
        } else {
            Cardinality::ZeroOrMany
        },
        parent,
    )
}

fn cardinality_label(value: Cardinality) -> &'static str {
    match value {
        Cardinality::One => "1",
        Cardinality::ZeroOrOne => "0..1",
        Cardinality::ZeroOrMany => "0..*",
        Cardinality::Unknown => "?",
    }
}

fn cardinality_wire_offset(value: Cardinality) -> f32 {
    match value {
        Cardinality::One => 15.0,
        Cardinality::ZeroOrOne | Cardinality::ZeroOrMany => 26.0,
        Cardinality::Unknown => 0.0,
    }
}

fn scroll_needs_rebuild(
    current: gpui::Point<gpui::Pixels>,
    rendered: gpui::Point<gpui::Pixels>,
    threshold: f32,
) -> bool {
    f32::from(current.x - rendered.x).abs() >= threshold
        || f32::from(current.y - rendered.y).abs() >= threshold
}

fn draw_relationship_wire(
    path: &mut gpui::PathBuilder,
    start: gpui::Point<gpui::Pixels>,
    end: gpui::Point<gpui::Pixels>,
    lane_x: gpui::Pixels,
) {
    let first_dx = f32::from(lane_x - start.x);
    let last_dx = f32::from(end.x - lane_x);
    let dy = f32::from(end.y - start.y);
    path.move_to(start);
    if dy.abs() < 2.0 {
        if f32::from(start.x - end.x).abs() < 2.0 {
            path.cubic_bezier_to(
                end,
                gpui::point(lane_x, start.y - px(24.)),
                gpui::point(lane_x, end.y + px(24.)),
            );
        } else {
            path.line_to(end);
        }
        return;
    }
    let radius = 13.0_f32
        .min(first_dx.abs() / 2.0)
        .min(last_dx.abs() / 2.0)
        .min(dy.abs() / 2.0);
    if radius < 2.0 {
        path.cubic_bezier_to(
            end,
            gpui::point(lane_x, start.y),
            gpui::point(lane_x, end.y),
        );
        return;
    }
    let corner = px(radius);
    path.line_to(gpui::point(lane_x - corner * first_dx.signum(), start.y));
    path.curve_to(
        gpui::point(lane_x, start.y + corner * dy.signum()),
        gpui::point(lane_x, start.y),
    );
    path.line_to(gpui::point(lane_x, end.y - corner * dy.signum()));
    path.curve_to(
        gpui::point(lane_x + corner * last_dx.signum(), end.y),
        gpui::point(lane_x, end.y),
    );
    path.line_to(end);
}

fn draw_cardinality_mark(
    path: &mut gpui::PathBuilder,
    at: gpui::Point<gpui::Pixels>,
    direction: f32,
    cardinality: Cardinality,
    zoom: f32,
) {
    let offset =
        |along: f32, across: f32| at + gpui::point(px(along * direction * zoom), px(across * zoom));
    let bar = |path: &mut gpui::PathBuilder, distance| {
        path.move_to(offset(distance, -5.));
        path.line_to(offset(distance, 5.));
    };
    match cardinality {
        Cardinality::One => {
            bar(path, 6.);
            bar(path, 12.);
        }
        Cardinality::ZeroOrOne | Cardinality::ZeroOrMany => {
            if cardinality == Cardinality::ZeroOrOne {
                bar(path, 6.);
            } else {
                path.move_to(offset(6., -6.));
                path.line_to(offset(14., 0.));
                path.line_to(offset(6., 6.));
            }
            for step in 0..=12 {
                let angle = step as f32 * std::f32::consts::TAU / 12.0;
                let point = offset(19. + 3.5 * angle.cos(), 3.5 * angle.sin());
                if step == 0 {
                    path.move_to(point);
                } else {
                    path.line_to(point);
                }
            }
        }
        Cardinality::Unknown => {}
    }
}

fn object_icon(kind: sift_protocol::CatalogNodeKind) -> IconName {
    match kind {
        sift_protocol::CatalogNodeKind::View | sift_protocol::CatalogNodeKind::MaterializedView => {
            IconName::View
        }
        _ => IconName::Table,
    }
}

struct CardSurfaceContent {
    table_id: sift_protocol::CatalogObjectId,
    title: String,
    kind: &'static str,
    footer: String,
    icon: IconName,
    rows: Vec<(String, String, String)>,
}

fn relationship_card_surface(
    content: CardSurfaceContent,
    zoom: f32,
    colors: ThemeColors,
    cache: Rc<RefCell<HashMap<sift_protocol::CatalogObjectId, CachedCardBody>>>,
    scroll: ScrollHandle,
) -> impl IntoElement {
    let CardSurfaceContent {
        table_id,
        title,
        kind,
        footer,
        icon,
        rows,
    } = content;
    canvas(
        move |_, window, _| {
            let compact = zoom < 0.6;
            let mut style = window.text_style();
            style.font_size = px(if compact { 10.0 } else { 11.0 * zoom }).into();
            if let Some(cached) = cache.borrow().get(&table_id) {
                if cached.text_style == style && cached.colors == colors && cached.zoom == zoom {
                    return (cached.row_count, cached.lines.clone());
                }
            }
            let font_size = style.font_size.to_pixels(window.rem_size());
            let mut key_style = style.clone();
            key_style.font_weight = gpui::FontWeight::SEMIBOLD;
            key_style.color = colors.accent;
            let mut name_style = style.clone();
            name_style.color = colors.text;
            let mut type_style = style.clone();
            type_style.color = colors.muted_text;
            let mut lines = Vec::with_capacity(rows.len() * 3 + 3);
            if compact {
                for (value, text_style, x, y, width, size) in [
                    (
                        &title,
                        &name_style,
                        17.0,
                        5.0,
                        CARD_WIDTH * zoom - 21.0,
                        10.0,
                    ),
                    (
                        &footer,
                        &type_style,
                        5.0,
                        CARD_HEIGHT * zoom - 14.0,
                        CARD_WIDTH * zoom - 10.0,
                        9.0,
                    ),
                ] {
                    let mut text_style = text_style.clone();
                    text_style.font_size = px(size).into();
                    let line = window.text_system().shape_line(
                        value.clone().into(),
                        px(size),
                        &[text_style.to_run(value.len())],
                        None,
                    );
                    lines.push((line, gpui::point(px(x), px(y)), px(width.max(0.0))));
                }
            } else {
                let kind_label = kind.to_owned();
                for (value, text_style, x, y, width, size) in [
                    (&title, &name_style, 32.0, 10.0, 184.0, 15.0),
                    (&kind_label, &type_style, 222.0, 13.0, 54.0, 10.0),
                    (&footer, &type_style, 10.0, 185.0, 264.0, 10.0),
                ] {
                    let mut text_style = text_style.clone();
                    text_style.font_size = px(size * zoom).into();
                    if size == 15.0 {
                        text_style.font_weight = gpui::FontWeight::BOLD;
                    }
                    let line = window.text_system().shape_line(
                        value.clone().into(),
                        text_style.font_size.to_pixels(window.rem_size()),
                        &[text_style.to_run(value.len())],
                        None,
                    );
                    lines.push((
                        line,
                        gpui::point(px(x * zoom), px(y * zoom)),
                        px(width * zoom),
                    ));
                }
                for (index, (key, name, data_type)) in rows.iter().enumerate() {
                    let y = px((CARD_HEADER_HEIGHT + 2.0 + CARD_ROW_HEIGHT * index as f32) * zoom);
                    for (value, text_style, x, width) in [
                        (key, &key_style, 9.0, 20.0),
                        (name, &name_style, 37.0, 138.0),
                        (data_type, &type_style, 180.0, 94.0),
                    ] {
                        if value.is_empty() {
                            continue;
                        }
                        let line = window.text_system().shape_line(
                            value.clone().into(),
                            font_size,
                            &[text_style.to_run(value.len())],
                            None,
                        );
                        lines.push((line, gpui::point(px(x * zoom), y), px(width * zoom)));
                    }
                }
            }
            let row_count = if compact { 0 } else { rows.len() };
            cache.borrow_mut().insert(
                table_id.clone(),
                CachedCardBody {
                    text_style: style,
                    colors,
                    zoom,
                    row_count,
                    lines: lines.clone(),
                },
            );
            (row_count, lines)
        },
        move |bounds, (row_count, mut lines), window, cx| {
            let viewport = scroll.bounds();
            if viewport.size.width > px(0.)
                && (bounds.right() < viewport.left()
                    || bounds.left() > viewport.right()
                    || bounds.bottom() < viewport.top()
                    || bounds.top() > viewport.bottom())
            {
                return;
            }
            window.paint_quad(gpui::fill(
                gpui::Bounds::new(
                    bounds.origin,
                    gpui::size(
                        bounds.size.width,
                        px(if zoom < 0.6 {
                            24.0
                        } else {
                            CARD_HEADER_HEIGHT * zoom
                        }),
                    ),
                ),
                colors.accent_muted,
            ));
            window.paint_quad(gpui::fill(
                gpui::Bounds::new(
                    gpui::point(
                        bounds.left(),
                        bounds.top()
                            + px(if zoom < 0.6 {
                                23.0
                            } else {
                                (CARD_HEADER_HEIGHT - 1.0) * zoom
                            }),
                    ),
                    gpui::size(bounds.size.width, px(1.0)),
                ),
                colors.accent,
            ));
            let _ = window.paint_svg(
                gpui::Bounds::new(
                    bounds.origin
                        + gpui::point(
                            px(if zoom < 0.6 { 4.0 } else { 9.0 * zoom }),
                            px(if zoom < 0.6 { 6.0 } else { 11.0 * zoom }),
                        ),
                    gpui::size(
                        px(if zoom < 0.6 { 10.0 } else { 15.0 * zoom }),
                        px(if zoom < 0.6 { 10.0 } else { 15.0 * zoom }),
                    ),
                ),
                icon.path().into(),
                None,
                Default::default(),
                colors.accent,
                cx,
            );
            let mut separators = gpui::PathBuilder::stroke(px(1.0));
            for row in 1..=row_count {
                let y =
                    bounds.top() + px((CARD_HEADER_HEIGHT + row as f32 * CARD_ROW_HEIGHT) * zoom);
                separators.move_to(gpui::point(bounds.left(), y));
                separators.line_to(gpui::point(bounds.right(), y));
            }
            if let Ok(path) = separators.build() {
                window.paint_path(path, colors.subtle_border);
            }
            for (line, relative_origin, width) in lines.drain(..) {
                let origin = bounds.origin + relative_origin;
                let line_height = (bounds.bottom() - origin.y).min(px(CARD_ROW_HEIGHT * zoom));
                if line.width() <= width {
                    let _ = line.paint(
                        origin,
                        line_height,
                        TextAlign::Left,
                        Some(width),
                        window,
                        cx,
                    );
                } else {
                    window.with_content_mask(
                        Some(ContentMask {
                            bounds: gpui::Bounds::new(origin, gpui::size(width, line_height)),
                        }),
                        |window| {
                            let _ = line.paint(
                                origin,
                                line_height,
                                TextAlign::Left,
                                Some(width),
                                window,
                                cx,
                            );
                        },
                    );
                }
            }
        },
    )
    .w_full()
    .h_full()
}

fn relationship_relation_list(
    rows: Vec<(usize, bool, String, String, String)>,
    row_count: usize,
    colors: ThemeColors,
    cache: Rc<RefCell<HashMap<usize, CachedRelationRow>>>,
    scroll: ScrollHandle,
) -> impl IntoElement {
    canvas(
        move |bounds, window, _| {
            let mut style = window.text_style();
            style.font_size = px(11.0).into();
            let font_size = style.font_size.to_pixels(window.rem_size());
            let mut main_style = style.clone();
            main_style.color = colors.text;
            let mut accent_style = style.clone();
            accent_style.color = colors.accent;
            let mut muted_style = style.clone();
            muted_style.color = colors.muted_text;
            let mut lines = Vec::with_capacity(rows.len() * 4);
            let viewport = scroll.bounds();
            let mut visible_rows = Vec::new();
            for (index, outgoing, target, cardinality, mapping) in &rows {
                let top = bounds.top() + px(*index as f32 * RELATION_ROW_STEP);
                if viewport.size.height > px(0.)
                    && (top + px(RELATION_ROW_STEP) < viewport.top() - px(24.)
                        || top > viewport.bottom() + px(24.))
                {
                    continue;
                }
                visible_rows.push(*index);
                let texts = [
                    if *outgoing { "→" } else { "←" }.to_owned(),
                    target.clone(),
                    cardinality.clone(),
                    mapping.clone(),
                ];
                let cached = cache
                    .borrow()
                    .get(index)
                    .filter(|cached| {
                        cached.text_style == style
                            && cached.colors == colors
                            && cached.texts == texts
                    })
                    .cloned();
                let shaped = if let Some(cached) = cached {
                    cached.lines
                } else {
                    let shaped = std::array::from_fn(|part| {
                        let text_style = match part {
                            0 => &accent_style,
                            1 => &main_style,
                            _ => &muted_style,
                        };
                        window.text_system().shape_line(
                            texts[part].clone().into(),
                            font_size,
                            &[text_style.to_run(texts[part].len())],
                            None,
                        )
                    });
                    cache.borrow_mut().insert(
                        *index,
                        CachedRelationRow {
                            text_style: style.clone(),
                            colors,
                            texts,
                            lines: shaped.clone(),
                        },
                    );
                    shaped
                };
                for (line, (x, y, width)) in shaped.into_iter().zip([
                    (8.0, 6.0, 16.0),
                    (28.0, 6.0, 154.0),
                    (188.0, 6.0, 104.0),
                    (8.0, 30.0, 284.0),
                ]) {
                    lines.push((
                        line,
                        gpui::point(bounds.left() + px(x), top + px(y)),
                        px(width),
                    ));
                }
            }
            (visible_rows, lines)
        },
        move |bounds, (row_indices, mut lines), window, cx| {
            let mut separators = gpui::PathBuilder::stroke(px(1.0));
            for index in row_indices {
                let y = bounds.top() + px((index + 1) as f32 * RELATION_ROW_STEP);
                separators.move_to(gpui::point(bounds.left(), y));
                separators.line_to(gpui::point(bounds.right(), y));
            }
            if let Ok(path) = separators.build() {
                window.paint_path(path, colors.subtle_border);
            }
            for (line, origin, width) in lines.drain(..) {
                if line.width() <= width {
                    let _ = line.paint(origin, px(23.), TextAlign::Left, Some(width), window, cx);
                } else {
                    window.with_content_mask(
                        Some(ContentMask {
                            bounds: gpui::Bounds::new(origin, gpui::size(width, px(23.))),
                        }),
                        |window| {
                            let _ = line.paint(
                                origin,
                                px(23.),
                                TextAlign::Left,
                                Some(width),
                                window,
                                cx,
                            );
                        },
                    );
                }
            }
        },
    )
    .w_full()
    .h(px(row_count as f32 * RELATION_ROW_STEP))
}

fn source_for_table(
    diagram: &sift_protocol::CatalogDiagram,
    index: &RelationshipViewerIndex,
    base: &DatabaseObjectSource,
    table_id: &sift_protocol::CatalogObjectId,
) -> Option<DatabaseObjectSource> {
    let table = &diagram.nodes[*index.nodes.get(table_id)?];
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
    let schema = &diagram.nodes[*index.nodes.get(table.parent_id.as_ref()?)?];
    let catalog = schema
        .parent_id
        .as_ref()
        .and_then(|id| index.nodes.get(id))
        .map(|&node| &diagram.nodes[node]);
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
    let mut output = String::from("erDiagram\n");
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
        .map(|node| (&node.id, node))
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
        let (child, parent) = fk_cardinality(&nodes, edge);
        if child == Cardinality::Unknown || parent == Cardinality::Unknown {
            output.push_str(&format!(
                "  %% {from} references {to}; cardinality unavailable\n"
            ));
            continue;
        }
        let child_mark = match child {
            Cardinality::ZeroOrOne => "|o",
            Cardinality::ZeroOrMany => "}o",
            Cardinality::One => "||",
            Cardinality::Unknown => unreachable!(),
        };
        let parent_mark = match parent {
            Cardinality::ZeroOrOne => "o|",
            Cardinality::One => "||",
            Cardinality::ZeroOrMany => "o{",
            Cardinality::Unknown => unreachable!(),
        };
        output.push_str(&format!(
            "  {from} {child_mark}--{parent_mark} {to} : references\n"
        ));
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
        let focus_handle = cx.focus_handle();
        self.relationship_viewers.insert(
            item_id,
            RelationshipViewerState::new(source, search_input, focus_handle),
        );
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
                        if let Some(index) = viewer.index.as_mut() {
                            index.filter(&viewer.search);
                            if viewer.selected.as_ref().is_none_or(|selected| {
                                !index.visible_positions.contains_key(selected)
                            }) {
                                viewer.selected = index.visible_tables.first().cloned();
                                viewer
                                    .details_scroll
                                    .set_offset(gpui::point(px(0.), px(0.)));
                                viewer.reveal_selection();
                            }
                        }
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
        let mut relation_rows = Vec::new();
        let mut relation_row_count = 0_usize;
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
        if let (Some(diagram), Some(index)) = (viewer.diagram.as_ref(), viewer.index.as_ref()) {
            let nodes = diagram
                .nodes
                .iter()
                .map(|node| (&node.id, node))
                .collect::<HashMap<_, _>>();
            let visible_tables = index
                .visible_tables
                .iter()
                .filter_map(|id| nodes.get(id).copied())
                .collect::<Vec<_>>();
            let selected = viewer.selected.as_ref();
            let selected_table = selected.and_then(|id| nodes.get(id).copied());
            let fk_edges = index
                .fk_edges
                .iter()
                .map(|&edge| &diagram.edges[edge])
                .collect::<Vec<_>>();
            let edge_cardinalities = &index.edge_cardinalities;
            let positions = &index.positions;
            let mut column_anchors = HashMap::new();
            scene_height = index.scene_height;
            summary = format!(
                "{} tables · {} FKs{}",
                index.table_search.len(),
                fk_edges.len(),
                if diagram.partial { " · partial" } else { "" }
            );
            let foreign_key_columns = &index.fk_columns;
            let viewport = viewer.scroll.bounds().size;
            let offset = viewer.scroll.offset();
            let cull_cards = viewport.width > px(0.) && viewport.height > px(0.);
            let overscan = 200.0;
            let visible_left = -f32::from(offset.x) - overscan;
            let visible_right = -f32::from(offset.x) + f32::from(viewport.width) + overscan;
            let visible_top = -f32::from(offset.y) - overscan;
            let visible_bottom = -f32::from(offset.y) + f32::from(viewport.height) + overscan;
            for (card_index, table) in visible_tables.into_iter().enumerate() {
                let table_id = table.id.clone();
                let Some((x, y)) = positions.get(&table.id).copied() else {
                    continue;
                };
                let all_columns = index
                    .columns_by_table
                    .get(&table.id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let column_count = all_columns.len();
                let hidden_columns = column_count.saturating_sub(6);
                for (position, &column) in all_columns.iter().take(6).enumerate() {
                    column_anchors.insert(
                        diagram.nodes[column].id.clone(),
                        CARD_HEADER_HEIGHT
                            + position as f32 * CARD_ROW_HEIGHT
                            + CARD_ROW_HEIGHT / 2.0,
                    );
                }
                let card_left = 12.0 + x * zoom;
                let card_top = 12.0 + y * zoom;
                if cull_cards
                    && (card_left + CARD_WIDTH * zoom < visible_left
                        || card_left > visible_right
                        || card_top + CARD_HEIGHT * zoom < visible_top
                        || card_top > visible_bottom)
                {
                    continue;
                }
                let table_schema = table
                    .parent_id
                    .as_ref()
                    .and_then(|id| nodes.get(id))
                    .map_or("?", |node| node.name.as_str());
                let rows = all_columns
                    .iter()
                    .copied()
                    .take(6)
                    .map(|column| {
                        let node = &diagram.nodes[column];
                        let (key, data_type) = match &node.details {
                            sift_protocol::CatalogNodeDetails::Column { column } => {
                                let key = if column.primary_key {
                                    "PK"
                                } else if foreign_key_columns.contains(&node.id) {
                                    "FK"
                                } else {
                                    ""
                                };
                                (key, type_ref_label(&column.type_ref))
                            }
                            _ => ("", String::new()),
                        };
                        (key.to_owned(), node.name.clone(), data_type)
                    })
                    .collect::<Vec<_>>();
                table_cards.push(
                    div()
                        .id(("relationship-card", card_index))
                        .debug_selector(move || format!("relationship-card-{card_index}"))
                        .absolute()
                        .left(px(x * zoom))
                        .top(px(y * zoom))
                        .w(px(CARD_WIDTH * zoom))
                        .h(px(CARD_HEIGHT * zoom))
                        .flex_none()
                        .flex()
                        .flex_col()
                        .text_size(px(12. * zoom))
                        .rounded_sm()
                        .overflow_hidden()
                        .border_1()
                        .border_color(if selected == Some(&table.id) {
                            colors.accent
                        } else {
                            colors.subtle_border
                        })
                        .bg(if selected == Some(&table.id) {
                            colors
                                .elevated_surface
                                .blend(colors.active_surface)
                                .alpha(1.0)
                        } else {
                            colors.elevated_surface
                        })
                        .cursor_pointer()
                        .role(Role::Button)
                        .aria_label(format!("Inspect table {}", table.qualified_name))
                        .occlude()
                        .on_click(cx.listener(move |pane, _, window, cx| {
                            if let Some(viewer) = pane.relationship_viewers.get_mut(&item_id) {
                                viewer.focus_handle.focus(window, cx);
                                if viewer.selected.as_ref() != Some(&table_id) {
                                    viewer.selected = Some(table_id.clone());
                                    viewer
                                        .details_scroll
                                        .set_offset(gpui::point(px(0.), px(0.)));
                                    cx.notify();
                                }
                            }
                        }))
                        .child(relationship_card_surface(
                            CardSurfaceContent {
                                table_id: table.id.clone(),
                                title: format!("{table_schema}.{}", table.name),
                                kind: match table.kind {
                                    sift_protocol::CatalogNodeKind::View => "VIEW",
                                    sift_protocol::CatalogNodeKind::MaterializedView => "MAT VIEW",
                                    sift_protocol::CatalogNodeKind::ForeignTable => "FOREIGN",
                                    sift_protocol::CatalogNodeKind::PartitionedTable => {
                                        "PARTITIONED"
                                    }
                                    _ => "TABLE",
                                },
                                footer: if hidden_columns > 0 {
                                    format!("+{hidden_columns} more columns")
                                } else {
                                    format!("{column_count} columns")
                                },
                                icon: object_icon(table.kind),
                                rows,
                            },
                            zoom,
                            colors,
                            viewer.card_text_cache.clone(),
                            viewer.scroll.clone(),
                        )),
                );
            }
            let mut route_counts = HashMap::<(i32, i32), usize>::new();
            for (index, edge) in fk_edges.iter().enumerate() {
                let Some(from) = owner(&nodes, &edge.from) else {
                    continue;
                };
                let to = edge.to.as_ref().and_then(|id| owner(&nodes, id));
                if let Some(to) = to {
                    if let (Some(&from_position), Some(&to_position)) =
                        (positions.get(&from.id), positions.get(&to.id))
                    {
                        let lane_pair = (
                            (from_position.0 / LANE_STEP) as i32,
                            (to_position.0 / LANE_STEP) as i32,
                        );
                        let route_index = route_counts.entry(lane_pair).or_default();
                        let route_offset = [0.0, 8.0, -8.0, 16.0, -16.0][*route_index % 5];
                        *route_index += 1;
                        scene_edges.push((
                            from_position,
                            to_position,
                            edge.column_pairs
                                .first()
                                .and_then(|pair| column_anchors.get(&pair.from))
                                .copied()
                                .unwrap_or(CARD_MIDPOINT_Y),
                            edge.column_pairs
                                .first()
                                .and_then(|pair| column_anchors.get(&pair.to))
                                .copied()
                                .unwrap_or(CARD_MIDPOINT_Y),
                            route_offset,
                            edge_cardinalities[index],
                            selected.is_some_and(|id| id == &from.id || id == &to.id),
                        ));
                    }
                }
            }
            if let Some(table) = selected_table.filter(|_| viewer.details_open) {
                for column in index
                    .columns_by_table
                    .get(&table.id)
                    .into_iter()
                    .flat_map(|columns| columns.iter().copied())
                {
                    let column = &diagram.nodes[column];
                    let (data_type, key, nullable) = match &column.details {
                        sift_protocol::CatalogNodeDetails::Column { column: metadata } => {
                            let is_fk = foreign_key_columns.contains(&column.id);
                            (
                                type_ref_label(&metadata.type_ref),
                                if metadata.primary_key {
                                    "PK"
                                } else if is_fk {
                                    "FK"
                                } else {
                                    ""
                                },
                                metadata.nullable,
                            )
                        }
                        _ => (String::new(), "", sift_protocol::Nullability::Unknown),
                    };
                    detail_rows.push(
                        div()
                            .h(px(26.))
                            .flex()
                            .items_center()
                            .gap_2()
                            .border_b_1()
                            .border_color(colors.subtle_border)
                            .text_xs()
                            .child(
                                div()
                                    .w(px(20.))
                                    .flex_none()
                                    .text_color(colors.accent)
                                    .child(key),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(column.name.clone()),
                            )
                            .child(
                                div()
                                    .max_w(px(80.))
                                    .truncate()
                                    .text_color(colors.muted_text)
                                    .child(data_type),
                            )
                            .child(div().w(px(12.)).text_color(colors.muted_text).child(
                                match nullable {
                                    sift_protocol::Nullability::Nullable => "○",
                                    sift_protocol::Nullability::NotNullable => "●",
                                    sift_protocol::Nullability::Unknown => "?",
                                },
                            )),
                    );
                }
                for &child in index.children_by_table.get(&table.id).into_iter().flatten() {
                    let node = &diagram.nodes[child];
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
                let details_scroll = &viewer.details_scroll;
                let details_viewport = details_scroll.bounds();
                let details_offset = details_scroll.offset();
                let cull_relations = details_viewport.size.height > px(0.);
                // Avoid stale bounds when selecting a table with a different
                // column count; the overscan covers the header's small variance.
                let relation_list_top =
                    120.0 + index.columns_by_table.get(&table.id).map_or(0, Vec::len) as f32 * 26.0;
                let visible_top = -f32::from(details_offset.y) - 320.0;
                let visible_bottom =
                    -f32::from(details_offset.y) + f32::from(details_viewport.size.height) + 320.0;
                for (index, edge) in fk_edges.iter().enumerate() {
                    let Some(from) = owner(&nodes, &edge.from) else {
                        continue;
                    };
                    let to = edge.to.as_ref().and_then(|id| owner(&nodes, id));
                    let is_outgoing = from.id == table.id;
                    let is_incoming = to.is_some_and(|to| to.id == table.id);
                    if !(is_outgoing && viewer.outgoing || is_incoming && viewer.incoming) {
                        continue;
                    }
                    let row_index = relation_row_count;
                    relation_row_count += 1;
                    let row_top = relation_list_top + row_index as f32 * RELATION_ROW_STEP;
                    if cull_relations
                        && (row_top + RELATION_ROW_STEP < visible_top || row_top > visible_bottom)
                    {
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
                    let (child, parent) = edge_cardinalities[index];
                    relation_rows.push((
                        row_index,
                        is_outgoing,
                        target.to_owned(),
                        format!(
                            "{} : {}",
                            cardinality_label(if is_outgoing { child } else { parent }),
                            cardinality_label(if is_outgoing { parent } else { child })
                        ),
                        pair_label(&nodes, edge),
                    ));
                }
            }
        }
        let depth = viewer.depth;
        let loading = viewer.loading;
        let incoming = viewer.incoming;
        let outgoing = viewer.outgoing;
        let selected_source =
            viewer
                .diagram
                .as_ref()
                .zip(viewer.index.as_ref())
                .and_then(|(diagram, index)| {
                    viewer
                        .selected
                        .as_ref()
                        .and_then(|id| source_for_table(diagram, index, &viewer.source, id))
                });
        let can_focus = selected_source.as_ref().is_some_and(|source| {
            source.catalog != viewer.source.catalog
                || source.schema != viewer.source.schema
                || source.object != viewer.source.object
        });
        let selected_schema = selected_source
            .as_ref()
            .map_or(viewer.source.schema.clone(), |source| source.schema.clone());
        let selected_node =
            viewer
                .diagram
                .as_ref()
                .zip(viewer.index.as_ref())
                .and_then(|(diagram, index)| {
                    let node = *index.nodes.get(viewer.selected.as_ref()?)?;
                    diagram.nodes.get(node)
                });
        let selected_title = selected_node
            .map(|node| node.name.clone())
            .unwrap_or_else(|| viewer.source.object.clone());
        let selected_kind =
            selected_node.map_or(sift_protocol::CatalogNodeKind::Table, |node| node.kind);
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
                    let source_icon = match source.object_kind {
                        sift_protocol::ObjectKind::View
                        | sift_protocol::ObjectKind::MaterializedView => IconName::View,
                        _ => IconName::Table,
                    };
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
                                viewer.index = None;
                                viewer.selected = None;
                                viewer.anchor = None;
                                viewer.scope_picker_open = false;
                                cx.emit(PaneEvent::RelationshipViewerRefreshRequested { item_id });
                                cx.notify();
                            }
                        }))
                        .child(icon(source_icon, colors.muted_text, 12.))
                        .child(div().ml_2().truncate().child(label))
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let scene_scroll = viewer.scroll.clone();
        let edge_layer = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let viewport = scene_scroll.bounds();
                let mut muted_path = gpui::PathBuilder::stroke(px(1.25));
                let mut accent_path = gpui::PathBuilder::stroke(px(1.75));
                for (
                    (from_x, from_y),
                    (to_x, to_y),
                    from_anchor,
                    to_anchor,
                    route_offset,
                    (child, parent),
                    highlighted,
                ) in &scene_edges
                {
                    let path = if *highlighted {
                        &mut accent_path
                    } else {
                        &mut muted_path
                    };
                    let direction = if from_x < to_x {
                        1.0
                    } else if from_x > to_x {
                        -1.0
                    } else {
                        1.0
                    };
                    let (start_x, end_x) = if from_x < to_x {
                        (from_x + CARD_WIDTH, *to_x)
                    } else if from_x > to_x {
                        (*from_x, to_x + CARD_WIDTH)
                    } else {
                        (from_x + CARD_WIDTH, to_x + CARD_WIDTH)
                    };
                    let start = bounds.origin
                        + gpui::point(px(start_x * zoom), px((*from_y + *from_anchor) * zoom));
                    let end = bounds.origin
                        + gpui::point(px(end_x * zoom), px((*to_y + *to_anchor) * zoom));
                    let end_direction = if from_x == to_x { 1.0 } else { -direction };
                    let wire_start = start
                        + gpui::point(
                            px(cardinality_wire_offset(*child) * direction * zoom),
                            px(0.),
                        );
                    let wire_end = end
                        + gpui::point(
                            px(cardinality_wire_offset(*parent) * end_direction * zoom),
                            px(0.),
                        );
                    let lane_x = if from_x == to_x {
                        start.x + px((82. + *route_offset) * zoom)
                    } else {
                        (wire_start.x + wire_end.x) / 2.0 + px(*route_offset * zoom)
                    };
                    if viewport.size.width > px(0.) && viewport.size.height > px(0.) {
                        let margin = px(32.);
                        let left = start.x.min(end.x).min(lane_x) - margin;
                        let right = start.x.max(end.x).max(lane_x) + margin;
                        let top = start.y.min(end.y) - margin;
                        let bottom = start.y.max(end.y) + margin;
                        if right < viewport.left()
                            || left > viewport.right()
                            || bottom < viewport.top()
                            || top > viewport.bottom()
                        {
                            continue;
                        }
                    }
                    draw_relationship_wire(path, wire_start, wire_end, lane_x);
                    draw_cardinality_mark(path, start, direction, *child, zoom);
                    draw_cardinality_mark(path, end, end_direction, *parent, zoom);
                }
                if let Ok(path) = muted_path.build() {
                    window.paint_path(path, colors.muted_text);
                }
                if let Ok(path) = accent_path.build() {
                    window.paint_path(path, colors.accent);
                }
            },
        )
        .w(px(SCENE_WIDTH * zoom))
        .h(px(scene_height * zoom));
        let wheel_scroll = viewer.scroll.clone();
        let pointer_scroll = viewer.scroll.clone();
        let rendered_offset = viewer.scroll.offset();
        let details_wheel_scroll = viewer.details_scroll.clone();
        let details_pointer_scroll = viewer.details_scroll.clone();
        let details_rendered_offset = viewer.details_scroll.offset();
        let pane_entity_id = cx.entity().entity_id();
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .id(("relationship-toolbar", item_id as usize))
                    .h(px(38.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .overflow_x_scroll()
                    .bg(colors.toolbar)
                    .border_b_1()
                    .border_color(colors.subtle_border)
                    .child(
                        div()
                            .h(px(33.))
                            .flex_none()
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().relative().flex_none()
                                .child(div().id(("relationship-scope", item_id as usize))
                                    .debug_selector(|| "relationship-scope".into())
                                    .h(px(24.)).max_w(px(220.)).px_2().flex().items_center().gap_2()
                                    .rounded_sm().border_1().border_color(colors.subtle_border)
                                    .cursor_pointer().role(Role::Button).aria_label("Choose relationship table")
                                    .hover(|button| button.bg(colors.hovered_surface))
                                    .on_click(cx.listener(move |pane, _, _, cx| {
                                        if let Some(viewer) = pane.relationship_viewers.get_mut(&item_id) {
                                            viewer.scope_picker_open = !viewer.scope_picker_open;
                                            cx.notify();
                                        }
                                    }))
                                    .child(icon(IconName::Table, colors.accent, 12.))
                                    .child(div().truncate().text_xs().child(format!("{}.{}", viewer.source.schema, viewer.source.object)))
                                    .child(icon(IconName::ChevronDown, colors.muted_text, 11.)))
                                .when(viewer.scope_picker_open, |trigger| trigger.child(
                                    div().absolute().top_full().left_0().child(deferred(
                                        anchored().anchor(Anchor::TopLeft).child(
                                            div().id("relationship-scope-menu")
                                                .debug_selector(|| "relationship-scope-menu".into())
                                                .w(px(290.)).max_h(px(300.)).overflow_y_scroll()
                                                .p_1().rounded_sm().border_1().border_color(colors.strong_border)
                                                .bg(colors.elevated_surface).shadow_lg().occlude().role(Role::Menu)
                                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                                .on_mouse_down_out(cx.listener(move |pane, _, _, cx| {
                                                    if let Some(viewer) = pane.relationship_viewers.get_mut(&item_id) {
                                                        viewer.scope_picker_open = false;
                                                    }
                                                    cx.notify();
                                                }))
                                                .children(scope_choices),
                                        )).with_priority(3),
                                    ),
                                ))),
                    )
                    .child(
                        div()
                            .id(("relationship-controls", item_id as usize))
                            .debug_selector(|| "relationship-controls".into())
                            .h(px(33.))
                            .flex_1()
                            .min_w(px(520.))
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().w(px(180.)).flex_none().child(viewer.search_input.clone()))
                            .child(div().flex_1())
                            .child(div().flex_none().text_xs().text_color(colors.muted_text).child(summary))
                            .child(Button::new("relationship-details-toggle", if viewer.details_open { "Hide details" } else { "Details" })
                                .debug_selector("relationship-details-toggle")
                                .tone(if viewer.details_open { ButtonTone::Neutral } else { ButtonTone::Ghost })
                                .on_click(cx.listener(move |pane, _, _, cx| pane.toggle_relationship_details(item_id, cx))))
                            .children((self.expanded_relationship_item != Some(item_id)).then(|| {
                                IconButton::new("relationship-expand", IconName::Maximize, "Open large relationship view")
                                    .debug_selector("relationship-expand")
                                    .square(px(26.))
                                    .icon_size(14.)
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(PaneEvent::RelationshipViewerExpandRequested { item_id });
                                    }))
                            }))
                            .child(Button::new("relationship-copy-mermaid", "Copy Mermaid")
                                .debug_selector("relationship-copy-mermaid")
                                .tone(ButtonTone::Ghost).disabled(viewer.diagram.is_none())
                                .on_click(cx.listener(move |pane, _, _, cx| {
                                    if let Some(diagram) = pane.relationship_viewers.get(&item_id).and_then(|viewer| viewer.diagram.as_deref()) {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(diagram_mermaid(diagram)));
                                    }
                                }))),
                    ),
            )
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
                        div().relative().flex_1().min_w_0().min_h_0()
                        .child(div()
                            .id(("relationship-cards", item_id as usize))
                            .debug_selector(|| "relationship-cards".into())
                            .size_full()
                            .overflow_x_scroll()
                            .overflow_y_scroll()
                            .track_scroll(&viewer.scroll)
                            .track_focus(&viewer.focus_handle)
                            .tab_index(0)
                            .on_mouse_down(MouseButton::Left, cx.listener(move |pane, _, window, cx| {
                                if let Some(viewer) = pane.relationship_viewers.get(&item_id) {
                                    viewer.focus_handle.focus(window, cx);
                                }
                            }))
                            .on_key_down(cx.listener(move |pane, event: &gpui::KeyDownEvent, _, cx| {
                                if event.keystroke.modifiers.modified() {
                                    return;
                                }
                                let Some(viewer) = pane.relationship_viewers.get_mut(&item_id) else {
                                    return;
                                };
                                let changed = match event.keystroke.key.as_str() {
                                    "j" => viewer.move_selection(true),
                                    "k" => viewer.move_selection(false),
                                    "h" | "l" => {
                                        let scroll = &viewer.scroll;
                                        let current = scroll.offset();
                                        let delta = if event.keystroke.key == "h" { 96.0 } else { -96.0 };
                                        let next = (current.x + px(delta)).clamp(-scroll.max_offset().x, px(0.));
                                        if next != current.x {
                                            scroll.set_offset(gpui::point(next, current.y));
                                            true
                                        } else {
                                            false
                                        }
                                    }
                                    _ => return,
                                };
                                if changed {
                                    cx.notify();
                                }
                                cx.stop_propagation();
                            }))
                            .on_scroll_wheel(move |event, window, cx| {
                                let delta = event.delta.pixel_delta(window.line_height());
                                let current = wheel_scroll.offset();
                                let max = wheel_scroll.max_offset();
                                let next = if event.modifiers.shift {
                                    let horizontal = if delta.y == px(0.) { delta.x } else { delta.y } * 1.75;
                                    gpui::point((current.x + horizontal).clamp(-max.x, px(0.)), current.y)
                                } else {
                                    gpui::point(
                                        (current.x + delta.x * 1.75).clamp(-max.x, px(0.)),
                                        (current.y + delta.y).clamp(-max.y, px(0.)),
                                    )
                                };
                                if next != current {
                                    wheel_scroll.set_offset(next);
                                    if scroll_needs_rebuild(next, rendered_offset, 100.0) {
                                        cx.notify(pane_entity_id);
                                    } else {
                                        window.refresh();
                                    }
                                }
                                cx.stop_propagation();
                            })
                            .on_mouse_move(move |_, _, cx| {
                                if scroll_needs_rebuild(pointer_scroll.offset(), rendered_offset, 100.0) {
                                    cx.notify(pane_entity_id);
                                }
                            })
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(div().relative().w(px(SCENE_WIDTH * zoom)).h(px(scene_height * zoom))
                                .child(edge_layer)
                                .children(table_cards)))
                        .child(div().absolute().right(px(16.)).bottom(px(16.))
                            .h(px(30.)).px_1().flex().items_center().gap_1()
                            .rounded_sm().border_1().border_color(colors.strong_border)
                            .bg(colors.elevated_surface).shadow_lg()
                            .child(Button::new("relationship-zoom-out", "−").tone(ButtonTone::Ghost)
                                .disabled(zoom <= 0.2).on_click(cx.listener(move |pane, _, _, cx| pane.change_relationship_zoom(item_id, -0.15, cx))))
                            .child(div().w(px(38.)).text_xs().text_color(colors.muted_text).child(format!("{}%", (zoom * 100.0).round() as u32)))
                            .child(Button::new("relationship-zoom-in", "+").tone(ButtonTone::Ghost)
                                .disabled(zoom >= 1.5).on_click(cx.listener(move |pane, _, _, cx| pane.change_relationship_zoom(item_id, 0.15, cx))))
                            .child(div().h(px(16.)).w(px(1.)).mx_1().bg(colors.subtle_border))
                            .child(Button::new("relationship-fit-width", if viewer.fit_active { "Actual size" } else { "Fit graph" }).debug_selector("relationship-fit-width").tone(ButtonTone::Ghost)
                                .on_click(cx.listener(move |pane, _, _, cx| pane.fit_relationship_width(item_id, cx))))
                            .child(div().h(px(16.)).w(px(1.)).mx_1().bg(colors.subtle_border))
                            .child(Button::new("relationship-depth-less", "−").tone(ButtonTone::Ghost)
                                .disabled(depth <= 1 || loading)
                                .on_click(cx.listener(move |pane, _, _, cx| pane.change_relationship_depth(item_id, -1, cx))))
                            .child(div().text_xs().text_color(colors.muted_text).child(format!(
                                "{depth} hop{}", if depth == 1 { "" } else { "s" }
                            )))
                            .child(Button::new("relationship-depth-more", "+").debug_selector("relationship-depth-more").tone(ButtonTone::Ghost)
                                .disabled(depth >= 3 || loading)
                                .on_click(cx.listener(move |pane, _, _, cx| pane.change_relationship_depth(item_id, 1, cx))))),
                    )
                        .children(viewer.details_open.then(||
                        div()
                            .id(("relationship-details", item_id as usize))
                            .debug_selector(|| "relationship-details".into())
                            .w(px(300.))
                            .flex_none()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&viewer.details_scroll)
                            .on_scroll_wheel(move |event, window, cx| {
                                let delta = event.delta.pixel_delta(window.line_height());
                                let current = details_wheel_scroll.offset();
                                let max = details_wheel_scroll.max_offset();
                                let next = (current.y + delta.y).clamp(-max.y, px(0.));
                                if next != current.y {
                                    details_wheel_scroll.set_offset(gpui::point(current.x, next));
                                    if scroll_needs_rebuild(details_wheel_scroll.offset(), details_rendered_offset, 120.0) {
                                        cx.notify(pane_entity_id);
                                    } else {
                                        window.refresh();
                                    }
                                }
                                cx.stop_propagation();
                            })
                            .on_mouse_move(move |_, _, cx| {
                                if scroll_needs_rebuild(details_pointer_scroll.offset(), details_rendered_offset, 120.0) {
                                    cx.notify(pane_entity_id);
                                }
                            })
                            .border_l_1()
                            .border_color(colors.subtle_border)
                            .bg(colors.panel)
                            .child(
                                div()
                                    .px_2().pt_2().pb_1()
                                    .flex().items_center().gap_2()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(icon(object_icon(selected_kind), colors.accent, 14.))
                                    .child(div().flex_1().min_w_0().flex().flex_col()
                                        .child(div().truncate().child(selected_title))
                                        .child(div().text_xs().text_color(colors.muted_text).font_weight(gpui::FontWeight::NORMAL).child(selected_schema)))
                                    .child(IconButton::new("relationship-close-details", IconName::CloseRightPane, "Hide details")
                                        .debug_selector("relationship-close-details")
                                        .square(px(22.)).icon_size(12.)
                                        .on_click(cx.listener(move |pane, _, _, cx| pane.toggle_relationship_details(item_id, cx)))),
                            )
                            .child(div().px_2().pb_1().flex().gap_1()
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
                                                viewer.index = None;
                                                viewer.selected = None;
                                                viewer.anchor = None;
                                                cx.emit(PaneEvent::RelationshipViewerRefreshRequested { item_id });
                                                cx.notify();
                                            }
                                        }))
                                })))
                            .child(
                                div()
                                    .px_2()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .child("COLUMNS"),
                            )
                            .child(div().px_2().text_xs().text_color(colors.muted_text).child("● required   ○ nullable   ? unknown"))
                            .child(div().px_2().children(detail_rows))
                            .child(div().px_2().py_1().flex().items_center().gap_1()
                                .child(div().flex_1().text_xs().text_color(colors.muted_text).child("RELATIONSHIPS"))
                                .child(Button::new("relationship-incoming", "Incoming")
                                    .debug_selector("relationship-incoming")
                                    .tone(if incoming { ButtonTone::Accent } else { ButtonTone::Ghost })
                                    .on_click(cx.listener(move |pane, _, _, cx| {
                                        if let Some(viewer) = pane.relationship_viewers.get_mut(&item_id) {
                                            viewer.incoming = !viewer.incoming;
                                            cx.notify();
                                        }
                                    })))
                                .child(Button::new("relationship-outgoing", "Outgoing")
                                    .debug_selector("relationship-outgoing")
                                    .tone(if outgoing { ButtonTone::Accent } else { ButtonTone::Ghost })
                                    .on_click(cx.listener(move |pane, _, _, cx| {
                                        if let Some(viewer) = pane.relationship_viewers.get_mut(&item_id) {
                                            viewer.outgoing = !viewer.outgoing;
                                            cx.notify();
                                        }
                                    }))))
                            .child(relationship_relation_list(
                                relation_rows,
                                relation_row_count,
                                colors,
                                viewer.relation_text_cache.clone(),
                                viewer.details_scroll.clone(),
                            ))
                            .child(div().p_2().text_xs().text_color(colors.muted_text).child("INDEXES · CONSTRAINTS · TRIGGERS"))
                            .child(div().px_2().children(auxiliary_rows)),
                    )),
            )
            .into_any_element()
    }

    fn toggle_relationship_details(&mut self, item_id: u64, cx: &mut Context<Self>) {
        if let Some(viewer) = self.relationship_viewers.get_mut(&item_id) {
            viewer.details_open = !viewer.details_open;
            viewer.fit_active = false;
            viewer.auto_fit_pending = false;
            cx.notify();
        }
    }

    fn change_relationship_depth(&mut self, item_id: u64, delta: i8, cx: &mut Context<Self>) {
        let Some(viewer) = self.relationship_viewers.get_mut(&item_id) else {
            return;
        };
        let depth = (i16::from(viewer.depth) + i16::from(delta)).clamp(1, 3) as u8;
        if depth == viewer.depth || viewer.loading {
            return;
        }
        viewer.depth = depth;
        cx.emit(PaneEvent::RelationshipViewerRefreshRequested { item_id });
        cx.notify();
    }

    fn change_relationship_zoom(&mut self, item_id: u64, delta: f32, cx: &mut Context<Self>) {
        if let Some(viewer) = self.relationship_viewers.get_mut(&item_id) {
            let zoom = (viewer.zoom + delta).clamp(0.2, 1.5);
            if zoom != viewer.zoom {
                viewer.zoom = zoom;
                viewer.fit_active = false;
                viewer.auto_fit_pending = false;
                cx.notify();
            }
        }
    }

    fn fit_relationship_width(&mut self, item_id: u64, cx: &mut Context<Self>) {
        if let Some(viewer) = self.relationship_viewers.get_mut(&item_id) {
            if viewer.fit_active {
                viewer.zoom = 1.0;
                viewer.details_open = true;
                viewer.fit_active = false;
                viewer.auto_fit_pending = false;
                cx.notify();
                return;
            }
            let bounds = viewer.scroll.bounds().size;
            let width = f32::from(bounds.width);
            if width <= 0.0 {
                return;
            }
            let available_width = width + if viewer.details_open { 300.0 } else { 0.0 };
            let zoom = relationship_fit_zoom(
                available_width,
                f32::from(bounds.height),
                viewer
                    .index
                    .as_ref()
                    .map_or(280.0, |index| index.scene_height),
            );
            viewer.zoom = zoom;
            viewer.details_open = false;
            viewer.fit_active = true;
            viewer.auto_fit_pending = false;
            viewer.scroll.set_offset(gpui::point(px(0.), px(0.)));
            cx.notify();
        }
    }
}

#[cfg(feature = "benchmark")]
impl WorkspaceShell {
    #[doc(hidden)]
    pub fn seed_relationship_viewer_benchmark(
        &mut self,
        source: DatabaseObjectSource,
        anchor: sift_protocol::CatalogObjectId,
        diagram: sift_protocol::CatalogDiagram,
        details_open: bool,
        cx: &mut Context<Self>,
    ) {
        let item_id = self.next_id;
        self.next_id += 1;
        self.left_dock.presentation.open = false;
        self.right_dock.presentation.open = false;
        self.bottom_dock.presentation.open = false;
        self.panes[self.active_pane].update(cx, |pane, cx| {
            pane.open_relationship_viewer(
                ItemPresentation {
                    id: item_id,
                    kind: ItemKind::Schema,
                    title: "relations · benchmark".into(),
                    dirty: false,
                    source: Some(ItemSource::DatabaseObject(source.clone())),
                    last_result: None,
                },
                source.clone(),
                cx,
            );
            let viewer = pane.relationship_viewers.get_mut(&item_id).unwrap();
            viewer.details_open = details_open;
            let request_id = viewer.start();
            viewer.finish(request_id, Ok((anchor, Box::new(diagram), vec![source])));
            cx.notify();
        });
        cx.notify();
    }

    #[doc(hidden)]
    pub fn scroll_relationship_viewer_benchmark(
        &mut self,
        delta: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.panes[self.active_pane].update(cx, |pane, _| {
            let Some(item_id) = pane.active_item().map(|item| item.id) else {
                return;
            };
            let Some(viewer) = pane.relationship_viewers.get(&item_id) else {
                return;
            };
            let scroll = &viewer.scroll;
            let current = scroll.offset();
            let max = scroll.max_offset();
            let next = (current.x + px(delta)).clamp(-max.x, px(0.));
            scroll.set_offset(gpui::point(next, current.y));
            window.refresh();
        });
    }
}

#[cfg(test)]
mod cardinality_tests {
    use super::*;
    use sift_protocol::{
        CatalogColumnPair, CatalogCompleteness, CatalogEdge, CatalogEdgeCertainty, CatalogNode,
        CatalogNodeDetails, CatalogNodeKind, CatalogObjectId, ColumnMetadata, IndexInfo, IndexKind,
        Nullability, PrimitiveType, TypeRef,
    };

    #[test]
    fn graph_fit_uses_narrower_of_width_and_height() {
        assert_eq!(relationship_fit_zoom(768.0, 800.0, 400.0), 0.5);
        assert_eq!(relationship_fit_zoom(1472.0, 432.0, 800.0), 0.5);
        assert_eq!(relationship_fit_zoom(200.0, 200.0, 800.0), 0.2);
    }

    #[test]
    fn relationship_index_filters_and_lays_out_tables_once_per_search() {
        let table = |id: &str, name: &str| CatalogNode {
            id: CatalogObjectId(id.into()),
            native_id: None,
            kind: CatalogNodeKind::Table,
            name: name.into(),
            qualified_name: format!("lab.{name}"),
            parent_id: None,
            ordinal: None,
            definition_digest: None,
            completeness: CatalogCompleteness::Complete,
            details: CatalogNodeDetails::None,
            extra: Default::default(),
        };
        let diagram = sift_protocol::CatalogDiagram {
            catalog_revision: sift_protocol::CatalogRevision(1),
            catalog_digest: String::new(),
            nodes: vec![table("alpha", "Alpha"), table("beta", "Beta")],
            edges: Vec::new(),
            omitted_nodes: 0,
            omitted_edges: 0,
            inaccessible_boundaries: 0,
            partial: false,
        };
        let anchor = CatalogObjectId("alpha".into());
        let mut index = RelationshipViewerIndex::new(&diagram, "", &anchor);
        assert_eq!(index.visible_tables.len(), 2);
        assert_eq!(index.visible_positions[&anchor], 0);
        assert_eq!(index.positions[&anchor].0, 24.0 + LANE_STEP);
        index.filter("beta");
        assert_eq!(index.visible_tables, vec![CatalogObjectId("beta".into())]);
        assert_eq!(index.visible_positions[&CatalogObjectId("beta".into())], 0);
        assert_eq!(index.positions.len(), 1);
    }

    #[test]
    fn fk_markers_follow_nullability_and_unique_keys() {
        let node = |id: &str, kind, name: &str, parent: Option<&str>, details| CatalogNode {
            id: CatalogObjectId(id.into()),
            native_id: None,
            kind,
            name: name.into(),
            qualified_name: name.into(),
            parent_id: parent.map(|id| CatalogObjectId(id.into())),
            ordinal: None,
            definition_digest: None,
            completeness: CatalogCompleteness::Complete,
            details,
            extra: Default::default(),
        };
        let mut column = ColumnMetadata::new("parent_id", TypeRef::Primitive(PrimitiveType::Int64));
        column.nullable = Nullability::NotNullable;
        let mut catalog = vec![
            node(
                "schema",
                CatalogNodeKind::Schema,
                "public",
                None,
                CatalogNodeDetails::None,
            ),
            node(
                "child",
                CatalogNodeKind::Table,
                "child",
                Some("schema"),
                CatalogNodeDetails::None,
            ),
            node(
                "parent",
                CatalogNodeKind::Table,
                "parent",
                Some("schema"),
                CatalogNodeDetails::None,
            ),
            node(
                "fk",
                CatalogNodeKind::Constraint,
                "child_parent_fk",
                Some("child"),
                CatalogNodeDetails::None,
            ),
            node(
                "column",
                CatalogNodeKind::Column,
                "parent_id",
                Some("child"),
                CatalogNodeDetails::Column { column },
            ),
        ];
        let edge = CatalogEdge {
            from: CatalogObjectId("fk".into()),
            to: Some(CatalogObjectId("parent".into())),
            kind: sift_protocol::CatalogEdgeKind::ForeignKey,
            certainty: CatalogEdgeCertainty::CatalogProven,
            referenced_path: None,
            column_pairs: vec![CatalogColumnPair {
                from: CatalogObjectId("column".into()),
                to: CatalogObjectId("target".into()),
            }],
        };
        let cardinality = |catalog: &Vec<CatalogNode>| {
            let nodes = catalog
                .iter()
                .map(|node| (&node.id, node))
                .collect::<HashMap<_, _>>();
            fk_cardinality(&nodes, &edge)
        };
        assert_eq!(
            cardinality(&catalog),
            (Cardinality::ZeroOrMany, Cardinality::One)
        );
        catalog.push(node(
            "index",
            CatalogNodeKind::Index,
            "child_parent_unique",
            Some("child"),
            CatalogNodeDetails::Index {
                index: IndexInfo {
                    name: "child_parent_unique".into(),
                    columns: vec!["parent_id".into()],
                    unique: true,
                    primary_key: false,
                    kind: IndexKind::Btree,
                    partial_predicate: Some("parent_id > 0".into()),
                },
            },
        ));
        assert_eq!(cardinality(&catalog).0, Cardinality::ZeroOrMany);
        if let CatalogNodeDetails::Index { index } = &mut catalog.last_mut().unwrap().details {
            index.partial_predicate = None;
        }
        assert_eq!(cardinality(&catalog).0, Cardinality::ZeroOrOne);
        if let CatalogNodeDetails::Column { column } = &mut catalog
            .iter_mut()
            .find(|node| node.id.0 == "column")
            .unwrap()
            .details
        {
            column.nullable = Nullability::Nullable;
        }
        assert_eq!(
            cardinality(&catalog),
            (Cardinality::ZeroOrOne, Cardinality::ZeroOrOne)
        );
    }
}
