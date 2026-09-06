//! Automation endpoints on the shared reference client.

use super::*;

impl Client {
    pub async fn run_configurations(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Vec<RunConfiguration>> {
        self.get(&format!(
            "/v1/metadata/workspaces/{}/run-configurations",
            workspace.0
        ))
        .await
    }

    pub async fn latest_successful_run_for_commit(
        &self,
        workspace: WorkspaceId,
        git_commit: &str,
    ) -> Result<Option<Run>> {
        self.get(&format!(
            "/v1/metadata/workspaces/{}/runs/latest-success?git_commit={}",
            workspace.0,
            urlencoding_replace(git_commit)
        ))
        .await
    }

    pub async fn create_run_configuration(
        &self,
        workspace: WorkspaceId,
        request: CreateRunConfigurationRequest,
    ) -> Result<RunConfiguration> {
        self.post(
            &format!("/v1/metadata/workspaces/{}/run-configurations", workspace.0),
            &request,
        )
        .await
    }

    pub async fn run_configuration(
        &self,
        configuration: RunConfigurationId,
    ) -> Result<RunConfiguration> {
        self.get(&format!(
            "/v1/metadata/run-configurations/{}",
            configuration.0
        ))
        .await
    }

    pub async fn update_run_configuration(
        &self,
        configuration: RunConfigurationId,
        request: UpdateRunConfigurationRequest,
    ) -> Result<RunConfiguration> {
        self.put(
            &format!("/v1/metadata/run-configurations/{}", configuration.0),
            &request,
        )
        .await
    }

    pub async fn delete_run_configuration(
        &self,
        configuration: RunConfigurationId,
        request: ExpectedRunConfigurationRevisionRequest,
    ) -> Result<()> {
        self.delete_body(
            &format!("/v1/metadata/run-configurations/{}", configuration.0),
            &request,
        )
        .await
    }

    pub async fn validate_run_configuration(
        &self,
        configuration: RunConfigurationId,
    ) -> Result<RunManifest> {
        self.post_empty(&format!(
            "/v1/metadata/run-configurations/{}/validate",
            configuration.0
        ))
        .await
    }

    pub async fn start_run(
        &self,
        configuration: RunConfigurationId,
        request: StartRunRequest,
    ) -> Result<Run> {
        self.post(
            &format!("/v1/metadata/run-configurations/{}/runs", configuration.0),
            &request,
        )
        .await
    }

    pub async fn run(&self, run: RunId) -> Result<Run> {
        self.get(&format!("/v1/metadata/runs/{}", run.0)).await
    }

    pub async fn run_steps(&self, run: RunId) -> Result<Vec<RunStepResult>> {
        self.get(&format!("/v1/metadata/runs/{}/steps", run.0))
            .await
    }

    pub async fn run_logs(&self, run: RunId, query: RunLogQuery) -> Result<Vec<RunLogEntry>> {
        self.get(&format!(
            "/v1/metadata/runs/{}/logs?after={}&limit={}",
            run.0, query.after, query.limit
        ))
        .await
    }

    pub async fn cancel_run(&self, run: RunId) -> Result<Run> {
        self.post_empty(&format!("/v1/metadata/runs/{}/cancel", run.0))
            .await
    }

    pub async fn rerun(&self, run: RunId, request: StartRunRequest) -> Result<Run> {
        self.post(&format!("/v1/metadata/runs/{}/rerun", run.0), &request)
            .await
    }

    pub async fn run_schedules(
        &self,
        configuration: RunConfigurationId,
    ) -> Result<Vec<RunSchedule>> {
        self.get(&format!(
            "/v1/metadata/run-configurations/{}/schedules",
            configuration.0
        ))
        .await
    }

    pub async fn create_run_schedule(
        &self,
        configuration: RunConfigurationId,
        request: CreateRunScheduleRequest,
    ) -> Result<RunSchedule> {
        self.post(
            &format!(
                "/v1/metadata/run-configurations/{}/schedules",
                configuration.0
            ),
            &request,
        )
        .await
    }

    pub async fn run_schedule(&self, schedule: ScheduleId) -> Result<RunSchedule> {
        self.get(&format!("/v1/metadata/schedules/{}", schedule.0))
            .await
    }

    pub async fn update_run_schedule(
        &self,
        schedule: ScheduleId,
        request: UpdateRunScheduleRequest,
    ) -> Result<RunSchedule> {
        self.put(&format!("/v1/metadata/schedules/{}", schedule.0), &request)
            .await
    }

    pub async fn delete_run_schedule(
        &self,
        schedule: ScheduleId,
        request: ExpectedRunConfigurationRevisionRequest,
    ) -> Result<()> {
        self.delete_body(&format!("/v1/metadata/schedules/{}", schedule.0), &request)
            .await
    }

    pub async fn enable_run_schedule(
        &self,
        schedule: ScheduleId,
        request: ExpectedRunConfigurationRevisionRequest,
    ) -> Result<RunSchedule> {
        self.post(
            &format!("/v1/metadata/schedules/{}/enable", schedule.0),
            &request,
        )
        .await
    }

    pub async fn disable_run_schedule(
        &self,
        schedule: ScheduleId,
        request: ExpectedRunConfigurationRevisionRequest,
    ) -> Result<RunSchedule> {
        self.post(
            &format!("/v1/metadata/schedules/{}/disable", schedule.0),
            &request,
        )
        .await
    }

    pub async fn schedule_occurrences(
        &self,
        schedule: ScheduleId,
        query: ScheduleOccurrenceQuery,
    ) -> Result<Vec<ScheduleOccurrence>> {
        self.get(&format!(
            "/v1/metadata/schedules/{}/occurrences?limit={}",
            schedule.0, query.limit
        ))
        .await
    }

    pub async fn resume_schedule_occurrence(
        &self,
        occurrence: ScheduleOccurrenceId,
    ) -> Result<ScheduleOccurrence> {
        self.post_empty(&format!(
            "/v1/metadata/schedule-occurrences/{}/resume",
            occurrence.0
        ))
        .await
    }
}
