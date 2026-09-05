//! Preferred content widths. Viewport constraints belong to the modal host.

use super::{DatabaseWizardStep, Modal};

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
