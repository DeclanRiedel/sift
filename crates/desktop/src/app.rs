use std::sync::Arc;

use gpui::{prelude::*, App, Context, Entity, IntoElement, Window};
use sift_workspace_ui::{PresentationState, PresentationStore, Rect, WorkspaceShell};

use crate::platform::{current_platform, presentation_state_path, PlatformKind};

/// Process-wide desktop services. Product state remains behind the SDK; this
/// object owns only platform and presentation concerns.
pub struct SiftApp {
    pub platform: PlatformKind,
    pub presentation_store: Arc<PresentationStore>,
}

impl SiftApp {
    pub fn new() -> Self {
        Self {
            platform: current_platform(),
            presentation_store: Arc::new(PresentationStore::new(presentation_state_path())),
        }
    }

    pub fn restore(&self, displays: &[Rect]) -> PresentationState {
        self.presentation_store
            .load()
            .recover_for_displays(displays)
    }
}

/// Window-level ownership boundary. Additional windows can each own exactly
/// one virtual workspace without adding product state to `SiftApp`.
pub struct SiftWindow {
    workspace: Entity<WorkspaceShell>,
}

impl SiftWindow {
    pub fn new(
        state: PresentationState,
        store: Arc<PresentationStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace = cx.new(|cx| WorkspaceShell::new(state, Some(store), window, cx));
        Self { workspace }
    }
}

impl gpui::Render for SiftWindow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.workspace.clone()
    }
}

pub fn display_rects(cx: &App) -> Vec<Rect> {
    cx.displays()
        .into_iter()
        .map(|display| {
            let bounds = display.bounds();
            Rect {
                x: bounds.origin.x.into(),
                y: bounds.origin.y.into(),
                width: bounds.size.width.into(),
                height: bounds.size.height.into(),
            }
        })
        .collect()
}
