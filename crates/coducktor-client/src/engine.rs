use async_trait::async_trait;
use coducktor_contract::{
    AgentProfilesResponse, ApiRun, ArchiveFinishedResponse, CancelAutoResumeResponse,
    CancelResponse, ConfigResponse, ContinueInput, ContinueResponse, CreatePrResponse,
    CreateRunInput, CreateRunResponse, DeleteRunResponse, EditQueuedMessageResponse,
    FinishResponse, GitCommitInput, GitCommitResponse, GitPushResponse, GithubData, HealthResponse,
    MarkAllReadResponse, MessageInput, MessageResponse, OpenInCliResponse, OpenInInput,
    PatchRunInput, PlanResponse, ProjectsResponse, ProviderStatusResponse, QueuedMessagePatchInput,
    RemoveQueuedMessageResponse, RunCommitsResponse, RunHistoryContext, RunHistoryPage, Runner,
    RunnerModelCatalogResponse, RunsIndexResponse, SetConfigInput, Skill, UiState,
    WorkflowsResponse, WorkspaceConfigResponse, WorkspaceUsageResponse,
};
use futures_core::stream::BoxStream;
use serde_json::Value;

use crate::error::EngineError;
use crate::http::HttpEngine;
use crate::scope::Scope;
use crate::ws::EngineEvent;

/// Input accepted by the engine's start-run seam.
pub type StartRunInput = CreateRunInput;

/// Demand-driven WebSocket topics exposed to screens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Topic {
    Health,
    Todos,
    Run { id: String },
    Named(String),
}

/// The only backend seam the terminal UI is allowed to import.
#[async_trait]
pub trait Engine: Send + Sync {
    async fn health(&self) -> Result<HealthResponse, EngineError>;
    async fn list_runs(&self, scope: &Scope) -> Result<Vec<ApiRun>, EngineError>;
    async fn start_run(
        &self,
        scope: &Scope,
        input: StartRunInput,
    ) -> Result<CreateRunResponse, EngineError>;
    async fn get_run(&self, scope: &Scope, run_id: &str) -> Result<ApiRun, EngineError>;
    async fn archive_run(
        &self,
        scope: &Scope,
        run_id: &str,
        archived: bool,
    ) -> Result<ApiRun, EngineError>;
    async fn delete_run(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<DeleteRunResponse, EngineError>;
    async fn read_run(&self, scope: &Scope, run_id: &str) -> Result<ApiRun, EngineError>;
    async fn unread_run(&self, scope: &Scope, run_id: &str) -> Result<ApiRun, EngineError>;
    async fn archive_finished(&self, scope: &Scope)
    -> Result<ArchiveFinishedResponse, EngineError>;
    async fn mark_all_read(&self, scope: &Scope) -> Result<MarkAllReadResponse, EngineError>;
    async fn runs_index(&self) -> Result<RunsIndexResponse, EngineError>;
    async fn workflows(&self, scope: &Scope) -> Result<WorkflowsResponse, EngineError>;
    async fn skills(&self, scope: &Scope) -> Result<Vec<Skill>, EngineError>;
    async fn projects(&self) -> Result<ProjectsResponse, EngineError>;
    async fn workspace_config(&self) -> Result<WorkspaceConfigResponse, EngineError>;
    async fn workspace_usage(&self) -> Result<WorkspaceUsageResponse, EngineError>;
    async fn config(&self, scope: &Scope) -> Result<ConfigResponse, EngineError>;
    async fn put_config(
        &self,
        scope: &Scope,
        input: &SetConfigInput,
    ) -> Result<ConfigResponse, EngineError>;
    async fn provider_status(&self) -> Result<ProviderStatusResponse, EngineError>;
    async fn models(&self, runner: Runner) -> Result<RunnerModelCatalogResponse, EngineError>;
    async fn github(&self, scope: &Scope) -> Result<GithubData, EngineError>;
    async fn agent_profiles(&self) -> Result<AgentProfilesResponse, EngineError>;
    async fn ui_state(&self, scope: &Scope) -> Result<UiState, EngineError>;
    async fn put_ui_state(&self, scope: &Scope, state: &UiState) -> Result<UiState, EngineError>;
    async fn plan(&self, scope: &Scope, task: &str) -> Result<PlanResponse, EngineError>;

    // ---- task thread (§8.4) ------------------------------------------------------------
    async fn run_history(
        &self,
        scope: &Scope,
        run_id: &str,
        cursor: Option<&str>,
    ) -> Result<RunHistoryPage, EngineError>;
    async fn run_history_context(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<RunHistoryContext, EngineError>;
    async fn patch_run(
        &self,
        scope: &Scope,
        run_id: &str,
        input: PatchRunInput,
    ) -> Result<ApiRun, EngineError>;
    async fn cancel_run(&self, scope: &Scope, run_id: &str) -> Result<CancelResponse, EngineError>;
    async fn send_message(
        &self,
        scope: &Scope,
        run_id: &str,
        input: MessageInput,
    ) -> Result<MessageResponse, EngineError>;
    async fn edit_queued_message(
        &self,
        scope: &Scope,
        run_id: &str,
        message_id: &str,
        input: QueuedMessagePatchInput,
    ) -> Result<EditQueuedMessageResponse, EngineError>;
    async fn remove_queued_message(
        &self,
        scope: &Scope,
        run_id: &str,
        message_id: &str,
    ) -> Result<RemoveQueuedMessageResponse, EngineError>;
    async fn finish_run(&self, scope: &Scope, run_id: &str) -> Result<FinishResponse, EngineError>;
    async fn continue_run(
        &self,
        scope: &Scope,
        run_id: &str,
        input: ContinueInput,
    ) -> Result<ContinueResponse, EngineError>;
    async fn open_in_cli(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<OpenInCliResponse, EngineError>;
    async fn open_in(
        &self,
        scope: &Scope,
        run_id: &str,
        input: OpenInInput,
    ) -> Result<Value, EngineError>;
    async fn git_commit(
        &self,
        scope: &Scope,
        run_id: &str,
        input: GitCommitInput,
    ) -> Result<GitCommitResponse, EngineError>;
    async fn git_push(&self, scope: &Scope, run_id: &str) -> Result<GitPushResponse, EngineError>;
    async fn run_commits(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<RunCommitsResponse, EngineError>;
    async fn create_pr(&self, scope: &Scope, run_id: &str)
    -> Result<CreatePrResponse, EngineError>;
    async fn cancel_auto_resume(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<CancelAutoResumeResponse, EngineError>;

    fn subscribe(&self, topic: Topic) -> BoxStream<'static, EngineEvent>;
}

#[async_trait]
impl Engine for HttpEngine {
    async fn health(&self) -> Result<HealthResponse, EngineError> {
        HttpEngine::health(self).await
    }

    async fn list_runs(&self, scope: &Scope) -> Result<Vec<ApiRun>, EngineError> {
        HttpEngine::list_runs(self, scope).await
    }

    async fn start_run(
        &self,
        scope: &Scope,
        input: StartRunInput,
    ) -> Result<CreateRunResponse, EngineError> {
        HttpEngine::start_run(self, scope, &input).await
    }

    async fn get_run(&self, scope: &Scope, run_id: &str) -> Result<ApiRun, EngineError> {
        HttpEngine::get_run(self, scope, run_id).await
    }

    async fn archive_run(
        &self,
        scope: &Scope,
        run_id: &str,
        archived: bool,
    ) -> Result<ApiRun, EngineError> {
        HttpEngine::archive_run(self, scope, run_id, archived).await
    }

    async fn delete_run(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<DeleteRunResponse, EngineError> {
        HttpEngine::delete_run(self, scope, run_id).await
    }

    async fn read_run(&self, scope: &Scope, run_id: &str) -> Result<ApiRun, EngineError> {
        HttpEngine::read_run(self, scope, run_id).await
    }

    async fn unread_run(&self, scope: &Scope, run_id: &str) -> Result<ApiRun, EngineError> {
        HttpEngine::unread_run(self, scope, run_id).await
    }

    async fn archive_finished(
        &self,
        scope: &Scope,
    ) -> Result<ArchiveFinishedResponse, EngineError> {
        HttpEngine::archive_finished(self, scope).await
    }

    async fn mark_all_read(&self, scope: &Scope) -> Result<MarkAllReadResponse, EngineError> {
        HttpEngine::mark_all_read(self, scope).await
    }

    async fn runs_index(&self) -> Result<RunsIndexResponse, EngineError> {
        HttpEngine::runs_index(self).await
    }

    async fn workflows(&self, scope: &Scope) -> Result<WorkflowsResponse, EngineError> {
        HttpEngine::workflows(self, scope).await
    }

    async fn skills(&self, scope: &Scope) -> Result<Vec<Skill>, EngineError> {
        HttpEngine::skills(self, scope).await
    }

    async fn projects(&self) -> Result<ProjectsResponse, EngineError> {
        HttpEngine::projects(self).await
    }

    async fn workspace_config(&self) -> Result<WorkspaceConfigResponse, EngineError> {
        HttpEngine::workspace_config(self).await
    }

    async fn workspace_usage(&self) -> Result<WorkspaceUsageResponse, EngineError> {
        HttpEngine::workspace_usage(self).await
    }

    async fn config(&self, scope: &Scope) -> Result<ConfigResponse, EngineError> {
        HttpEngine::config(self, scope).await
    }

    async fn put_config(
        &self,
        scope: &Scope,
        input: &SetConfigInput,
    ) -> Result<ConfigResponse, EngineError> {
        HttpEngine::put_config(self, scope, input).await
    }

    async fn provider_status(&self) -> Result<ProviderStatusResponse, EngineError> {
        HttpEngine::provider_status(self).await
    }

    async fn models(&self, runner: Runner) -> Result<RunnerModelCatalogResponse, EngineError> {
        HttpEngine::models(self, runner).await
    }

    async fn github(&self, scope: &Scope) -> Result<GithubData, EngineError> {
        HttpEngine::github(self, scope).await
    }

    async fn agent_profiles(&self) -> Result<AgentProfilesResponse, EngineError> {
        HttpEngine::agent_profiles(self).await
    }

    async fn ui_state(&self, scope: &Scope) -> Result<UiState, EngineError> {
        HttpEngine::ui_state(self, scope).await
    }

    async fn put_ui_state(&self, scope: &Scope, state: &UiState) -> Result<UiState, EngineError> {
        HttpEngine::put_ui_state(self, scope, state).await
    }

    async fn plan(&self, scope: &Scope, task: &str) -> Result<PlanResponse, EngineError> {
        HttpEngine::plan(self, scope, task).await
    }

    async fn run_history(
        &self,
        scope: &Scope,
        run_id: &str,
        cursor: Option<&str>,
    ) -> Result<RunHistoryPage, EngineError> {
        HttpEngine::run_history(self, scope, run_id, cursor).await
    }

    async fn run_history_context(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<RunHistoryContext, EngineError> {
        HttpEngine::run_history_context(self, scope, run_id).await
    }

    async fn patch_run(
        &self,
        scope: &Scope,
        run_id: &str,
        input: PatchRunInput,
    ) -> Result<ApiRun, EngineError> {
        HttpEngine::patch_run(self, scope, run_id, &input).await
    }

    async fn cancel_run(&self, scope: &Scope, run_id: &str) -> Result<CancelResponse, EngineError> {
        HttpEngine::cancel_run(self, scope, run_id).await
    }

    async fn send_message(
        &self,
        scope: &Scope,
        run_id: &str,
        input: MessageInput,
    ) -> Result<MessageResponse, EngineError> {
        HttpEngine::send_message(self, scope, run_id, &input).await
    }

    async fn edit_queued_message(
        &self,
        scope: &Scope,
        run_id: &str,
        message_id: &str,
        input: QueuedMessagePatchInput,
    ) -> Result<EditQueuedMessageResponse, EngineError> {
        HttpEngine::edit_queued_message(self, scope, run_id, message_id, &input).await
    }

    async fn remove_queued_message(
        &self,
        scope: &Scope,
        run_id: &str,
        message_id: &str,
    ) -> Result<RemoveQueuedMessageResponse, EngineError> {
        HttpEngine::remove_queued_message(self, scope, run_id, message_id).await
    }

    async fn finish_run(&self, scope: &Scope, run_id: &str) -> Result<FinishResponse, EngineError> {
        HttpEngine::finish_run(self, scope, run_id).await
    }

    async fn continue_run(
        &self,
        scope: &Scope,
        run_id: &str,
        input: ContinueInput,
    ) -> Result<ContinueResponse, EngineError> {
        HttpEngine::continue_run(self, scope, run_id, &input).await
    }

    async fn open_in_cli(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<OpenInCliResponse, EngineError> {
        HttpEngine::open_in_cli(self, scope, run_id).await
    }

    async fn open_in(
        &self,
        scope: &Scope,
        run_id: &str,
        input: OpenInInput,
    ) -> Result<Value, EngineError> {
        HttpEngine::open_in(self, scope, run_id, &input).await
    }

    async fn git_commit(
        &self,
        scope: &Scope,
        run_id: &str,
        input: GitCommitInput,
    ) -> Result<GitCommitResponse, EngineError> {
        HttpEngine::git_commit(self, scope, run_id, &input).await
    }

    async fn git_push(&self, scope: &Scope, run_id: &str) -> Result<GitPushResponse, EngineError> {
        HttpEngine::git_push(self, scope, run_id).await
    }

    async fn run_commits(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<RunCommitsResponse, EngineError> {
        HttpEngine::run_commits(self, scope, run_id).await
    }

    async fn create_pr(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<CreatePrResponse, EngineError> {
        HttpEngine::create_pr(self, scope, run_id).await
    }

    async fn cancel_auto_resume(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<CancelAutoResumeResponse, EngineError> {
        HttpEngine::cancel_auto_resume(self, scope, run_id).await
    }

    fn subscribe(&self, topic: Topic) -> BoxStream<'static, EngineEvent> {
        let topic = match topic {
            Topic::Health => "health".to_owned(),
            Topic::Todos => "todos".to_owned(),
            Topic::Run { id } => format!("run:{id}"),
            Topic::Named(topic) => topic,
        };
        self.subscribe_topic(topic)
    }
}
