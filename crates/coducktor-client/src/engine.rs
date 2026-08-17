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
    QueuedMessagePatchInput, ReclaimWorktreesResponse, RegisterProjectInput,
    RegisterProjectResponse, RemoveAgentProfileResponse, RemoveProjectResponse,
    RemoveQueuedMessageResponse, RemoveTodoResponse, RemoveWorktreeResponse, RepoBranchRequest,
    RepoBranchResponse, RepoCommitPayload, RepoResponse, RunCommitsResponse, RunHistoryContext,
    RunHistoryPage, Runner, RunnerModelCatalogResponse, RunsIndexResponse, SaveWorkflowInput,
    SaveWorkflowResponse, SelectAgentProfileInput, SetAgentConfigInput, SetConfigInput,
    SetWorkspaceConfigInput, SetWorkspaceUiStateInput, Skill, StartTodoResponse, TodoItem, UiState,
    UpdateAgentProfileInput, UpdateProjectInput, UpdateProjectResponse, WorkflowsResponse,
    WorkspaceConfigResponse, WorkspaceUiState, WorkspaceUsageResponse, WorktreeEntry,
    WorktreesResponse,
};
use futures_core::stream::BoxStream;
use serde_json::Value;

use crate::error::EngineError;
use crate::events::EngineEvent;
use crate::in_process::InProcessEngine;
use crate::scope::Scope;

/// Input accepted by the engine's start-run seam.
pub type StartRunInput = CreateRunInput;

/// Demand-driven live topics exposed to screens.
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
    async fn register_project(
        &self,
        input: &RegisterProjectInput,
    ) -> Result<RegisterProjectResponse, EngineError>;
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

    // ---- GitHub detail reads ---------------------------------------------------------------
    /// Return one status glyph per PR number.
    async fn github_checks(
        &self,
        scope: &Scope,
        prs: &[String],
    ) -> Result<GithubChecksData, EngineError>;
    /// Return reference status (draft/review/checks/merged…) per PR or issue.
    async fn github_ref_status(
        &self,
        scope: &Scope,
        prs: &[String],
        issues: &[String],
    ) -> Result<GithubRefStatusData, EngineError>;
    /// Return comment and timeline detail for one GitHub item.
    async fn github_comments(
        &self,
        scope: &Scope,
        kind: &str,
        number: u64,
    ) -> Result<GithubCommentsData, EngineError>;
    /// Return the PR merge gate, checks, and eligibility.
    async fn github_pr_merge_state(
        &self,
        scope: &Scope,
        number: u64,
    ) -> Result<GithubPrMergeStateResponse, EngineError>;
    /// Merge a PR with an explicit method and expected head SHA.
    async fn github_merge_pr(
        &self,
        scope: &Scope,
        number: u64,
        input: &GithubMergeInput,
    ) -> Result<GithubMergeResponse, EngineError>;
    /// Return a PR's file diff for the Changes tab.
    async fn github_pr_changes(
        &self,
        scope: &Scope,
        number: u64,
    ) -> Result<GithubPrChangesData, EngineError>;

    // ---- follow-up inbox -------------------------------------------------------------------
    async fn todos(&self, scope: &Scope) -> Result<Vec<TodoItem>, EngineError>;
    async fn delete_todo(&self, scope: &Scope, id: &str)
    -> Result<RemoveTodoResponse, EngineError>;
    /// Start the todo's suggested task.
    async fn start_todo(&self, scope: &Scope, id: &str) -> Result<StartTodoResponse, EngineError>;

    // ---- workflow builder writes -----------------------------------------------------------
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

    // ---- task thread ----------------------------------------------------------------------
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

    // ---- diff engine: task git, repo git, compare ------------------------------------------
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

    // ---- IDE: project file browser + editor ------------------------------------------------
    /// Resolve a scope's repository root on disk — a registered project's root or the
    /// engine's workspace root — for the `$EDITOR` handoff.
    fn project_root(&self, scope: &Scope) -> Result<String, EngineError>;
    /// Return one directory listing at the given project-relative path (`None` = root).
    async fn ide_tree(
        &self,
        scope: &Scope,
        path: Option<&str>,
    ) -> Result<IdeDirectoryResponse, EngineError>;
    /// Return one file's content, capped at 1 MB.
    async fn ide_file(&self, scope: &Scope, path: &str) -> Result<IdeFileResponse, EngineError>;
    /// Save `content` to `path`, returning the stored file's metadata.
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

    // ---- Settings --------------------------------------------------------------------------
    /// Update the global settings slice (account defaults, resources, and project checkout root).
    async fn put_workspace_config(
        &self,
        input: &SetWorkspaceConfigInput,
    ) -> Result<WorkspaceConfigResponse, EngineError>;
    /// Return cross-project UI state (notifications and appearance).
    async fn workspace_ui_state(&self) -> Result<WorkspaceUiState, EngineError>;
    /// Shallow-merge cross-project UI state.
    async fn put_workspace_ui_state(
        &self,
        input: &SetWorkspaceUiStateInput,
    ) -> Result<WorkspaceUiState, EngineError>;
    /// Return the selected project's agent-owned config catalog.
    async fn agent_config(&self, scope: &Scope) -> Result<AgentConfigListing, EngineError>;
    /// Return one agent config file's raw contents.
    async fn agent_config_file(
        &self,
        scope: &Scope,
        id: &str,
    ) -> Result<AgentConfigFileContent, EngineError>;
    /// Save one agent config file.
    async fn put_agent_config_file(
        &self,
        scope: &Scope,
        id: &str,
        input: &SetAgentConfigInput,
    ) -> Result<AgentConfigFileContent, EngineError>;
    /// Register an extra config directory as an account.
    async fn create_agent_profile(
        &self,
        input: &CreateAgentProfileInput,
    ) -> Result<AgentProfileResponse, EngineError>;
    /// Rename an account or repoint its folder.
    async fn update_agent_profile(
        &self,
        id: &str,
        input: &UpdateAgentProfileInput,
    ) -> Result<AgentProfileResponse, EngineError>;
    /// Deregister an account.
    async fn remove_agent_profile(
        &self,
        id: &str,
    ) -> Result<RemoveAgentProfileResponse, EngineError>;
    /// Return one account's auth state, probed for real.
    async fn agent_account_status(
        &self,
        id: &str,
        refresh: bool,
    ) -> Result<AgentAccountStatusResponse, EngineError>;
    /// Return who an account is signed in as.
    async fn agent_account_details(
        &self,
        id: &str,
    ) -> Result<AgentAccountDetailsResponse, EngineError>;
    /// Open one of an account's config files.
    async fn open_agent_account_file(
        &self,
        id: &str,
        input: &OpenAgentAccountFileInput,
    ) -> Result<OpenAgentAccountFileResponse, EngineError>;
    /// Point one project's provider at an account.
    async fn select_agent_profile(
        &self,
        input: &SelectAgentProfileInput,
    ) -> Result<AgentProfileSelectionsResponse, EngineError>;
    /// Deregister a project from the workspace registry.
    async fn remove_project(&self, project_id: &str) -> Result<RemoveProjectResponse, EngineError>;
    /// Update a project's concurrency ceiling and tags.
    async fn update_project(
        &self,
        project_id: &str,
        input: &UpdateProjectInput,
    ) -> Result<UpdateProjectResponse, EngineError>;
    /// Return every materialized task worktree, disk usage, and retention state.
    async fn worktrees(&self, scope: &Scope) -> Result<WorktreesResponse, EngineError>;
    /// Force the retention enforcer to reclaim over-limit worktrees.
    async fn reclaim_worktrees(
        &self,
        scope: &Scope,
    ) -> Result<ReclaimWorktreesResponse, EngineError>;
    /// Reclaim one run's worktree and its branch.
    async fn remove_run_worktree(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<RemoveWorktreeResponse, EngineError>;
    /// Return the local editors, file managers, and terminals this machine can open.
    async fn open_targets(&self, scope: &Scope) -> Result<OpenTargetsResponse, EngineError>;
    /// Open the active project's folder in the chosen local app.
    async fn open_project_in(
        &self,
        scope: &Scope,
        target: &str,
    ) -> Result<OpenProjectInResponse, EngineError>;

    fn subscribe(&self, topic: Topic) -> BoxStream<'static, EngineEvent>;
}

fn decode_in_process_ui_state(value: Value) -> Result<UiState, EngineError> {
    serde_json::from_value(value)
        .map_err(|error| EngineError::Transport(format!("invalid in-process ui state: {error}")))
}

#[async_trait]
impl Engine for InProcessEngine {
    async fn health(&self) -> Result<HealthResponse, EngineError> {
        InProcessEngine::health(self).await
    }

    async fn list_runs(&self, _scope: &Scope) -> Result<Vec<ApiRun>, EngineError> {
        InProcessEngine::list_runs(self).await
    }

    async fn start_run(
        &self,
        _scope: &Scope,
        input: StartRunInput,
    ) -> Result<CreateRunResponse, EngineError> {
        InProcessEngine::start_run(self, input).await
    }

    async fn get_run(&self, _scope: &Scope, run_id: &str) -> Result<ApiRun, EngineError> {
        InProcessEngine::get_run(self, run_id).await
    }

    async fn archive_run(
        &self,
        _scope: &Scope,
        run_id: &str,
        archived: bool,
    ) -> Result<ApiRun, EngineError> {
        InProcessEngine::archive_run(self, run_id, archived).await
    }

    async fn delete_run(
        &self,
        _scope: &Scope,
        run_id: &str,
    ) -> Result<DeleteRunResponse, EngineError> {
        InProcessEngine::delete_run(self, run_id).await
    }

    async fn read_run(&self, _scope: &Scope, run_id: &str) -> Result<ApiRun, EngineError> {
        InProcessEngine::read_run(self, run_id).await
    }

    async fn unread_run(&self, _scope: &Scope, run_id: &str) -> Result<ApiRun, EngineError> {
        InProcessEngine::unread_run(self, run_id).await
    }

    async fn archive_finished(
        &self,
        _scope: &Scope,
    ) -> Result<ArchiveFinishedResponse, EngineError> {
        InProcessEngine::archive_finished(self).await
    }

    async fn mark_all_read(&self, _scope: &Scope) -> Result<MarkAllReadResponse, EngineError> {
        InProcessEngine::mark_all_read(self).await
    }

    async fn runs_index(&self) -> Result<RunsIndexResponse, EngineError> {
        InProcessEngine::runs_index(self).await
    }

    async fn workflows(&self, _scope: &Scope) -> Result<WorkflowsResponse, EngineError> {
        InProcessEngine::workflows(self).await
    }

    async fn skills(&self, _scope: &Scope) -> Result<Vec<Skill>, EngineError> {
        InProcessEngine::skills(self).await
    }

    async fn projects(&self) -> Result<ProjectsResponse, EngineError> {
        InProcessEngine::projects(self).await
    }

    async fn register_project(
        &self,
        input: &RegisterProjectInput,
    ) -> Result<RegisterProjectResponse, EngineError> {
        InProcessEngine::register_project(self, input).await
    }

    async fn workspace_config(&self) -> Result<WorkspaceConfigResponse, EngineError> {
        InProcessEngine::workspace_config(self).await
    }

    async fn workspace_usage(&self) -> Result<WorkspaceUsageResponse, EngineError> {
        InProcessEngine::workspace_usage(self).await
    }

    async fn config(&self, _scope: &Scope) -> Result<ConfigResponse, EngineError> {
        InProcessEngine::config(self).await
    }

    async fn put_config(
        &self,
        _scope: &Scope,
        input: &SetConfigInput,
    ) -> Result<ConfigResponse, EngineError> {
        InProcessEngine::put_config(self, input).await
    }

    async fn provider_status(&self) -> Result<ProviderStatusResponse, EngineError> {
        InProcessEngine::provider_status(self).await
    }

    async fn models(&self, runner: Runner) -> Result<RunnerModelCatalogResponse, EngineError> {
        InProcessEngine::models(self, runner).await
    }

    async fn github(&self, _scope: &Scope) -> Result<GithubData, EngineError> {
        InProcessEngine::github(self).await
    }

    async fn github_checks(
        &self,
        _scope: &Scope,
        prs: &[String],
    ) -> Result<GithubChecksData, EngineError> {
        InProcessEngine::github_checks(self, prs).await
    }

    async fn github_ref_status(
        &self,
        _scope: &Scope,
        prs: &[String],
        issues: &[String],
    ) -> Result<GithubRefStatusData, EngineError> {
        InProcessEngine::github_ref_status(self, prs, issues).await
    }

    async fn github_comments(
        &self,
        _scope: &Scope,
        kind: &str,
        number: u64,
    ) -> Result<GithubCommentsData, EngineError> {
        InProcessEngine::github_comments(self, kind, number).await
    }

    async fn github_pr_merge_state(
        &self,
        _scope: &Scope,
        number: u64,
    ) -> Result<GithubPrMergeStateResponse, EngineError> {
        InProcessEngine::github_pr_merge_state(self, number).await
    }

    async fn github_merge_pr(
        &self,
        _scope: &Scope,
        number: u64,
        input: &GithubMergeInput,
    ) -> Result<GithubMergeResponse, EngineError> {
        InProcessEngine::github_merge_pr(self, number, input).await
    }

    async fn github_pr_changes(
        &self,
        _scope: &Scope,
        number: u64,
    ) -> Result<GithubPrChangesData, EngineError> {
        InProcessEngine::github_pr_changes(self, number).await
    }

    async fn todos(&self, _scope: &Scope) -> Result<Vec<TodoItem>, EngineError> {
        InProcessEngine::todos(self).await
    }

    async fn delete_todo(
        &self,
        _scope: &Scope,
        id: &str,
    ) -> Result<RemoveTodoResponse, EngineError> {
        InProcessEngine::delete_todo(self, id).await
    }

    async fn start_todo(&self, _scope: &Scope, id: &str) -> Result<StartTodoResponse, EngineError> {
        InProcessEngine::start_todo(self, id).await
    }

    async fn save_workflow(
        &self,
        _scope: &Scope,
        input: &SaveWorkflowInput,
    ) -> Result<SaveWorkflowResponse, EngineError> {
        InProcessEngine::save_workflow(self, input).await
    }

    async fn delete_workflow(
        &self,
        _scope: &Scope,
        name: &str,
    ) -> Result<DeleteWorkflowResponse, EngineError> {
        InProcessEngine::delete_workflow(self, name).await
    }

    async fn parse_workflow(
        &self,
        _scope: &Scope,
        yaml: &str,
    ) -> Result<ParsedWorkflow, EngineError> {
        InProcessEngine::parse_workflow(self, yaml).await
    }

    async fn agent_profiles(&self) -> Result<AgentProfilesResponse, EngineError> {
        InProcessEngine::agent_profiles(self).await
    }

    async fn ui_state(&self, _scope: &Scope) -> Result<UiState, EngineError> {
        decode_in_process_ui_state(InProcessEngine::ui_state(self).await?)
    }

    async fn put_ui_state(&self, _scope: &Scope, state: &UiState) -> Result<UiState, EngineError> {
        let value = serde_json::to_value(state).map_err(|error| {
            EngineError::Transport(format!("could not encode ui state: {error}"))
        })?;
        decode_in_process_ui_state(InProcessEngine::put_ui_state(self, value).await?)
    }

    async fn plan(&self, _scope: &Scope, task: &str) -> Result<PlanResponse, EngineError> {
        InProcessEngine::plan(self, task).await
    }

    async fn run_history(
        &self,
        _scope: &Scope,
        run_id: &str,
        cursor: Option<&str>,
    ) -> Result<RunHistoryPage, EngineError> {
        InProcessEngine::run_history(self, run_id, cursor).await
    }

    async fn run_history_context(
        &self,
        _scope: &Scope,
        run_id: &str,
    ) -> Result<RunHistoryContext, EngineError> {
        InProcessEngine::run_history_context(self, run_id).await
    }

    async fn patch_run(
        &self,
        _scope: &Scope,
        run_id: &str,
        input: PatchRunInput,
    ) -> Result<ApiRun, EngineError> {
        InProcessEngine::patch_run(self, run_id, input).await
    }

    async fn cancel_run(
        &self,
        _scope: &Scope,
        run_id: &str,
    ) -> Result<CancelResponse, EngineError> {
        InProcessEngine::cancel_run(self, run_id).await
    }

    async fn send_message(
        &self,
        _scope: &Scope,
        run_id: &str,
        input: MessageInput,
    ) -> Result<MessageResponse, EngineError> {
        InProcessEngine::send_message(self, run_id, input).await
    }

    async fn edit_queued_message(
        &self,
        _scope: &Scope,
        run_id: &str,
        message_id: &str,
        input: QueuedMessagePatchInput,
    ) -> Result<EditQueuedMessageResponse, EngineError> {
        InProcessEngine::edit_queued_message(self, run_id, message_id, input).await
    }

    async fn remove_queued_message(
        &self,
        _scope: &Scope,
        run_id: &str,
        message_id: &str,
    ) -> Result<RemoveQueuedMessageResponse, EngineError> {
        InProcessEngine::remove_queued_message(self, run_id, message_id).await
    }

    async fn finish_run(
        &self,
        _scope: &Scope,
        run_id: &str,
    ) -> Result<FinishResponse, EngineError> {
        InProcessEngine::finish_run(self, run_id).await
    }

    async fn continue_run(
        &self,
        _scope: &Scope,
        run_id: &str,
        input: ContinueInput,
    ) -> Result<ContinueResponse, EngineError> {
        InProcessEngine::continue_run(self, run_id, input).await
    }

    async fn open_in_cli(
        &self,
        _scope: &Scope,
        run_id: &str,
    ) -> Result<OpenInCliResponse, EngineError> {
        InProcessEngine::open_in_cli(self, run_id).await
    }

    async fn open_in(
        &self,
        _scope: &Scope,
        run_id: &str,
        input: OpenInInput,
    ) -> Result<Value, EngineError> {
        InProcessEngine::open_in(self, run_id, input).await
    }

    async fn git_commit(
        &self,
        _scope: &Scope,
        run_id: &str,
        input: GitCommitInput,
    ) -> Result<GitCommitResponse, EngineError> {
        InProcessEngine::git_commit(self, run_id, input).await
    }

    async fn git_push(&self, _scope: &Scope, run_id: &str) -> Result<GitPushResponse, EngineError> {
        InProcessEngine::git_push(self, run_id).await
    }

    async fn run_commits(
        &self,
        _scope: &Scope,
        run_id: &str,
    ) -> Result<RunCommitsResponse, EngineError> {
        InProcessEngine::run_commits(self, run_id).await
    }

    async fn create_pr(
        &self,
        _scope: &Scope,
        run_id: &str,
    ) -> Result<CreatePrResponse, EngineError> {
        InProcessEngine::create_pr(self, run_id).await
    }

    async fn run_diff_text(&self, _scope: &Scope, run_id: &str) -> Result<String, EngineError> {
        InProcessEngine::run_diff_text(self, run_id).await
    }

    async fn run_changes(
        &self,
        _scope: &Scope,
        run_id: &str,
    ) -> Result<ChangesPayload, EngineError> {
        InProcessEngine::run_changes(self, run_id).await
    }

    async fn run_commit(
        &self,
        _scope: &Scope,
        run_id: &str,
        sha: &str,
    ) -> Result<RepoCommitPayload, EngineError> {
        InProcessEngine::run_commit(self, run_id, sha).await
    }

    async fn run_files(
        &self,
        _scope: &Scope,
        run_id: &str,
        path: Option<&str>,
    ) -> Result<WorktreeEntry, EngineError> {
        InProcessEngine::run_files(self, run_id, path).await
    }

    async fn run_file_raw(
        &self,
        _scope: &Scope,
        run_id: &str,
        path: &str,
    ) -> Result<Vec<u8>, EngineError> {
        InProcessEngine::run_file_raw(self, run_id, path).await
    }

    async fn repo(&self, scope: &Scope) -> Result<RepoResponse, EngineError> {
        let root = self.root_for_scope(scope)?;
        InProcessEngine::repo_at(root).await
    }

    async fn repo_changes(&self, scope: &Scope) -> Result<ChangesPayload, EngineError> {
        let root = self.root_for_scope(scope)?;
        InProcessEngine::repo_changes_at(root).await
    }

    async fn repo_commit(
        &self,
        scope: &Scope,
        sha: &str,
    ) -> Result<RepoCommitPayload, EngineError> {
        let root = self.root_for_scope(scope)?;
        InProcessEngine::repo_commit_at(root, sha).await
    }

    async fn repo_branch(
        &self,
        scope: &Scope,
        input: &RepoBranchRequest,
    ) -> Result<RepoBranchResponse, EngineError> {
        let root = self.root_for_scope(scope)?;
        InProcessEngine::repo_branch_at(root, input).await
    }

    async fn group(&self, _scope: &Scope, group_id: &str) -> Result<GroupResponse, EngineError> {
        InProcessEngine::group(self, group_id).await
    }

    async fn pick_variant(
        &self,
        _scope: &Scope,
        group_id: &str,
        input: &PickVariantRequest,
    ) -> Result<PickVariantResponse, EngineError> {
        InProcessEngine::pick_variant(self, group_id, input).await
    }

    async fn ide_tree(
        &self,
        scope: &Scope,
        path: Option<&str>,
    ) -> Result<IdeDirectoryResponse, EngineError> {
        let root = self.root_for_scope(scope)?;
        InProcessEngine::ide_tree_at(root, path).await
    }

    fn project_root(&self, scope: &Scope) -> Result<String, EngineError> {
        self.root_for_scope(scope)
            .map(|root| root.display().to_string())
    }

    async fn ide_file(&self, scope: &Scope, path: &str) -> Result<IdeFileResponse, EngineError> {
        let root = self.root_for_scope(scope)?;
        InProcessEngine::ide_file_at(root, path).await
    }

    async fn ide_save(
        &self,
        scope: &Scope,
        path: &str,
        content: &str,
    ) -> Result<IdeFileResponse, EngineError> {
        let root = self.root_for_scope(scope)?;
        InProcessEngine::ide_save_at(root, path, content).await
    }

    async fn cancel_auto_resume(
        &self,
        _scope: &Scope,
        run_id: &str,
    ) -> Result<CancelAutoResumeResponse, EngineError> {
        InProcessEngine::cancel_auto_resume(self, run_id).await
    }

    async fn put_workspace_config(
        &self,
        input: &SetWorkspaceConfigInput,
    ) -> Result<WorkspaceConfigResponse, EngineError> {
        InProcessEngine::put_workspace_config(self, input).await
    }

    async fn workspace_ui_state(&self) -> Result<WorkspaceUiState, EngineError> {
        InProcessEngine::workspace_ui_state(self).await
    }

    async fn put_workspace_ui_state(
        &self,
        input: &SetWorkspaceUiStateInput,
    ) -> Result<WorkspaceUiState, EngineError> {
        InProcessEngine::put_workspace_ui_state(self, input).await
    }

    async fn agent_config(&self, _scope: &Scope) -> Result<AgentConfigListing, EngineError> {
        InProcessEngine::agent_config(self).await
    }

    async fn agent_config_file(
        &self,
        _scope: &Scope,
        id: &str,
    ) -> Result<AgentConfigFileContent, EngineError> {
        InProcessEngine::agent_config_file(self, id).await
    }

    async fn put_agent_config_file(
        &self,
        _scope: &Scope,
        id: &str,
        input: &SetAgentConfigInput,
    ) -> Result<AgentConfigFileContent, EngineError> {
        InProcessEngine::put_agent_config_file(self, id, input).await
    }

    async fn create_agent_profile(
        &self,
        input: &CreateAgentProfileInput,
    ) -> Result<AgentProfileResponse, EngineError> {
        InProcessEngine::create_agent_profile(self, input).await
    }

    async fn update_agent_profile(
        &self,
        id: &str,
        input: &UpdateAgentProfileInput,
    ) -> Result<AgentProfileResponse, EngineError> {
        InProcessEngine::update_agent_profile(self, id, input).await
    }

    async fn remove_agent_profile(
        &self,
        id: &str,
    ) -> Result<RemoveAgentProfileResponse, EngineError> {
        InProcessEngine::remove_agent_profile(self, id).await
    }

    async fn agent_account_status(
        &self,
        id: &str,
        refresh: bool,
    ) -> Result<AgentAccountStatusResponse, EngineError> {
        InProcessEngine::agent_account_status(self, id, refresh).await
    }

    async fn agent_account_details(
        &self,
        id: &str,
    ) -> Result<AgentAccountDetailsResponse, EngineError> {
        InProcessEngine::agent_account_details(self, id).await
    }

    async fn open_agent_account_file(
        &self,
        id: &str,
        input: &OpenAgentAccountFileInput,
    ) -> Result<OpenAgentAccountFileResponse, EngineError> {
        InProcessEngine::open_agent_account_file(self, id, input).await
    }

    async fn select_agent_profile(
        &self,
        input: &SelectAgentProfileInput,
    ) -> Result<AgentProfileSelectionsResponse, EngineError> {
        InProcessEngine::select_agent_profile(self, input).await
    }

    async fn remove_project(&self, project_id: &str) -> Result<RemoveProjectResponse, EngineError> {
        InProcessEngine::remove_project(self, project_id).await
    }

    async fn update_project(
        &self,
        project_id: &str,
        input: &UpdateProjectInput,
    ) -> Result<UpdateProjectResponse, EngineError> {
        InProcessEngine::update_project(self, project_id, input).await
    }

    async fn worktrees(&self, _scope: &Scope) -> Result<WorktreesResponse, EngineError> {
        InProcessEngine::worktrees(self).await
    }

    async fn reclaim_worktrees(
        &self,
        _scope: &Scope,
    ) -> Result<ReclaimWorktreesResponse, EngineError> {
        InProcessEngine::reclaim_worktrees(self).await
    }

    async fn remove_run_worktree(
        &self,
        _scope: &Scope,
        run_id: &str,
    ) -> Result<RemoveWorktreeResponse, EngineError> {
        InProcessEngine::remove_run_worktree(self, run_id).await
    }

    async fn open_targets(&self, _scope: &Scope) -> Result<OpenTargetsResponse, EngineError> {
        InProcessEngine::open_targets(self).await
    }

    async fn open_project_in(
        &self,
        _scope: &Scope,
        target: &str,
    ) -> Result<OpenProjectInResponse, EngineError> {
        InProcessEngine::open_project_in(self, target).await
    }

    fn subscribe(&self, topic: Topic) -> BoxStream<'static, EngineEvent> {
        InProcessEngine::subscribe(self, topic)
    }
}
