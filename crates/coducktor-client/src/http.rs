use std::pin::Pin;

use coducktor_contract::{
    AgentProfilesResponse, ApiRun, ArchiveFinishedResponse, CancelAutoResumeResponse,
    CancelResponse, ChangesPayload, ConfigResponse, ContinueInput, ContinueResponse,
    CreatePrResponse, CreateRunInput, CreateRunResponse, DeleteRunResponse,
    EditQueuedMessageResponse, FinishResponse, GitCommitInput, GitCommitResponse, GitPushResponse,
    GithubData, GroupResponse, HealthResponse, IdeDirectoryResponse, IdeFileInput, IdeFileResponse,
    MarkAllReadResponse, MessageInput, MessageResponse, OpenInCliResponse, OpenInInput,
    PatchRunInput, PickVariantRequest, PickVariantResponse, PlanResponse, ProjectsResponse,
    ProviderStatusResponse, QueuedMessagePatchInput, RemoveQueuedMessageResponse,
    RepoBranchRequest, RepoBranchResponse, RepoCommitPayload, RepoResponse, RunCommitsResponse,
    RunEvent, RunHistoryContext, RunHistoryPage, Runner, RunnerModelCatalogResponse,
    RunsIndexResponse, SetConfigInput, Skill, UiState, WorkflowsResponse, WorkspaceConfigResponse,
    WorkspaceUsageResponse, WorktreeEntry,
};
use futures_core::Stream;
use futures_util::StreamExt;
use reqwest::{Client, Method, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::EngineError;
use crate::scope::{Scope, api_path, encode_path_segment};
use crate::sse::{SseFrame, parse_sse_stream};
use crate::ws::WsTopicBus;

/// One sequenced run event from either the v1 or v2 SSE channel.
#[derive(Debug, Clone, PartialEq)]
pub struct RunStreamEvent {
    pub seq: f64,
    pub channel: Option<String>,
    pub event: RunEvent,
}

/// HTTP transport for the versioned service API.
#[derive(Clone)]
pub struct HttpEngine {
    pub(crate) client: Client,
    pub(crate) base_url: Url,
    pub(crate) bus: WsTopicBus,
}

impl HttpEngine {
    /// Construct an engine from the service origin, such as `http://127.0.0.1:4321`.
    pub fn new(base_url: impl AsRef<str>) -> Result<Self, EngineError> {
        let base_url = Url::parse(base_url.as_ref())
            .map_err(|error| EngineError::Transport(format!("invalid service URL: {error}")))?;
        let client = Client::builder()
            .build()
            .map_err(|error| EngineError::Transport(error.to_string()))?;
        Ok(Self::with_client(base_url, client))
    }

    pub(crate) fn with_client(base_url: Url, client: Client) -> Self {
        let ws_url = websocket_url(&base_url);
        Self {
            client,
            base_url,
            bus: WsTopicBus::new(ws_url),
        }
    }

    pub async fn health(&self) -> Result<HealthResponse, EngineError> {
        self.get_json(&Scope::Workspace, "/health").await
    }

    pub async fn list_runs(&self, scope: &Scope) -> Result<Vec<ApiRun>, EngineError> {
        self.get_json(scope, "/runs").await
    }

    pub async fn start_run(
        &self,
        scope: &Scope,
        input: &CreateRunInput,
    ) -> Result<CreateRunResponse, EngineError> {
        self.send_json(Method::POST, scope, "/runs", Some(input))
            .await
    }

    pub async fn get_run(&self, scope: &Scope, run_id: &str) -> Result<ApiRun, EngineError> {
        self.get_json(scope, &format!("/runs/{}", encode_path_segment(run_id)))
            .await
    }

    pub async fn archive_run(
        &self,
        scope: &Scope,
        run_id: &str,
        archived: bool,
    ) -> Result<ApiRun, EngineError> {
        self.send_json(
            Method::POST,
            scope,
            &format!("/runs/{}/archive", encode_path_segment(run_id)),
            Some(&serde_json::json!({ "archived": archived })),
        )
        .await
    }

    pub async fn delete_run(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<DeleteRunResponse, EngineError> {
        self.send_json(
            Method::DELETE,
            scope,
            &format!("/runs/{}", encode_path_segment(run_id)),
            Option::<&serde_json::Value>::None,
        )
        .await
    }

    pub async fn read_run(&self, scope: &Scope, run_id: &str) -> Result<ApiRun, EngineError> {
        self.send_json(
            Method::POST,
            scope,
            &format!("/runs/{}/read", encode_path_segment(run_id)),
            Option::<&serde_json::Value>::None,
        )
        .await
    }

    pub async fn unread_run(&self, scope: &Scope, run_id: &str) -> Result<ApiRun, EngineError> {
        self.send_json(
            Method::POST,
            scope,
            &format!("/runs/{}/unread", encode_path_segment(run_id)),
            Option::<&serde_json::Value>::None,
        )
        .await
    }

    pub async fn archive_finished(
        &self,
        scope: &Scope,
    ) -> Result<ArchiveFinishedResponse, EngineError> {
        self.send_json(
            Method::POST,
            scope,
            "/runs/archive-finished",
            Option::<&serde_json::Value>::None,
        )
        .await
    }

    pub async fn mark_all_read(&self, scope: &Scope) -> Result<MarkAllReadResponse, EngineError> {
        self.send_json(
            Method::POST,
            scope,
            "/runs/read-all",
            Option::<&serde_json::Value>::None,
        )
        .await
    }

    pub async fn runs_index(&self) -> Result<RunsIndexResponse, EngineError> {
        self.get_json(&Scope::Workspace, "/workspace/runs-index")
            .await
    }

    pub async fn workflows(&self, scope: &Scope) -> Result<WorkflowsResponse, EngineError> {
        self.get_json(scope, "/workflows").await
    }

    pub async fn skills(&self, scope: &Scope) -> Result<Vec<Skill>, EngineError> {
        self.get_json(scope, "/skills").await
    }

    pub async fn projects(&self) -> Result<ProjectsResponse, EngineError> {
        self.get_json(&Scope::Workspace, "/projects").await
    }

    pub async fn workspace_config(&self) -> Result<WorkspaceConfigResponse, EngineError> {
        self.get_json(&Scope::Workspace, "/workspace/config").await
    }

    pub async fn workspace_usage(&self) -> Result<WorkspaceUsageResponse, EngineError> {
        self.get_json(&Scope::Workspace, "/workspace/usage").await
    }

    pub async fn config(&self, scope: &Scope) -> Result<ConfigResponse, EngineError> {
        self.get_json(scope, "/config").await
    }

    pub async fn put_config(
        &self,
        scope: &Scope,
        input: &SetConfigInput,
    ) -> Result<ConfigResponse, EngineError> {
        self.send_json(Method::PUT, scope, "/config", Some(input))
            .await
    }

    pub async fn provider_status(&self) -> Result<ProviderStatusResponse, EngineError> {
        self.get_json(&Scope::Workspace, "/providers/status").await
    }

    pub async fn models(&self, runner: Runner) -> Result<RunnerModelCatalogResponse, EngineError> {
        self.get_json(
            &Scope::Workspace,
            &format!("/models?runner={}", runner_name(runner)),
        )
        .await
    }

    pub async fn github(&self, scope: &Scope) -> Result<GithubData, EngineError> {
        self.get_json(scope, "/github").await
    }

    pub async fn agent_profiles(&self) -> Result<AgentProfilesResponse, EngineError> {
        self.get_json(&Scope::Workspace, "/workspace/agent-profiles")
            .await
    }

    pub async fn ui_state(&self, scope: &Scope) -> Result<UiState, EngineError> {
        self.get_json(scope, "/ui-state").await
    }

    pub async fn put_ui_state(
        &self,
        scope: &Scope,
        state: &UiState,
    ) -> Result<UiState, EngineError> {
        self.send_json(Method::PUT, scope, "/ui-state", Some(state))
            .await
    }

    pub async fn plan(&self, scope: &Scope, task: &str) -> Result<PlanResponse, EngineError> {
        self.send_json(
            Method::POST,
            scope,
            "/plan",
            Some(&serde_json::json!({ "task": task })),
        )
        .await
    }

    /// One page of a run's persisted event history (§8.4), newest-first cursor semantics.
    pub async fn run_history(
        &self,
        scope: &Scope,
        run_id: &str,
        cursor: Option<&str>,
    ) -> Result<RunHistoryPage, EngineError> {
        let query = query(&[("cursor", cursor)]);
        self.get_json(
            scope,
            &format!("/runs/{}/history{query}", encode_path_segment(run_id)),
        )
        .await
    }

    pub async fn run_history_context(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<RunHistoryContext, EngineError> {
        self.get_json(
            scope,
            &format!("/runs/{}/history-context", encode_path_segment(run_id)),
        )
        .await
    }

    /// Editable title/prompt (§8.4 `run-header.tsx` / queued-run task edit).
    pub async fn patch_run(
        &self,
        scope: &Scope,
        run_id: &str,
        input: &PatchRunInput,
    ) -> Result<ApiRun, EngineError> {
        self.send_json(
            Method::PATCH,
            scope,
            &format!("/runs/{}", encode_path_segment(run_id)),
            Some(input),
        )
        .await
    }

    pub async fn cancel_run(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<CancelResponse, EngineError> {
        self.send_json(
            Method::POST,
            scope,
            &format!("/runs/{}/cancel", encode_path_segment(run_id)),
            Option::<&serde_json::Value>::None,
        )
        .await
    }

    /// Live-session participation: deliver a message into the run's open session, fold
    /// it into a queued prompt, or buffer it while the run is still starting (§8.4's
    /// three-rung delivery ladder — `MessageResponse` names which rung answered).
    pub async fn send_message(
        &self,
        scope: &Scope,
        run_id: &str,
        input: &MessageInput,
    ) -> Result<MessageResponse, EngineError> {
        self.send_json(
            Method::POST,
            scope,
            &format!("/runs/{}/messages", encode_path_segment(run_id)),
            Some(input),
        )
        .await
    }

    pub async fn edit_queued_message(
        &self,
        scope: &Scope,
        run_id: &str,
        message_id: &str,
        input: &QueuedMessagePatchInput,
    ) -> Result<EditQueuedMessageResponse, EngineError> {
        self.send_json(
            Method::PATCH,
            scope,
            &format!(
                "/runs/{}/queued-messages/{}",
                encode_path_segment(run_id),
                encode_path_segment(message_id)
            ),
            Some(input),
        )
        .await
    }

    pub async fn remove_queued_message(
        &self,
        scope: &Scope,
        run_id: &str,
        message_id: &str,
    ) -> Result<RemoveQueuedMessageResponse, EngineError> {
        self.send_json(
            Method::DELETE,
            scope,
            &format!(
                "/runs/{}/queued-messages/{}",
                encode_path_segment(run_id),
                encode_path_segment(message_id)
            ),
            Option::<&serde_json::Value>::None,
        )
        .await
    }

    /// Gracefully close a waiting session — the run completes as done.
    pub async fn finish_run(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<FinishResponse, EngineError> {
        self.send_json(
            Method::POST,
            scope,
            &format!("/runs/{}/finish", encode_path_segment(run_id)),
            Option::<&serde_json::Value>::None,
        )
        .await
    }

    /// Reopen a finished run's session in-process — Continue, the review panel's
    /// "Send back" (prefixed `Review feedback:` by the caller), and an ask-answer resume
    /// all ride this one route.
    pub async fn continue_run(
        &self,
        scope: &Scope,
        run_id: &str,
        input: &ContinueInput,
    ) -> Result<ContinueResponse, EngineError> {
        self.send_json(
            Method::POST,
            scope,
            &format!("/runs/{}/continue", encode_path_segment(run_id)),
            Some(input),
        )
        .await
    }

    /// Hand the session off to a real terminal in the run's worktree.
    pub async fn open_in_cli(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<OpenInCliResponse, EngineError> {
        self.send_json(
            Method::POST,
            scope,
            &format!("/runs/{}/open-in-cli", encode_path_segment(run_id)),
            Option::<&serde_json::Value>::None,
        )
        .await
    }

    /// Open a run's worktree file/session in the chosen local app or CLI. The response
    /// shape varies by `target` (an opened path, a resumed command, …), so callers read
    /// it as JSON rather than through one fixed contract type.
    pub async fn open_in(
        &self,
        scope: &Scope,
        run_id: &str,
        input: &OpenInInput,
    ) -> Result<Value, EngineError> {
        self.send_json(
            Method::POST,
            scope,
            &format!("/runs/{}/open-in", encode_path_segment(run_id)),
            Some(input),
        )
        .await
    }

    pub async fn git_commit(
        &self,
        scope: &Scope,
        run_id: &str,
        input: &GitCommitInput,
    ) -> Result<GitCommitResponse, EngineError> {
        self.send_json(
            Method::POST,
            scope,
            &format!("/runs/{}/git/commit", encode_path_segment(run_id)),
            Some(input),
        )
        .await
    }

    pub async fn git_push(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<GitPushResponse, EngineError> {
        self.send_json(
            Method::POST,
            scope,
            &format!("/runs/{}/git/push", encode_path_segment(run_id)),
            Option::<&serde_json::Value>::None,
        )
        .await
    }

    pub async fn run_commits(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<RunCommitsResponse, EngineError> {
        self.get_json(
            scope,
            &format!("/runs/{}/commits", encode_path_segment(run_id)),
        )
        .await
    }

    /// The run's worktree diff vs its base, as plain text (spec 006). Used only where the
    /// text form is the contract — the structured `/changes` route below is what screens
    /// actually render.
    pub async fn run_diff_text(&self, scope: &Scope, run_id: &str) -> Result<String, EngineError> {
        self.get_text(
            scope,
            &format!("/runs/{}/diff", encode_path_segment(run_id)),
        )
        .await
    }

    /// The run's structured diff — the Changes tab's one read.
    pub async fn run_changes(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<ChangesPayload, EngineError> {
        self.get_json(
            scope,
            &format!("/runs/{}/changes", encode_path_segment(run_id)),
        )
        .await
    }

    /// One of the run's own commits, structured like the Changes tab.
    pub async fn run_commit(
        &self,
        scope: &Scope,
        run_id: &str,
        sha: &str,
    ) -> Result<RepoCommitPayload, EngineError> {
        self.get_json(
            scope,
            &format!(
                "/runs/{}/commit/{}",
                encode_path_segment(run_id),
                encode_path_segment(sha)
            ),
        )
        .await
    }

    /// The Files tab's directory listing or one file's (size-capped) content.
    pub async fn run_files(
        &self,
        scope: &Scope,
        run_id: &str,
        path: Option<&str>,
    ) -> Result<WorktreeEntry, EngineError> {
        let route = format!(
            "/runs/{}/files{}",
            encode_path_segment(run_id),
            query(&[("path", path)])
        );
        self.get_json(scope, &route).await
    }

    /// The raw bytes behind one worktree file — the Files tab's image preview path (`?raw=1`).
    pub async fn run_file_raw(
        &self,
        scope: &Scope,
        run_id: &str,
        path: &str,
    ) -> Result<Vec<u8>, EngineError> {
        let route = format!(
            "/runs/{}/files{}",
            encode_path_segment(run_id),
            query(&[("path", Some(path)), ("raw", Some("1"))])
        );
        let response = self.send(Method::GET, scope, &route, None).await?;
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| EngineError::Transport(error.to_string()))
    }

    /// `GET /repo` — the repo view's one read.
    pub async fn repo(&self, scope: &Scope) -> Result<RepoResponse, EngineError> {
        self.get_json(scope, "/repo").await
    }

    /// The MAIN working tree's structured diff vs `HEAD` — the repo Changes tab.
    pub async fn repo_changes(&self, scope: &Scope) -> Result<ChangesPayload, EngineError> {
        self.get_json(scope, "/repo/changes").await
    }

    /// One repo commit, structured like the Changes tab (`?structured=1`).
    pub async fn repo_commit(
        &self,
        scope: &Scope,
        sha: &str,
    ) -> Result<RepoCommitPayload, EngineError> {
        self.get_json(
            scope,
            &format!("/repo/commit/{}?structured=1", encode_path_segment(sha)),
        )
        .await
    }

    /// Switch to an existing branch, or create one and switch — the Branches tab's one write.
    pub async fn repo_branch(
        &self,
        scope: &Scope,
        input: &RepoBranchRequest,
    ) -> Result<RepoBranchResponse, EngineError> {
        self.send_json(Method::POST, scope, "/repo/branch", Some(input))
            .await
    }

    /// `GET /groups/:groupId` — every run sharing a groupId, side by side (compare view).
    pub async fn group(&self, scope: &Scope, group_id: &str) -> Result<GroupResponse, EngineError> {
        self.get_json(scope, &format!("/groups/{}", encode_path_segment(group_id)))
            .await
    }

    /// `POST /groups/:groupId/pick` — the compare view's "Pick this one".
    pub async fn pick_variant(
        &self,
        scope: &Scope,
        group_id: &str,
        input: &PickVariantRequest,
    ) -> Result<PickVariantResponse, EngineError> {
        self.send_json(
            Method::POST,
            scope,
            &format!("/groups/{}/pick", encode_path_segment(group_id)),
            Some(input),
        )
        .await
    }

    /// One IDE directory listing. `path = None` means the project root; the server rejects
    /// anything outside it, and never follows symlinks (spec §8.8, A10).
    pub async fn ide_tree(
        &self,
        scope: &Scope,
        path: Option<&str>,
    ) -> Result<IdeDirectoryResponse, EngineError> {
        let route = format!("/ide/tree{}", query(&[("path", path)]));
        self.get_json(scope, &route).await
    }

    /// One IDE file's UTF-8 content. The server answers 409 for files over its 1 MB cap and
    /// for binary content — those map to `EngineError::Conflict` with the server's reason.
    pub async fn ide_file(
        &self,
        scope: &Scope,
        path: &str,
    ) -> Result<IdeFileResponse, EngineError> {
        let route = format!("/ide/file{}", query(&[("path", Some(path))]));
        self.get_json(scope, &route).await
    }

    /// Save one IDE file — the editor's `Ctrl+S` round-trip through `PUT /ide/file`.
    pub async fn ide_save(
        &self,
        scope: &Scope,
        path: &str,
        content: &str,
    ) -> Result<IdeFileResponse, EngineError> {
        self.send_json(
            Method::PUT,
            scope,
            "/ide/file",
            Some(&IdeFileInput {
                path: path.to_owned(),
                content: content.to_owned(),
            }),
        )
        .await
    }

    /// Draft PR from the review gate: final autosave, push, `gh pr create --draft`.
    pub async fn create_pr(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<CreatePrResponse, EngineError> {
        self.send_json(
            Method::POST,
            scope,
            &format!("/runs/{}/pr", encode_path_segment(run_id)),
            Option::<&serde_json::Value>::None,
        )
        .await
    }

    /// The auto-resume hint's "Don't resume" — idempotent (a run with nothing pending
    /// still answers `{cancelled: true}`).
    pub async fn cancel_auto_resume(
        &self,
        scope: &Scope,
        run_id: &str,
    ) -> Result<CancelAutoResumeResponse, EngineError> {
        self.send_json(
            Method::DELETE,
            scope,
            &format!("/runs/{}/auto-resume", encode_path_segment(run_id)),
            Option::<&serde_json::Value>::None,
        )
        .await
    }

    pub(crate) fn subscribe_topic(
        &self,
        topic: impl Into<String>,
    ) -> futures_core::stream::BoxStream<'static, crate::ws::EngineEvent> {
        self.bus.subscribe(topic)
    }

    /// Open a raw SSE stream. Callers own event-specific decoding and high-water marks.
    pub async fn sse_frames(
        &self,
        scope: &Scope,
        route: &str,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<SseFrame, EngineError>> + Send>>, EngineError>
    {
        let response = self.send(Method::GET, scope, route, None).await?;
        Ok(Box::pin(parse_sse_stream(response.bytes_stream())))
    }

    /// Subscribe to one run's replayable SSE stream with cursor/sequence resume and dedup.
    pub async fn run_events(
        &self,
        scope: &Scope,
        run_id: &str,
        cursor: Option<&str>,
        after_seq: Option<f64>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<RunStreamEvent, EngineError>> + Send>>, EngineError>
    {
        let query = query(&[
            ("cursor", cursor),
            ("afterSeq", after_seq.map(|seq| seq.to_string()).as_deref()),
        ]);
        let route = format!("/runs/{}/events{query}", encode_path_segment(run_id));
        let frames = self.sse_frames(scope, &route).await?;
        let mut high_water = after_seq.unwrap_or(-1.0);
        let stream = async_stream::try_stream! {
            futures_util::pin_mut!(frames);
            while let Some(frame) = frames.next().await {
                let frame = frame?;
                let payload: Value = match serde_json::from_str(&frame.data) {
                    Ok(payload) => payload,
                    Err(_) => continue,
                };
                let Some(seq) = payload.get("seq").and_then(Value::as_f64) else {
                    continue;
                };
                if seq <= high_water {
                    continue;
                }
                let Ok(event) = serde_json::from_value::<RunEvent>(payload) else {
                    continue;
                };
                high_water = seq;
                yield RunStreamEvent { seq, channel: frame.event, event };
            }
        };
        Ok(Box::pin(stream))
    }

    pub(crate) async fn send(
        &self,
        method: Method,
        scope: &Scope,
        route: &str,
        body: Option<Value>,
    ) -> Result<reqwest::Response, EngineError> {
        let url = api_path(&self.base_url, scope, route)?;
        let mut request = self.client.request(method, url);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))?;
        if response.status().is_success() {
            return Ok(response);
        }
        Err(map_response_error(response).await)
    }

    async fn get_json<T>(&self, scope: &Scope, route: &str) -> Result<T, EngineError>
    where
        T: DeserializeOwned,
    {
        self.send_json(Method::GET, scope, route, Option::<&Value>::None)
            .await
    }

    /// `GET` a `text/plain` route (the legacy diff-blob endpoints) rather than JSON.
    async fn get_text(&self, scope: &Scope, route: &str) -> Result<String, EngineError> {
        let response = self.send(Method::GET, scope, route, None).await?;
        response
            .text()
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))
    }

    async fn send_json<T, B>(
        &self,
        method: Method,
        scope: &Scope,
        route: &str,
        body: Option<&B>,
    ) -> Result<T, EngineError>
    where
        T: DeserializeOwned,
        B: serde::Serialize,
    {
        let body = body
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| EngineError::Transport(error.to_string()))?;
        let response = self.send(method, scope, route, body).await?;
        response
            .json::<T>()
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))
    }
}

fn query(parts: &[(&str, Option<&str>)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in parts {
        if let Some(value) = value {
            serializer.append_pair(key, value);
        }
    }
    let query = serializer.finish();
    if query.is_empty() {
        String::new()
    } else {
        format!("?{query}")
    }
}

fn runner_name(runner: Runner) -> &'static str {
    match runner {
        Runner::Claude => "claude",
        Runner::Codex => "codex",
        Runner::OpenCode => "opencode",
        Runner::Pi => "pi",
    }
}

fn websocket_url(base: &Url) -> Url {
    let mut url = base.clone();
    let scheme = match base.scheme() {
        "https" => "wss",
        _ => "ws",
    };
    let _ = url.set_scheme(scheme);
    let base_path = base.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/api/v1/ws"));
    url.set_query(None);
    url.set_fragment(None);
    url
}

async fn map_response_error(response: reqwest::Response) -> EngineError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let reason = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| {
            if body.is_empty() {
                "request failed".to_owned()
            } else {
                body
            }
        });
    match status {
        StatusCode::NOT_FOUND => EngineError::NotFound,
        StatusCode::CONFLICT => EngineError::Conflict { reason },
        StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT => {
            EngineError::Unavailable { reason }
        }
        _ => EngineError::Transport(reason),
    }
}
