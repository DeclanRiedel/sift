//! GPUI-owned projection of one server-authoritative Sift workspace.

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
    AutomationDetailsSnapshot, CancelExecution, CloseActiveItem, CloseActivePane,
    CommandDefinition, CommandId, CommandRegistry, CommandSpec, ConnectionHealthFailure,
    ConnectionHealthReport, ConnectionStatus, DismissModal, Dock, DockDefinition, DockId,
    DockPlacement, DockRegistry, ExecutorCommand, ExecutorEvent, FocusNextPane, InstanceCommand,
    InstanceConfigurationPresentation, InstanceCredentialKind, InstanceCredentialPresentation,
    InstanceManagerEvent, InstancePlanPresentation, ItemDefinition, ItemRegistry, ItemRuntimeKind,
    Modal, OpenCommandPalette, OpenSchemaSearch, OpenServerConnection, PaletteConfirm, PaletteDown,
    PaletteUp, Pane, PaneEvent, PaneNavigateBack, PaneNavigateForward, ResultEditApplyFailure,
    RoomDocumentCommand, RoomDocumentEvent, SaveActiveItem, SavedInstanceRoot, SavedServerKind,
    SavedServerProfile, SplitDirection, SplitPane, StageJsonResultEdit, StatusBar, Toast,
    ToastTone, ToggleBottomDock, ToggleFrameMetrics, ToggleLeftDock, ToggleRightDock,
    WorkspaceShell,
};
pub use workspace::{WorkspaceFileRow, WorkspaceFilesProjection, WorkspaceFilesSnapshot};
