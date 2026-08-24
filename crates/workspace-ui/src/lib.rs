//! GPUI-owned projection of one server-authoritative Sift workspace.

pub mod editor;
mod lifecycle;
mod presentation;
pub mod results;
mod settings;
mod shell;

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
pub use settings::{EditorMode, EditorSettings, SettingsStore, UserSettings};
pub use shell::{
    CancelExecution, CloseActiveItem, CloseActivePane, CommandDefinition, CommandId,
    CommandRegistry, CommandSpec, ConnectionStatus, DismissModal, Dock, DockDefinition, DockId,
    DockPlacement, DockRegistry, ExecutorCommand, ExecutorEvent, FocusNextPane, InstanceCommand,
    InstanceConfigurationPresentation, InstanceCredentialKind, InstanceCredentialPresentation,
    InstanceManagerEvent, InstancePlanPresentation, ItemDefinition, ItemRegistry, ItemRuntimeKind,
    Modal, OpenCommandPalette, OpenSchemaSearch, OpenServerConnection, PaletteConfirm, PaletteDown,
    PaletteUp, Pane, PaneEvent, RoomDocumentCommand, RoomDocumentEvent, SaveActiveItem,
    SavedInstanceRoot, SavedServerProfile, SplitPane, StatusBar, Toast, ToastTone,
    ToggleBottomDock, ToggleLeftDock, ToggleRightDock, WorkspaceShell,
};
