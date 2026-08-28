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
    git_panel_first_frame,
    git_panel_steady_refresh
);
gpui::bench_main!(benches);
