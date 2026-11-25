use gpui::{div, prelude::*, Context, Render, Task, Window};
use sift_desktop_updater::Status;
use sift_ui::{ActiveTheme as _, Button, Clickable as _};
use std::sync::atomic::{AtomicBool, Ordering};

gpui::actions!(sift_desktop_updates, [RestartToUpdate]);

static RESTART_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn restart_requested() -> bool {
    RESTART_REQUESTED.load(Ordering::Acquire)
}

pub fn request_restart() {
    RESTART_REQUESTED.store(true, Ordering::Release);
}

pub struct UpdateBanner {
    status: Status,
    _task: Option<Task<()>>,
}

impl UpdateBanner {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut banner = Self {
            status: Status::default(),
            _task: None,
        };
        if let Some(path) = std::env::var_os("SIFT_DESKTOP_UPDATE_STATUS") {
            let path = std::path::PathBuf::from(path);
            banner._task = Some(cx.spawn(async move |banner, cx| loop {
                let path = path.clone();
                let status = cx
                    .background_spawn(async move { sift_desktop_updater::read_status(&path) })
                    .await;
                if banner
                    .update(cx, |banner, cx| {
                        let status = status.unwrap_or_default();
                        if banner.status != status {
                            banner.status = status;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(30))
                    .await;
            }));
        }
        banner
    }

    pub fn ready(&self) -> bool {
        self.status.pending_version.is_some()
    }
}

impl Render for UpdateBanner {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let message = self
            .status
            .pending_version
            .as_ref()
            .map(|version| format!("Desktop update {version} downloaded"))
            .or_else(|| self.status.error.clone());
        div().flex_none().children(message.map(|message| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .bg(colors.surface)
                .border_b_1()
                .border_color(colors.subtle_border)
                .text_xs()
                .text_color(colors.muted_text)
                .child(div().flex_1().min_w_0().truncate().child(message))
                .children(self.ready().then(|| {
                    Button::new("restart-to-update", "Restart to update").on_click(
                        |_, window, cx| window.dispatch_action(Box::new(RestartToUpdate), cx),
                    )
                }))
        }))
    }
}
