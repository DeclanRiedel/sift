use gpui::{Action as _, BenchAppContext, Keystroke, VisualContext as _};
use sift_protocol::{
    ColumnMetadata, Page, PrimitiveType, RepositoryBindingId, Row, TypeRef, Value, VcsFileState,
    VcsStageState, VcsStatus, VcsStatusEntry, WorkspacePath, WorkspaceRevision,
};
use sift_workspace_ui::editor::{EditorKeymap, QueryDocument, QueryEditor};
use sift_workspace_ui::results::{
    MoveCellDown, MoveCellUp, PreparedResultPage, ResultColumn, ResultData, ResultState,
    ResultsView,
};
use sift_workspace_ui::{PresentationState, UserSettings, WorkspaceShell};

const LARGE_SQL_LINES: usize = 8_000;
const FIRST_PAGE_ROWS: usize = 500;
const RESULT_COLUMNS: usize = 20;
const RETAINED_RESULT_ROWS: usize = 10_000;
const GIT_STATUS_ROWS: usize = 20_000;
const OUTLINE_STATEMENTS: usize = 2_000;
const OUTLINE_SYMBOLS: usize = 4_000;
const CHANGE_LEDGER_ROWS: usize = 1_000;
const SCHEMA_OBJECTS: usize = 100_000;
const VISIBLE_RESULT_SET_TABS: usize = 8;

fn large_sql() -> String {
    (0..LARGE_SQL_LINES)
        .map(|line| format!("select {line} as id, 'payload-{line}' as payload;"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn result_data(row_count: usize) -> ResultData {
    let columns = (0..RESULT_COLUMNS)
        .map(|column| ResultColumn {
            name: format!("column_{column}"),
            type_label: "text".into(),
            nullable: false,
        })
        .collect::<Vec<_>>();
    let rows = (0..row_count)
        .map(|row| {
            Row::new(
                (0..RESULT_COLUMNS)
                    .map(|column| Value::Text(format!("r{row}c{column}")))
                    .collect(),
            )
        })
        .collect();
    ResultData {
        columns,
        rows,
        ..ResultData::default()
    }
}

fn repository_status() -> VcsStatus {
    VcsStatus {
        binding_id: RepositoryBindingId(1),
        workspace_revision: WorkspaceRevision(1),
        binding_revision: 1,
        head_oid: Some("0123456789abcdef0123456789abcdef01234567".into()),
        branch: Some("feature/performance".into()),
        upstream: None,
        operation: None,
        entries: (0..GIT_STATUS_ROWS)
            .map(|index| VcsStatusEntry {
                path: WorkspacePath::new(format!("queries/group-{}/q_{index}.sql", index / 100))
                    .unwrap(),
                previous_path: None,
                state: VcsFileState::Modified,
                stage: if index % 2 == 0 {
                    VcsStageState::Staged
                } else {
                    VcsStageState::Unstaged
                },
                conflict: None,
                pending: None,
                affected_objects: Vec::new(),
                validation_errors: 0,
            })
            .collect(),
        truncated: false,
        observed_at: chrono::Utc::now(),
        validation: None,
    }
}

fn large_schema_snapshot() -> sift_protocol::SchemaSnapshot {
    let mut snapshot = sift_protocol::SchemaSnapshot::empty(sift_protocol::SchemaScope::shallow());
    snapshot.trees.push(sift_protocol::CatalogTree {
        name: "warehouse".into(),
        schemas: vec![sift_protocol::SchemaTree {
            name: "public".into(),
            objects: (0..SCHEMA_OBJECTS)
                .map(|index| {
                    sift_protocol::ObjectInfo::new(
                        format!("relation_{index:06}"),
                        sift_protocol::ObjectKind::Table,
                    )
                })
                .collect(),
        }],
    });
    snapshot
}

#[gpui::bench(fps = 120)]
fn vim_typing_large_document(cx: &mut BenchAppContext) {
    let mut window = cx.add_empty_window();
    let editor = window
        .replace_root_view(|_, cx| {
            QueryEditor::new(QueryDocument::with_random_peer(&large_sql()), cx)
                .with_keymap(EditorKeymap::Vim)
        })
        .unwrap();
    window.focus(&editor).unwrap();
    window.update(|window, cx| {
        window.dispatch_keystroke(Keystroke::parse("i").unwrap(), cx);
    });

    let insert = Keystroke::parse("x").unwrap();
    let backspace = Keystroke::parse("backspace").unwrap();
    cx.bench_iter(|_| {
        window.update(|window, cx| {
            window.dispatch_keystroke(insert.clone(), cx);
            window.dispatch_keystroke(backspace.clone(), cx);
        });
    });
}

fn outline_statements() -> Vec<sift_protocol::SemanticStatement> {
    (0..OUTLINE_STATEMENTS)
        .map(|index| {
            let start = (index * 64) as u32;
            sift_protocol::SemanticStatement {
                statement_id: format!("statement-{index}"),
                ordinal: index as u32,
                full_range: sift_protocol::TextRange {
                    start,
                    end: start + 63,
                },
                executable_range: sift_protocol::TextRange {
                    start,
                    end: start + 63,
                },
                kind: sift_protocol::StatementKind::Query,
                recovered: index % 97 == 0,
            }
        })
        .collect()
}

fn outline_symbols() -> Vec<sift_protocol::SemanticOutlineSymbol> {
    (0..OUTLINE_SYMBOLS)
        .map(|index| {
            let start = (index * 32) as u32;
            sift_protocol::SemanticOutlineSymbol {
                symbol_id: format!("symbol-{index}"),
                statement_id: format!("statement-{}", index / 2),
                kind: if index % 2 == 0 {
                    sift_protocol::SemanticOutlineSymbolKind::Cte
                } else {
                    sift_protocol::SemanticOutlineSymbolKind::Object
                },
                name: format!("relation_{index}"),
                range: sift_protocol::TextRange {
                    start,
                    end: start + 31,
                },
                definition_range: None,
                alias: (index % 3 == 0).then(|| format!("alias_{index}")),
                target: Some(format!("public.relation_{index}")),
                usage_kind: sift_protocol::SqlUsageKind::Read,
            }
        })
        .collect()
}

fn change_ledger_entries() -> Vec<sift_protocol::ChangeLedgerEntry> {
    let at: chrono::DateTime<chrono::Utc> = "2026-08-29T12:00:00Z".parse().unwrap();
    (0..CHANGE_LEDGER_ROWS)
        .map(|index| sift_protocol::ChangeLedgerEntry {
            id: index as i64,
            at,
            tenant_id: Some(1),
            room_id: None,
            connection_profile_id: Some(2),
            database_target: Some("warehouse".into()),
            operation: sift_protocol::ChangeLedgerOperation::GridUpdate,
            affected_object: Some(format!("public.table_{index}")),
            row_count: Some(index as i64),
            sql_fingerprint: Some(format!("fingerprint-{index}")),
            row_identity_fingerprint: None,
            transaction_id: None,
            correlation_id: None,
            workspace_id: None,
            workspace_revision: None,
            checkpoint_id: None,
            workspace_path: None,
            git_commit: Some(format!("{index:040x}")),
            source_workflow: "grid".into(),
            authored_by: None,
            approved_by: None,
            executed_by: 7,
            database_actor: None,
            outcome: sift_protocol::ChangeLedgerOutcome::Committed,
            result_code: None,
            identity_source: sift_protocol::ChangeIdentitySource::Sift,
            identity_confidence: sift_protocol::ChangeIdentityConfidence::Authenticated,
            previous_hash: format!("{index:064x}"),
            entry_hash: format!("{:064x}", index + 1),
        })
        .collect()
}

#[gpui::bench(fps = 120)]
fn command_palette_open(cx: &mut BenchAppContext) {
    let mut window = cx.add_empty_window();
    let shell = window
        .replace_root_view(|window, cx| {
            WorkspaceShell::new(
                PresentationState::default(),
                UserSettings::default(),
                None,
                None,
                window,
                cx,
            )
        })
        .unwrap();
    cx.bench_renderer(shell, move |shell, window, cx| {
        shell.open_command_palette_benchmark(window, cx);
    });
}

#[gpui::bench(fps = 120)]
fn command_palette_arrow_navigation(cx: &mut BenchAppContext) {
    let mut window = cx.add_empty_window();
    let shell = window
        .replace_root_view(|window, cx| {
            WorkspaceShell::new(
                PresentationState::default(),
                UserSettings::default(),
                None,
                None,
                window,
                cx,
            )
        })
        .unwrap();
    window.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_command_palette_benchmark(window, cx);
        });
    });
    let mut down = true;
    cx.bench_renderer(shell, move |shell, window, cx| {
        shell.palette_step_benchmark(down, window, cx);
        down = !down;
    });
}

#[gpui::bench(fps = 120)]
fn command_palette_filter_typing(cx: &mut BenchAppContext) {
    let mut window = cx.add_empty_window();
    let shell = window
        .replace_root_view(|window, cx| {
            WorkspaceShell::new(
                PresentationState::default(),
                UserSettings::default(),
                None,
                None,
                window,
                cx,
            )
        })
        .unwrap();
    window.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_command_palette_benchmark(window, cx);
        });
    });
    let queries = ["q", "qu", "que", "quer", "query"];
    let mut index = 0usize;
    cx.bench_renderer(shell, move |shell, _, cx| {
        shell.set_palette_filter_benchmark(queries[index % queries.len()], cx);
        index += 1;
    });
}

#[gpui::bench(fps = 120)]
fn schema_tree_filter(cx: &mut BenchAppContext) {
    let snapshot = large_schema_snapshot();
    let mut window = cx.add_empty_window();
    let shell = window
        .replace_root_view(|window, cx| {
            WorkspaceShell::new(
                PresentationState::default(),
                UserSettings::default(),
                None,
                None,
                window,
                cx,
            )
        })
        .unwrap();
    window.update(|_, cx| {
        shell.update(cx, |shell, cx| {
            shell.apply_schema_snapshot_benchmark(snapshot, cx);
        });
    });
    let queries = ["relation_9", "relation_99", "relation_999", "relation_9999"];
    let mut index = 0usize;
    cx.bench_renderer(shell, move |shell, _, cx| {
        shell.set_schema_filter_benchmark(queries[index % queries.len()], cx);
        index += 1;
    });
}

#[gpui::bench(fps = 120)]
fn query_outline_first_frame(cx: &mut BenchAppContext) {
    let statements = outline_statements();
    let symbols = outline_symbols();
    let mut window = cx.add_empty_window();
    let shell = window
        .replace_root_view(|window, cx| {
            WorkspaceShell::new(
                PresentationState::default(),
                UserSettings::default(),
                None,
                None,
                window,
                cx,
            )
        })
        .unwrap();
    cx.bench_renderer(shell, move |shell, _, cx| {
        shell.seed_query_outline_benchmark(statements.clone(), symbols.clone(), cx);
    });
}

#[gpui::bench(fps = 120)]
fn query_outline_navigation(cx: &mut BenchAppContext) {
    let statements = outline_statements();
    let symbols = outline_symbols();
    let mut window = cx.add_empty_window();
    let shell = window
        .replace_root_view(|window, cx| {
            WorkspaceShell::new(
                PresentationState::default(),
                UserSettings::default(),
                None,
                None,
                window,
                cx,
            )
        })
        .unwrap();
    window.update(|_, cx| {
        shell.update(cx, |shell, cx| {
            shell.seed_query_outline_benchmark(statements, symbols, cx);
        });
    });
    let mut down = true;
    cx.bench_renderer(shell, move |shell, _, cx| {
        shell.step_query_outline_benchmark(if down { 1 } else { -1 }, cx);
        down = !down;
    });
}

#[gpui::bench(fps = 120)]
fn change_ledger_first_frame(cx: &mut BenchAppContext) {
    let entries = change_ledger_entries();
    let mut window = cx.add_empty_window();
    let shell = window
        .replace_root_view(|window, cx| {
            WorkspaceShell::new(
                PresentationState::default(),
                UserSettings::default(),
                None,
                None,
                window,
                cx,
            )
        })
        .unwrap();
    cx.bench_renderer(shell, move |shell, window, cx| {
        shell.seed_change_ledger_benchmark(entries.clone(), window, cx);
    });
}

#[gpui::bench(fps = 120)]
fn first_result_page(cx: &mut BenchAppContext) {
    // Match production: the executor prepares display strings before the page
    // reaches GPUI. This benchmark measures the UI-thread append and first
    // paint, not work deliberately moved to the background executor.
    let data = result_data(FIRST_PAGE_ROWS);
    let columns = PreparedResultPage::new(Page::NextResult {
        columns: (0..RESULT_COLUMNS)
            .map(|column| {
                ColumnMetadata::new(
                    format!("column_{column}"),
                    TypeRef::Primitive(PrimitiveType::Text),
                )
            })
            .collect(),
    });
    let rows = PreparedResultPage::new(Page::Rows { rows: data.rows });
    let mut window = cx.add_empty_window();
    let results = window
        .replace_root_view(|_, cx| ResultsView::new(cx))
        .unwrap();

    cx.bench_renderer(results, move |results, _, cx| {
        results.begin_stream(cx);
        results.apply_stream_page(columns.clone(), cx);
        results.apply_stream_page(rows.clone(), cx);
    });
}

#[gpui::bench(fps = 120)]
fn retained_grid_navigation(cx: &mut BenchAppContext) {
    let mut window = cx.add_empty_window();
    let results = window
        .replace_root_view(|_, cx| {
            let mut results = ResultsView::new(cx);
            results.set_state(ResultState::Ready(result_data(RETAINED_RESULT_ROWS)), cx);
            results
        })
        .unwrap();
    window.focus(&results).unwrap();

    let mut down = true;
    cx.bench_iter(|_| {
        window.update(|window, cx| {
            let action = if down {
                MoveCellDown.boxed_clone()
            } else {
                MoveCellUp.boxed_clone()
            };
            window.dispatch_action(action, cx);
            down = !down;
        });
    });
}

#[gpui::bench(fps = 120)]
fn result_set_tab_navigation(cx: &mut BenchAppContext) {
    let mut window = cx.add_empty_window();
    let results = window
        .replace_root_view(|_, cx| {
            let mut view = ResultsView::new(cx);
            view.begin_stream(cx);
            for result_set in 0..VISIBLE_RESULT_SET_TABS {
                view.apply_stream_page(
                    Page::NextResult {
                        columns: vec![ColumnMetadata::new(
                            format!("result_{result_set}"),
                            TypeRef::Primitive(PrimitiveType::Int64),
                        )],
                    },
                    cx,
                );
                view.apply_stream_page(
                    Page::Rows {
                        rows: vec![Row::new(vec![Value::Int64(result_set as i64)])],
                    },
                    cx,
                );
            }
            view.apply_stream_page(
                Page::Done {
                    affected_rows: None,
                    warnings: Vec::new(),
                },
                cx,
            );
            view
        })
        .unwrap();
    let mut index = 0usize;
    cx.bench_renderer(results, move |results, _, cx| {
        index = (index + 1) % VISIBLE_RESULT_SET_TABS;
        results.select_result_set_benchmark(index, cx);
    });
}

#[gpui::bench(fps = 120)]
fn git_panel_first_frame(cx: &mut BenchAppContext) {
    let status = repository_status();
    let mut window = cx.add_empty_window();
    let shell = window
        .replace_root_view(|window, cx| {
            WorkspaceShell::new(
                PresentationState::default(),
                UserSettings::default(),
                None,
                None,
                window,
                cx,
            )
        })
        .unwrap();
    cx.bench_renderer(shell, move |shell, _, cx| {
        shell.apply_repository_status_benchmark(status.clone(), true, cx);
    });
}

#[gpui::bench(fps = 120)]
fn git_panel_steady_refresh(cx: &mut BenchAppContext) {
    let status = repository_status();
    let initial = status.clone();
    let mut window = cx.add_empty_window();
    let shell = window
        .replace_root_view(move |window, cx| {
            let mut shell = WorkspaceShell::new(
                PresentationState::default(),
                UserSettings::default(),
                None,
                None,
                window,
                cx,
            );
            shell.apply_repository_status_benchmark(initial, true, cx);
            shell
        })
        .unwrap();
    cx.bench_renderer(shell, move |shell, _, cx| {
        shell.apply_repository_status_benchmark(status.clone(), false, cx);
    });
}

gpui::bench_group!(
    benches,
    vim_typing_large_document,
    first_result_page,
    retained_grid_navigation,
    result_set_tab_navigation,
    git_panel_first_frame,
    git_panel_steady_refresh,
    command_palette_open,
    command_palette_arrow_navigation,
    command_palette_filter_typing,
    schema_tree_filter,
    query_outline_first_frame,
    query_outline_navigation,
    change_ledger_first_frame
);
gpui::bench_main!(benches);
