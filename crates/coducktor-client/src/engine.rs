use async_trait::async_trait;
use coducktor_contract::{
    AgentAccountDetailsResponse, AgentAccountStatusResponse, AgentConfigFileContent,
    AgentConfigListing, AgentProfileResponse, AgentProfileSelectionsResponse,
    AgentProfilesResponse, ApiRun, ArchiveFinishedResponse, CancelAutoResumeResponse,
    CancelResponse, ChangesPayload, ConfigResponse, ContinueInput, ContinueResponse,
    CreateAgentProfileInput, CreatePrResponse, CreateRunInput, CreateRunResponse,
    DeleteRunResponse, DeleteWorkflowResponse, EditQueuedMessageResponse, FinishResponse,
    GitCommitInput, GitCommitResponse, GitPushResponse, GithubChecksData, GithubCommentsData,
    GithubData, GithubMergeInput, GithubMergeResponse, GithubPrChangesData,
    GithubPrMergeStateResponse, GithubRefStatusData, GroupResponse, HealthResponse,
    IdeDirectoryResponse, IdeFileResponse, MarkAllReadResponse, MessageInput, MessageResponse,
    OpenAgentAccountFileInput, OpenAgentAccountFileResponse, OpenInCliResponse, OpenInInput,
    OpenProjectInResponse, OpenTargetsResponse, ParsedWorkflow, PatchRunInput, PickVariantRequest,
    PickVariantResponse, PlanResponse, ProjectsResponse, ProviderStatusResponse,
    QueuedMessagePatchInput, ReclaimWorktreesResponse, RemoveAgentProfileResponse,
    RemoveProjectResponse, RemoveQueuedMessageResponse, RemoveTodoResponse, RemoveWorktreeResponse,
    RepoBranchRequest, RepoBranchResponse, RepoCommitPayload, RepoResponse, RunCommitsResponse,
    RunHistoryContext, RunHistoryPage, Runner, RunnerModelCatalogResponse, RunsIndexResponse,
    SaveWorkflowInput, SaveWorkflowResponse, SelectAgentProfileInput, SetAgentConfigInput,
    SetConfigInput, SetWorkspaceConfigInput, SetWorkspaceUiStateInput, Skill, StartTodoResponse,
    TodoItem, UiState, UpdateAgentProfileInput, UpdateProjectInput, UpdateProjectResponse,
    WorkflowsResponse, WorkspaceConfigResponse, WorkspaceUiState, WorkspaceUsageResponse,
    WorktreeEntry, WorktreesResponse,
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

    // ---- GitHub detail reads (spec §8.9, A11) ----------------------------------------------
    /// `GET /github/checks?prs=` — one glyph per PR number, from the server's cache.
    async fn github_checks(
        &self,
        scope: &Scope,
        prs: &[String],
    ) -> Result<GithubChecksData, EngineError>;
    /// `GET /github/ref-status` — reference status (draft/review/checks/merged…) per PR/issue.
    async fn github_ref_status(&self, scope: &Scope) -> Result<GithubRefStatusData, EngineError>;
    /// `GET /github/comments/:kind/:number` — the comment + timeline detail for one item.
    async fn github_comments(
        &self,
        scope: &Scope,
        kind: &str,
        number: u64,
    ) -> Result<GithubCommentsData, EngineError>;
    /// `GET /github/prs/:number/merge-state` — the PR merge gate, checks and eligibility.
    async fn github_pr_merge_state(
        &self,
        scope: &Scope,
        number: u64,
    ) -> Result<GithubPrMergeStateResponse, EngineError>;
    /// `POST /github/prs/:number/merge` — merge with an explicit method + expected head sha.
    async fn github_merge_pr(
        &self,
        scope: &Scope,
        number: u64,
        input: &GithubMergeInput,
    ) -> Result<GithubMergeResponse, EngineError>;
    /// `GET /github/prs/:number/changes` — the PR's file diff (the Changes tab).
    async fn github_pr_changes(
        &self,
        scope: &Scope,
        number: u64,
    ) -> Result<GithubPrChangesData, EngineError>;

    // ---- follow-up inbox (spec §8.12, A11) -------------------------------------------------
    async fn todos(&self, scope: &Scope) -> Result<Vec<TodoItem>, EngineError>;
    async fn delete_todo(&self, scope: &Scope, id: &str)
    -> Result<RemoveTodoResponse, EngineError>;
    /// `POST /todos/:id/start` — no body: the server runs the todo's own suggested task.
    async fn start_todo(&self, scope: &Scope, id: &str) -> Result<StartTodoResponse, EngineError>;

    // ---- workflow builder writes (spec §8.13, A11) -----------------------------------------
    async fn save_workflow(
        &self,
        scope: &Scope,
        input: &SaveWorkflowInput,
    ) -> Result<SaveWorkflowResponse, EngineError>;
    async fn delete_workflow(
        &self,
        scope: &Scope,
        name: &str,
    ) -> Result<DeleteWorkflowResponse, EngineError>;
    async fn parse_workflow(
        &self,
        scope: &Scope,
        yaml: &str,
    ) -> Result<ParsedWorkflow, EngineError>;
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

    // ---- diff engine: task git, repo git, compare (§8.5–§8.7, A9) -----------------------
    async fn run_diff_text(&self, scope: &Scope, run_id: &str) -> Result<String, EngineError>;
    async fn run_changes(&self, scope: &Scope, run_id: &str)
    -> Result<ChangesPayload, EngineError>;
    async fn run_commit(
        &self,
        scope: &Scope,
        run_id: &str,
        sha: &str,
    ) -> Result<RepoCommitPayload, EngineError>;
    async fn run_files(
        &self,
        scope: &Scope,
        run_id: &str,
        path: Option<&str>,
    ) -> Result<WorktreeEntry, EngineError>;
    async fn run_file_raw(
        &self,
        scope: &Scope,
        run_id: &str,
        path: &str,
    ) -> Result<Vec<u8>, EngineError>;
    async fn repo(&self, scope: &Scope) -> Result<RepoResponse, EngineError>;
    async fn repo_changes(&self, scope: &Scope) -> Result<ChangesPayload, EngineError>;
    async fn repo_commit(&self, scope: &Scope, sha: &str)
    -> Result<RepoCommitPayload, EngineError>;
    async fn repo_branch(
        &self,
        scope: &Scope,
        input: &RepoBranchRequest,
    ) -> Result<RepoBranchResponse, EngineError>;
    async fn group(&self, scope: &Scope, group_id: &str) -> Result<GroupResponse, EngineError>;
    async fn pick_variant(
        &self,
        scope: &Scope,
        group_id: &str,
        input: &PickVariantRequest,
    ) -> Result<PickVariantResponse, EngineError>;

    // ---- IDE: project file browser + editor (spec §8.8, A10) ----------------------------
    /// `GET /ide/tree` — one directory listing at the given project-relative path (`None` = root).
    async fn ide_tree(
        &self,
        scope: &Scope,
        path: Option<&str>,
    ) -> Result<IdeDirectoryResponse, EngineError>;
    /// `GET /ide/file` — one file's content, capped at 1 MB by the server.
    async fn ide_file(&self, scope: &Scope, path: &str) -> Result<IdeFileResponse, EngineError>;
    /// `PUT /ide/file` — save `content` to `path`, returning the stored file's metadata.
    async fn ide_save(
        &self,
        scope: &Scope,
        path: &str,
        content: &str,
    ) -> Result<IdeFileResponse, EngineError>;
    async fn cancel_auto_resume(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<CancelAutoResumeResponse, EngineError>;

    // ---- Settings (spec §8.14, A12) ------------------------------------------------------
    /// `PUT /workspace/config` — the global settings slice (Accounts defaults, Resources,
    /// Projects' checkout root).
    async fn put_workspace_config(
        &self,
        input: &SetWorkspaceConfigInput,
    ) -> Result<WorkspaceConfigResponse, EngineError>;
    /// `GET /workspace/ui-state` — the cross-project GUI state (Notifications, appearance).
    async fn workspace_ui_state(&self) -> Result<WorkspaceUiState, EngineError>;
    /// `PUT /workspace/ui-state` — shallow top-level merge, same semantics as `put_ui_state`.
    async fn put_workspace_ui_state(
        &self,
        input: &SetWorkspaceUiStateInput,
    ) -> Result<WorkspaceUiState, EngineError>;
    /// `GET /agent-config` — the selected project's agent-owned config catalog.
    async fn agent_config(&self, scope: &Scope) -> Result<AgentConfigListing, EngineError>;
    /// `GET /agent-config/:id` — one config file's raw contents.
    async fn agent_config_file(
        &self,
        scope: &Scope,
        id: &str,
    ) -> Result<AgentConfigFileContent, EngineError>;
    /// `PUT /agent-config/:id` — save one config file.
    async fn put_agent_config_file(
        &self,
        scope: &Scope,
        id: &str,
        input: &SetAgentConfigInput,
    ) -> Result<AgentConfigFileContent, EngineError>;
    /// `POST /workspace/agent-profiles` — register an extra config dir as an account.
    async fn create_agent_profile(
        &self,
        input: &CreateAgentProfileInput,
    ) -> Result<AgentProfileResponse, EngineError>;
    /// `PATCH /workspace/agent-profiles/:id` — rename an account or repoint its folder.
    async fn update_agent_profile(
        &self,
        id: &str,
        input: &UpdateAgentProfileInput,
    ) -> Result<AgentProfileResponse, EngineError>;
    /// `DELETE /workspace/agent-profiles/:id` — deregister an account.
    async fn remove_agent_profile(
        &self,
        id: &str,
    ) -> Result<RemoveAgentProfileResponse, EngineError>;
    /// `GET /workspace/agent-profiles/:id/status` — one account's auth state, probed for real.
    async fn agent_account_status(
        &self,
        id: &str,
        refresh: bool,
    ) -> Result<AgentAccountStatusResponse, EngineError>;
    /// `GET /workspace/agent-profiles/:id/details` — who an account is signed in as.
    async fn agent_account_details(
        &self,
        id: &str,
    ) -> Result<AgentAccountDetailsResponse, EngineError>;
    /// `POST /workspace/agent-profiles/:id/open` — open one of an account's config files.
    async fn open_agent_account_file(
        &self,
        id: &str,
        input: &OpenAgentAccountFileInput,
    ) -> Result<OpenAgentAccountFileResponse, EngineError>;
    /// `PUT /workspace/agent-profiles/selection` — point one project's provider at an account.
    async fn select_agent_profile(
        &self,
        input: &SelectAgentProfileInput,
    ) -> Result<AgentProfileSelectionsResponse, EngineError>;
    /// `DELETE /projects/:projectId` — deregister a project (registry-only).
    async fn remove_project(&self, project_id: &str) -> Result<RemoveProjectResponse, EngineError>;
    /// `PATCH /projects/:projectId` — the per-project concurrency ceiling and tags.
    async fn update_project(
        &self,
        project_id: &str,
        input: &UpdateProjectInput,
    ) -> Result<UpdateProjectResponse, EngineError>;
    /// `GET /worktrees` — every materialized task worktree, disk usage and retention state.
    async fn worktrees(&self, scope: &Scope) -> Result<WorktreesResponse, EngineError>;
    /// `POST /worktrees/reclaim` — force the retention enforcer to reclaim over-limit worktrees.
    async fn reclaim_worktrees(
        &self,
        scope: &Scope,
    ) -> Result<ReclaimWorktreesResponse, EngineError>;
    /// `POST /runs/:id/remove-worktree` — reclaim one run's worktree and its branch.
    async fn remove_run_worktree(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<RemoveWorktreeResponse, EngineError>;
    /// `GET /open-targets` — the local editors/file-manager/terminal this machine can open.
    async fn open_targets(&self, scope: &Scope) -> Result<OpenTargetsResponse, EngineError>;
    /// `POST /open-in` — open the active project's own folder in the chosen local app.
    async fn open_project_in(
        &self,
        scope: &Scope,
        target: &str,
    ) -> Result<OpenProjectInResponse, EngineError>;

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

    async fn github_checks(
        &self,
        scope: &Scope,
        prs: &[String],
    ) -> Result<GithubChecksData, EngineError> {
        HttpEngine::github_checks(self, scope, prs).await
    }

    async fn github_ref_status(&self, scope: &Scope) -> Result<GithubRefStatusData, EngineError> {
        HttpEngine::github_ref_status(self, scope).await
    }

    async fn github_comments(
        &self,
        scope: &Scope,
        kind: &str,
        number: u64,
    ) -> Result<GithubCommentsData, EngineError> {
        HttpEngine::github_comments(self, scope, kind, number).await
    }

    async fn github_pr_merge_state(
        &self,
        scope: &Scope,
        number: u64,
    ) -> Result<GithubPrMergeStateResponse, EngineError> {
        HttpEngine::github_pr_merge_state(self, scope, number).await
    }

    async fn github_merge_pr(
        &self,
        scope: &Scope,
        number: u64,
        input: &GithubMergeInput,
    ) -> Result<GithubMergeResponse, EngineError> {
        HttpEngine::github_merge_pr(self, scope, number, input).await
    }

    async fn github_pr_changes(
        &self,
        scope: &Scope,
        number: u64,
    ) -> Result<GithubPrChangesData, EngineError> {
        HttpEngine::github_pr_changes(self, scope, number).await
    }

    async fn todos(&self, scope: &Scope) -> Result<Vec<TodoItem>, EngineError> {
        HttpEngine::todos(self, scope).await
    }

    async fn delete_todo(
        &self,
        scope: &Scope,
        id: &str,
    ) -> Result<RemoveTodoResponse, EngineError> {
        HttpEngine::delete_todo(self, scope, id).await
    }

    async fn start_todo(&self, scope: &Scope, id: &str) -> Result<StartTodoResponse, EngineError> {
        HttpEngine::start_todo(self, scope, id).await
    }

    async fn save_workflow(
        &self,
        scope: &Scope,
        input: &SaveWorkflowInput,
    ) -> Result<SaveWorkflowResponse, EngineError> {
        HttpEngine::save_workflow(self, scope, input).await
    }

    async fn delete_workflow(
        &self,
        scope: &Scope,
        name: &str,
    ) -> Result<DeleteWorkflowResponse, EngineError> {
        HttpEngine::delete_workflow(self, scope, name).await
    }

    async fn parse_workflow(
        &self,
        scope: &Scope,
        yaml: &str,
    ) -> Result<ParsedWorkflow, EngineError> {
        HttpEngine::parse_workflow(self, scope, yaml).await
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

    async fn run_diff_text(&self, scope: &Scope, run_id: &str) -> Result<String, EngineError> {
        HttpEngine::run_diff_text(self, scope, run_id).await
    }

    async fn run_changes(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<ChangesPayload, EngineError> {
        HttpEngine::run_changes(self, scope, run_id).await
    }

    async fn run_commit(
        &self,
        scope: &Scope,
        run_id: &str,
        sha: &str,
    ) -> Result<RepoCommitPayload, EngineError> {
        HttpEngine::run_commit(self, scope, run_id, sha).await
    }

    async fn run_files(
        &self,
        scope: &Scope,
        run_id: &str,
        path: Option<&str>,
    ) -> Result<WorktreeEntry, EngineError> {
        HttpEngine::run_files(self, scope, run_id, path).await
    }

    async fn run_file_raw(
        &self,
        scope: &Scope,
        run_id: &str,
        path: &str,
    ) -> Result<Vec<u8>, EngineError> {
        HttpEngine::run_file_raw(self, scope, run_id, path).await
    }

    async fn repo(&self, scope: &Scope) -> Result<RepoResponse, EngineError> {
        HttpEngine::repo(self, scope).await
    }

    async fn repo_changes(&self, scope: &Scope) -> Result<ChangesPayload, EngineError> {
        HttpEngine::repo_changes(self, scope).await
    }

    async fn repo_commit(
        &self,
        scope: &Scope,
        sha: &str,
    ) -> Result<RepoCommitPayload, EngineError> {
        HttpEngine::repo_commit(self, scope, sha).await
    }

    async fn repo_branch(
        &self,
        scope: &Scope,
        input: &RepoBranchRequest,
    ) -> Result<RepoBranchResponse, EngineError> {
        HttpEngine::repo_branch(self, scope, input).await
    }

    async fn group(&self, scope: &Scope, group_id: &str) -> Result<GroupResponse, EngineError> {
        HttpEngine::group(self, scope, group_id).await
    }

    async fn pick_variant(
        &self,
        scope: &Scope,
        group_id: &str,
        input: &PickVariantRequest,
    ) -> Result<PickVariantResponse, EngineError> {
        HttpEngine::pick_variant(self, scope, group_id, input).await
    }

    async fn ide_tree(
        &self,
        scope: &Scope,
        path: Option<&str>,
    ) -> Result<IdeDirectoryResponse, EngineError> {
        HttpEngine::ide_tree(self, scope, path).await
    }

    async fn ide_file(&self, scope: &Scope, path: &str) -> Result<IdeFileResponse, EngineError> {
        HttpEngine::ide_file(self, scope, path).await
    }

    async fn ide_save(
        &self,
        scope: &Scope,
        path: &str,
        content: &str,
    ) -> Result<IdeFileResponse, EngineError> {
        HttpEngine::ide_save(self, scope, path, content).await
    }

    async fn cancel_auto_resume(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<CancelAutoResumeResponse, EngineError> {
        HttpEngine::cancel_auto_resume(self, scope, run_id).await
    }

    async fn put_workspace_config(
        &self,
        input: &SetWorkspaceConfigInput,
    ) -> Result<WorkspaceConfigResponse, EngineError> {
        HttpEngine::put_workspace_config(self, input).await
    }

    async fn workspace_ui_state(&self) -> Result<WorkspaceUiState, EngineError> {
        HttpEngine::workspace_ui_state(self).await
    }

    async fn put_workspace_ui_state(
        &self,
        input: &SetWorkspaceUiStateInput,
    ) -> Result<WorkspaceUiState, EngineError> {
        HttpEngine::put_workspace_ui_state(self, input).await
    }

    async fn agent_config(&self, scope: &Scope) -> Result<AgentConfigListing, EngineError> {
        HttpEngine::agent_config(self, scope).await
    }

    async fn agent_config_file(
        &self,
        scope: &Scope,
        id: &str,
    ) -> Result<AgentConfigFileContent, EngineError> {
        HttpEngine::agent_config_file(self, scope, id).await
    }

    async fn put_agent_config_file(
        &self,
        scope: &Scope,
        id: &str,
        input: &SetAgentConfigInput,
    ) -> Result<AgentConfigFileContent, EngineError> {
        HttpEngine::put_agent_config_file(self, scope, id, input).await
    }

    async fn create_agent_profile(
        &self,
        input: &CreateAgentProfileInput,
    ) -> Result<AgentProfileResponse, EngineError> {
        HttpEngine::create_agent_profile(self, input).await
    }

    async fn update_agent_profile(
        &self,
        id: &str,
        input: &UpdateAgentProfileInput,
    ) -> Result<AgentProfileResponse, EngineError> {
        HttpEngine::update_agent_profile(self, id, input).await
    }

    async fn remove_agent_profile(
        &self,
        id: &str,
    ) -> Result<RemoveAgentProfileResponse, EngineError> {
        HttpEngine::remove_agent_profile(self, id).await
    }

    async fn agent_account_status(
        &self,
        id: &str,
        refresh: bool,
    ) -> Result<AgentAccountStatusResponse, EngineError> {
        HttpEngine::agent_account_status(self, id, refresh).await
    }

    async fn agent_account_details(
        &self,
        id: &str,
    ) -> Result<AgentAccountDetailsResponse, EngineError> {
        HttpEngine::agent_account_details(self, id).await
    }

    async fn open_agent_account_file(
        &self,
        id: &str,
        input: &OpenAgentAccountFileInput,
    ) -> Result<OpenAgentAccountFileResponse, EngineError> {
        HttpEngine::open_agent_account_file(self, id, input).await
    }

    async fn select_agent_profile(
        &self,
        input: &SelectAgentProfileInput,
    ) -> Result<AgentProfileSelectionsResponse, EngineError> {
        HttpEngine::select_agent_profile(self, input).await
    }

    async fn remove_project(&self, project_id: &str) -> Result<RemoveProjectResponse, EngineError> {
        HttpEngine::remove_project(self, project_id).await
    }

    async fn update_project(
        &self,
        project_id: &str,
        input: &UpdateProjectInput,
    ) -> Result<UpdateProjectResponse, EngineError> {
        HttpEngine::update_project(self, project_id, input).await
    }

    async fn worktrees(&self, scope: &Scope) -> Result<WorktreesResponse, EngineError> {
        HttpEngine::worktrees(self, scope).await
    }

    async fn reclaim_worktrees(
        &self,
        scope: &Scope,
    ) -> Result<ReclaimWorktreesResponse, EngineError> {
        HttpEngine::reclaim_worktrees(self, scope).await
    }

    async fn remove_run_worktree(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<RemoveWorktreeResponse, EngineError> {
        HttpEngine::remove_run_worktree(self, scope, run_id).await
    }

    async fn open_targets(&self, scope: &Scope) -> Result<OpenTargetsResponse, EngineError> {
        HttpEngine::open_targets(self, scope).await
    }

    async fn open_project_in(
        &self,
        scope: &Scope,
        target: &str,
    ) -> Result<OpenProjectInResponse, EngineError> {
        HttpEngine::open_project_in(self, scope, target).await
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
