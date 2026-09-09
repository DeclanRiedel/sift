//! Preferred content widths. Viewport constraints belong to the modal host.

use super::{DatabaseWizardStep, Modal};
use gpui::{div, prelude::*, px, Div, IntoElement, Pixels, Stateful};
use sift_ui::{ThemeColors, ThemeMetrics};

/// Shared frame for every modal, including the viewport-relative results view.
/// Content and Vim focus behavior stay with the owning shell feature.
pub(super) fn card(
    data_results: bool,
    padded: bool,
    width: f32,
    max_height: Pixels,
    colors: ThemeColors,
    metrics: ThemeMetrics,
) -> Stateful<Div> {
    div()
        .id("modal-card")
        .debug_selector(|| "modal-card".into())
        .occlude()
        .when(!data_results, |card| card.w_full().max_w(px(width)))
        .when(data_results, |card| {
            card.w(gpui::relative(0.985)).h(gpui::relative(0.985))
        })
        .when(!data_results, |card| {
            card.max_h(max_height).overflow_scroll()
        })
        .flex()
        .flex_col()
        .when(padded, |card| card.p_3())
        .when(data_results, |card| card.overflow_hidden())
        .rounded(metrics.radius_large)
        .border_1()
        .border_color(colors.strong_border)
        .bg(colors.panel)
        .shadow_lg()
}

/// A constrained dialog with independently scrollable content and persistent
/// actions. The host's padding and border must fit inside the card's height cap.
pub(super) fn dialog(
    body: impl IntoElement,
    actions: impl IntoElement,
    max_card_height: Pixels,
) -> Div {
    div()
        .w_full()
        .min_h_0()
        .max_h((max_card_height - px(26.)).max(px(1.)))
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .id("dialog-body")
                .debug_selector(|| "dialog-body".into())
                .min_h_0()
                .overflow_y_scroll()
                .child(body),
        )
        .child(
            div()
                .debug_selector(|| "dialog-actions".into())
                .flex_none()
                .child(actions),
        )
}

pub(super) fn actions() -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_end()
        .gap_2()
}

pub(super) fn content_width(modal: &Modal, wizard: DatabaseWizardStep) -> f32 {
    match modal {
        Modal::DataResults(_) => 0.0, // Uses its own viewport-relative layout.
        Modal::ServerPicker | Modal::Account => 360.0,
        Modal::DatabaseConnection => match wizard {
            DatabaseWizardStep::Provider => 760.0,
            DatabaseWizardStep::Details => 900.0,
            DatabaseWizardStep::Review => 720.0,
        },
        Modal::CatalogDiagram | Modal::WorkspaceReconcile => 1040.0,
        Modal::ChangeLedger => 980.0,
        Modal::DataSearch | Modal::TransferRecipes => 900.0,
        Modal::CsvImport | Modal::RepositoryConflict | Modal::SemanticRename => 860.0,
        Modal::Snippets => 820.0,
        Modal::RepositoryHistory => 780.0,
        Modal::RoomAdministration
        | Modal::RepositoryCommitDetail
        | Modal::RepositoryHistoricalFile => 760.0,
        Modal::CommandPalette
        | Modal::QueryParameters
        | Modal::EditResultCell
        | Modal::PlanCaptures
        | Modal::BenchmarkLibrary
        | Modal::InstanceSetup
        | Modal::Settings
        | Modal::Themes
        | Modal::Keymaps
        | Modal::ApiTokens
        | Modal::ConnectionPolicy
        | Modal::TenantUsage
        | Modal::VcsDiagnostics
        | Modal::Administration
        | Modal::CatalogMigration
        | Modal::RepositoryHosting
        | Modal::ObjectPeek => 720.0,
        Modal::DdlSources | Modal::RepositoryComparison => 700.0,
        Modal::CatalogSnapshots | Modal::WorkspaceHistory | Modal::VaultItemDetails => 680.0,
        Modal::RepositoryBranches => 640.0,
        Modal::RepositoryCommit => 620.0,
        Modal::ConfirmTransactionDisconnect
        | Modal::ConfirmProductionExecution
        | Modal::ConfirmOutcomeUnknownRerun(_, _)
        | Modal::ServerConnection
        | Modal::ConnectionUrl
        | Modal::ConfirmDeleteConnection(_)
        | Modal::ConfirmTerminateProcess(_)
        | Modal::ConfirmRepositoryUncommit
        | Modal::ConfirmRepositoryDiscard(_)
        | Modal::ConfirmRepositoryHunkRevert { .. }
        | Modal::RepositoryRenameBranch(_)
        | Modal::RepositoryBranchFromCheckpoint(_)
        | Modal::RepositorySetUpstream(_)
        | Modal::ConfirmRepositoryDeleteBranch { .. }
        | Modal::RepositorySetup
        | Modal::RepositoryRemotes
        | Modal::WorkspaceCreateFile
        | Modal::WorkspaceCreateFolder
        | Modal::WorkspaceMove
        | Modal::ConfirmWorkspaceDelete
        | Modal::WorkspaceCheckpoint
        | Modal::ConfirmWorkspaceRestore(_)
        | Modal::CreateVault
        | Modal::EditVault
        | Modal::CreateVaultItem
        | Modal::ConfirmDeleteDatabaseObject => 520.0,
    }
}
