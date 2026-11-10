//! GPUI-owned projection of one server-authoritative Sift workspace.

mod binary_preview;
pub mod editor;
mod lifecycle;
mod presentation;
mod repository;
pub mod results;
mod settings;
mod shell;
mod workspace;

pub use editor::{QueryDocument, QueryEditor, SemanticOutcome, SemanticRequestKind};
pub use results::{ResultData, ResultState, ResultsView};

pub use lifecycle::{
    create_virtual_workspace, load_instance, stream_room_presence, ConnectionNavEntry,
    ConnectionPhase, DegradedReason, DocumentNavEntry, InstanceCatalog, InstanceKind, InstanceSpec,
    LifecycleEvent, LifecycleProjection, LoadedInstance, PresenceEvent, RoomNavEntry,
    RoomPresenceProjection, TenantNavEntry, WorkspaceNavEntry,
};
pub use presentation::{
    BottomTool, DatabaseObjectSource, DockPresentation, ItemKind, ItemPresentation, ItemSource,
    LeftPanel, PanePresentation, PresentationState, PresentationStore, Rect, ResultReference,
    RoomDocumentSource, WindowPresentation, WorkspacePresentation,
};
pub use settings::{
    DataSettings, EditorMode, EditorSettings, KeyboardProfile, KeyboardSettings, KeymapSettings,
    QueryResultsPlacement, SettingsStore, UserSettings,
};
pub use shell::{
    AccountAction, AccountReply, AccountScope, AiConversationSnapshot, AiDatabaseExecutionView,
    AiExtensionExecutionView, AiExternalSourceAction, AiExternalSourceChoice, AiModelOption,
    AiQueryEditorObservation, AiQueryEditorWait, AiQueryEditorWork, AiQueryExecutionView,
    AiSourceManagerSnapshot, AiViewScope, AutomationDetailsSnapshot, BenchmarkDefinitionAction,
    BenchmarkDefinitionReply, BenchmarkLibraryAction, BenchmarkLibraryReply, CancelExecution,
    CloseActiveItem, CloseActivePane, CommandDefinition, CommandId, CommandRegistry, CommandSpec,
    ConnectionHealthFailure, ConnectionHealthReport, ConnectionStatus, CsvImportSource,
    DismissModal, Dock, DockDefinition, DockId, DockPlacement, DockRegistry, ExecutorCommand,
    ExecutorEvent, ExecutorSender, ExtensionCallerScope, FocusNextPane, GithubOwnerSetupEvent,
    InstanceCommand, InstanceConfigurationPresentation, InstanceCredentialKind,
    InstanceCredentialPresentation, InstanceFieldChangePresentation,
    InstanceGenerationPresentation, InstanceManagerEvent, InstancePlanPresentation,
    InstanceResourceChangePresentation, ItemDefinition, ItemRegistry, ItemRuntimeKind, Modal,
    MonitorActionSource, OpenCommandPalette, OpenSchemaSearch, OpenServerConnection,
    PaletteConfirm, PaletteDown, PaletteUp, Pane, PaneEvent, PaneNavigateBack, PaneNavigateForward,
    ResultEditApplyFailure, RoomDocumentCommand, RoomDocumentEvent, SaveActiveItem,
    SavedInstanceRoot, SavedServerKind, SavedServerProfile, SemanticConnectionTarget,
    SplitDirection, SplitPane, StageJsonResultEdit, StatusBar, Toast, ToastTone, ToggleBottomDock,
    ToggleFrameMetrics, ToggleLeftDock, ToggleRightDock, TransactionConnection,
    TransactionControlTarget, TransactionTarget, TransferMappingAction, TransferMappingReply,
    TransferMappingSource, WorkspaceShell,
};
pub use workspace::{WorkspaceFileRow, WorkspaceFilesProjection, WorkspaceFilesSnapshot};
