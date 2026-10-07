//! Quiet activity feedback and explicit transcript following.
use super::*;

pub(super) fn activity_label(activity: &str) -> &'static str {
    if let Some(tool) = activity.strip_suffix(" running") {
        return match tool {
            "sift_schema" | "sift_catalog" => "Reading schema…",
            "sift_diagnostics" => "Checking SQL…",
            "sift_explain" | "sift_plan_captures" | "sift_plan_capture" => "Reading plans…",
            "sift_select" => "Reading rows…",
            "sift_query_history" => "Reading history…",
            "sift_benchmark_runs" | "sift_benchmark_run" => "Reading benchmarks…",
            "sift_external_tools" => "Listing source tools…",
            "sift_external_read" => "Reading source…",
            "sift_stage_sql" | "sift_stage_database" | "sift_external_stage" => "Preparing draft…",
            _ => "Working…",
        };
    }
    match activity {
        "Responding…" => "Responding…",
        "SQL draft staged for review"
        | "Database changes staged for human review"
        | "Reviewed source intent staged locally for human review" => "Draft ready for review",
        _ if activity.starts_with("Starting ") => "Starting…",
        _ if activity.starts_with("Stopping ") => "Stopping…",
        _ if activity.starts_with("Preparing exact attachment") => "Preparing context…",
        _ if activity.starts_with("Earlier chat turns were omitted") => {
            "Earlier turns omitted · see work log"
        }
        _ => "Thinking…",
    }
}

impl WorkspaceShell {
    pub(super) fn ai_transcript_changed(&mut self) {
        if self.ai.follow_agent {
            self.ai.transcript_scroll.scroll_to_bottom();
        } else {
            self.ai.unseen_content = true;
        }
    }

    pub(super) fn resume_ai_follow(&mut self) {
        self.ai.follow_agent = true;
        self.ai.unseen_content = false;
        self.ai.transcript_scroll.scroll_to_bottom();
    }

    pub(super) fn retain_ai_activity(&mut self, activity: &str) {
        let detail = activity.chars().take(500).collect::<String>();
        if self.ai.live_work_log.last() != Some(&detail) {
            if self.ai.live_work_log.len() == 32 {
                self.ai.live_work_log.remove(0);
            }
            self.ai.live_work_log.push(detail);
        }
    }

    pub(super) fn handle_ai_transcript_scroll_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.ai.transcript_focus.is_focused(window)
            || self.ai.menu_expanded
            || self.ai.thread_picker_expanded
            || self.ai.response_menu.is_some()
            || self.ai.context_choices_open
            || self.ai.settings_expanded
            || self.ai.model_picker_expanded
            || self.ai.permission_picker_expanded
        {
            return false;
        }
        let key = event.keystroke.key.as_str();
        if key == "G" || (key == "g" && event.keystroke.modifiers.shift) || key == "end" {
            self.resume_ai_follow();
            cx.notify();
            return true;
        }
        let delta = match key {
            "k" | "up" => px(24.),
            "j" | "down" => px(-24.),
            "u" if event.keystroke.modifiers.control => {
                self.ai.transcript_scroll.bounds().size.height / 2.
            }
            "d" if event.keystroke.modifiers.control => {
                -self.ai.transcript_scroll.bounds().size.height / 2.
            }
            _ => return false,
        };
        let mut offset = self.ai.transcript_scroll.offset();
        offset.y = (offset.y + delta).clamp(-self.ai.transcript_scroll.max_offset().y, px(0.));
        self.ai.transcript_scroll.set_offset(offset);
        if delta > px(0.) && self.ai.transcript_scroll.max_offset().y > px(0.) {
            self.ai.follow_agent = false;
        }
        cx.notify();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn statuses_are_short_and_do_not_echo_provider_details() {
        assert_eq!(activity_label("sift_schema running"), "Reading schema…");
        assert_eq!(activity_label("sift_select running"), "Reading rows…");
        assert_eq!(activity_label("sift_stage_sql running"), "Preparing draft…");
        assert_eq!(activity_label("sift_schema completed"), "Thinking…");
        assert_eq!(
            activity_label("**Long** provider reasoning\nwith extra details"),
            "Thinking…"
        );
        assert_eq!(
            activity_label("Earlier chat turns were omitted to fit the context limit."),
            "Earlier turns omitted · see work log"
        );
    }
}
