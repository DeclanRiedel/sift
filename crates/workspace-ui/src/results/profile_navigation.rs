//! Local navigation through an accepted live or private saved Profile.
use super::*;

impl ResultsView {
    pub(super) fn replace_profile_plan(
        &mut self,
        response: sift_protocol::ProfileResponse,
        saved: bool,
    ) {
        self.profile_nodes = flatten_plan(&response.plan.root);
        self.profile_findings = plan_findings::findings(&response.plan);
        self.profile_selected_node = (!self.profile_nodes.is_empty()).then_some(0);
        self.profile_scroll_handle
            .scroll_to_item(0, ScrollStrategy::Top);
        self.profile_panel_scroll_handle
            .set_offset(gpui::point(px(0.), px(0.)));
        self.profile_is_saved = saved;
        self.profile_result = Some(response);
    }

    fn profile_navigation_active(&self) -> bool {
        self.tab == ResultTab::Performance && self.performance_section == PerformanceSection::Plan
    }

    pub(super) fn select_captured_profile_node(
        &mut self,
        run_id: Option<uuid::Uuid>,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        if run_id.is_some() && self.profile_result.as_ref().map(|profile| profile.run_id) == run_id
        {
            self.select_profile_node(index, cx);
        }
    }

    pub(super) fn select_profile_node(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.profile_navigation_active() || index >= self.profile_nodes.len() {
            return;
        }
        self.profile_selected_node = Some(index);
        self.profile_scroll_handle
            .scroll_to_item(index, ScrollStrategy::Center);
        cx.notify();
    }

    fn move_profile_node(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.profile_nodes.is_empty() {
            return;
        }
        let index = self
            .profile_selected_node
            .unwrap_or(0)
            .saturating_add_signed(delta)
            .min(self.profile_nodes.len() - 1);
        self.select_profile_node(index, cx);
    }

    fn move_profile_finding(&mut self, forward: bool, cx: &mut Context<Self>) {
        let mut indices: Vec<_> = self
            .profile_findings
            .iter()
            .filter_map(|finding| finding.node_index)
            .collect();
        indices.dedup();
        let selected = self.profile_selected_node.unwrap_or(0);
        let next = if forward {
            indices
                .iter()
                .find(|&&index| index > selected)
                .or(indices.first())
        } else {
            indices
                .iter()
                .rev()
                .find(|&&index| index < selected)
                .or(indices.last())
        };
        if let Some(&index) = next {
            self.select_profile_node(index, cx);
        }
    }

    pub(super) fn previous_profile_node(
        &mut self,
        _: &PreviousProfileNode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_profile_node(-1, cx);
    }
    pub(super) fn next_profile_node(
        &mut self,
        _: &NextProfileNode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_profile_node(1, cx);
    }
    pub(super) fn first_profile_node(
        &mut self,
        _: &FirstProfileNode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_profile_node(0, cx);
    }
    pub(super) fn last_profile_node(
        &mut self,
        _: &LastProfileNode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.profile_nodes.len().checked_sub(1) {
            self.select_profile_node(index, cx);
        }
    }
    pub(super) fn next_profile_finding(
        &mut self,
        _: &NextProfileFinding,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_profile_finding(true, cx);
    }
    pub(super) fn previous_profile_finding(
        &mut self,
        _: &PreviousProfileFinding,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_profile_finding(false, cx);
    }
}
