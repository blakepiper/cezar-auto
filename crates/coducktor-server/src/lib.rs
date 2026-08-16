//! The temporary HTTP boundary used while the Rust engine and terminal UI are separate
//! processes.
//!
//! B9 ports the Node service behind the same `/api/v1` contract.  This crate deliberately
//! owns transport concerns only: route handlers validate a request, delegate to the core
//! services, and serialize a contract value.  The first B9 slice exposes health and the
//! request-origin perimeter so subsequent route-family commits have one stable shell to
//! extend.  The entire crate is deleted at C2 when the TUI switches to an in-process engine.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::header::HeaderName;
use axum::http::header::{ACCESS_CONTROL_ALLOW_ORIGIN, HOST, ORIGIN};
use axum::http::{HeaderValue, Method, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use coducktor_contract::{
    ApiRun, BackendCheck, BackendCheckName, Capabilities, ContinueInput, CreateRunInput,
    CreateRunResponse, DeleteRunResponse, ForgeInfo, ForgeKind, HealthProject, HealthResponse,
    MarkAllReadResponse, MessageInput, PatchRunInput, RepoInfo, RunRecord, RunnerSelection,
};
use coducktor_core::workflows::load::load_workflows;
use coducktor_core::workflows::run::{
    ContinueOptions, RunManager, StartRunInput as CoreStartRunInput,
};
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Map, Value};

const HEALTH_PATH: &str = "/api/v1/health";
const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");

/// Configuration shared by the route families.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub repo_root: PathBuf,
    pub version: String,
}

impl ServerConfig {
    pub fn new(repo_root: impl Into<PathBuf>, version: impl Into<String>) -> Self {
        Self {
            repo_root: repo_root.into(),
            version: version.into(),
        }
    }
}

/// Immutable state for the B9 transport shell. Mutable route-family state is added behind
/// this `Arc` as each family lands; handlers never keep state in global statics.
#[derive(Clone)]
pub struct ServerState {
    config: Arc<ServerConfig>,
    manager: Arc<Mutex<RunManager>>,
}

impl ServerState {
    pub fn new(config: ServerConfig) -> Self {
        let manager = RunManager::for_repo(&config.repo_root);
        Self::with_manager(config, manager)
    }

    pub fn with_manager(config: ServerConfig, manager: RunManager) -> Self {
        Self {
            config: Arc::new(config),
            manager: Arc::new(Mutex::new(manager)),
        }
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn manager(&self) -> &Arc<Mutex<RunManager>> {
        &self.manager
    }
}

/// Build the versioned HTTP application.
pub fn router(config: ServerConfig) -> Router {
    let state = ServerState::new(config);
    router_with_state(state)
}

/// Construct the application around a caller-owned manager. Route-family tests use this to
/// seed durable records without reaching through HTTP; the production path uses [`router`].
pub fn router_with_state(state: ServerState) -> Router {
    Router::new()
        .route(HEALTH_PATH, get(health))
        .route("/api/v1/runs", get(list_runs).post(create_run))
        .route(
            "/api/v1/runs/archive-finished",
            axum::routing::post(archive_finished),
        )
        .route("/api/v1/runs/read-all", axum::routing::post(mark_all_read))
        .route(
            "/api/v1/runs/{id}",
            get(get_run).patch(patch_run).delete(delete_run),
        )
        .route(
            "/api/v1/runs/{id}/archive",
            axum::routing::post(archive_run),
        )
        .route("/api/v1/runs/{id}/read", axum::routing::post(read_run))
        .route("/api/v1/runs/{id}/unread", axum::routing::post(unread_run))
        .route("/api/v1/runs/{id}/cancel", axum::routing::post(cancel_run))
        .route(
            "/api/v1/runs/{id}/continue",
            axum::routing::post(continue_run),
        )
        .route("/api/v1/runs/{id}/finish", axum::routing::post(finish_run))
        .route(
            "/api/v1/runs/{id}/messages",
            axum::routing::post(send_message),
        )
        .route(
            "/api/v1/runs/{id}/auto-resume",
            axum::routing::delete(cancel_auto_resume),
        )
        // Project-scoped routes have the same default and boot-project aliases as the Node
        // service.  The scope guard below admits only `default` and this server's boot project.
        .route("/api/v1/p/{project}/runs", get(list_runs).post(create_run))
        .route(
            "/api/v1/p/{project}/runs/archive-finished",
            axum::routing::post(archive_finished),
        )
        .route(
            "/api/v1/p/{project}/runs/read-all",
            axum::routing::post(mark_all_read),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}",
            get(get_run).patch(patch_run).delete(delete_run),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}/archive",
            axum::routing::post(archive_run),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}/read",
            axum::routing::post(read_run),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}/unread",
            axum::routing::post(unread_run),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}/cancel",
            axum::routing::post(cancel_run),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}/continue",
            axum::routing::post(continue_run),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}/finish",
            axum::routing::post(finish_run),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}/messages",
            axum::routing::post(send_message),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}/auto-resume",
            axum::routing::delete(cancel_auto_resume),
        )
        .fallback(not_found)
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, request_origin_guard))
}

/// Start serving an already-bound listener. The caller owns binding so tests can select a free
/// port and the future CLI can keep port policy outside the temporary server crate.
pub async fn serve(
    listener: tokio::net::TcpListener,
    config: ServerConfig,
) -> Result<(), std::io::Error> {
    axum::serve(listener, router(config)).await
}

async fn health(State(state): State<ServerState>) -> Response {
    let payload = health_payload(state.config());
    let mut response = Json(payload).into_response();
    // Health is the one intentionally CORS-readable discovery route. The host guard still runs
    // first, so this does not make DNS rebinding readable.
    response
        .headers_mut()
        .insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    response
}

async fn not_found() -> Response {
    error_response(StatusCode::NOT_FOUND, "not found")
}

async fn list_runs(State(state): State<ServerState>) -> Response {
    let Ok(manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    let runs = manager
        .list_runs()
        .into_iter()
        .map(api_run)
        .collect::<Vec<_>>();
    Json(runs).into_response()
}

async fn get_run(State(state): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    let Ok(manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    match manager.get_run(&id).cloned() {
        Some(run) => Json(api_run(run)).into_response(),
        None => error_response(StatusCode::NOT_FOUND, "not found"),
    }
}

async fn create_run(
    State(state): State<ServerState>,
    Json(input): Json<CreateRunInput>,
) -> Response {
    let workflow = match workflow_for_input(state.config(), &input) {
        Ok(workflow) => workflow,
        Err((status, message)) => return error_response(status, &message),
    };
    let core_input = CoreStartRunInput {
        task: input.task,
        model: input.model,
        reasoning_effort: input.reasoning_effort,
        runner: input.runner,
        agent_profile: input.agent_profile,
        system_prompt: input.system_prompt,
        generate_followups: input.generate_followups,
        autonomous: input.autonomous,
        worktree: input.worktree,
    };
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    let variants = input.variants.unwrap_or(1.0);
    if !variants.is_finite() || variants.fract() != 0.0 || !(1.0..=3.0).contains(&variants) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "variants must be an integer from 1 to 3",
        );
    }
    if variants > 1.0 {
        return match manager.start_variants(&workflow, core_input, variants as usize) {
            Ok(runs) => {
                (StatusCode::CREATED, Json(CreateRunResponse::Group { runs })).into_response()
            }
            Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
        };
    }
    match manager.start_run(&workflow, core_input) {
        Ok(run) => (
            StatusCode::CREATED,
            Json(CreateRunResponse::Single(Box::new(run))),
        )
            .into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn archive_finished(State(state): State<ServerState>) -> Response {
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    match manager.archive_finished() {
        Ok(archived) => Json(coducktor_contract::ArchiveFinishedResponse {
            archived: archived as f64,
        })
        .into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn mark_all_read(State(state): State<ServerState>) -> Response {
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    match manager.mark_all_read() {
        Ok(read) => Json(MarkAllReadResponse { read: read as f64 }).into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

#[derive(Debug, Default, Deserialize)]
struct ArchiveInput {
    archived: Option<bool>,
}

async fn archive_run(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    body: Option<Json<ArchiveInput>>,
) -> Response {
    let archived = body.and_then(|Json(value)| value.archived).unwrap_or(true);
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    match manager.archive(&id, archived) {
        Ok(Some(run)) => Json(run).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "not found"),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn read_run(State(state): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    mutate_read(state, id, true)
}

async fn unread_run(State(state): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    mutate_read(state, id, false)
}

fn mutate_read(state: ServerState, id: String, read: bool) -> Response {
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    let result = if read {
        manager.mark_read(&id)
    } else {
        manager.mark_unread(&id)
    };
    match result {
        Ok(Some(run)) => Json(run).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "not found"),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn patch_run(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(input): Json<PatchRunInput>,
) -> Response {
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    let Some(current) = manager.get_run(&id).cloned() else {
        return error_response(StatusCode::NOT_FOUND, "not found");
    };
    if input.task.is_some() && current.status != coducktor_contract::RunStatus::Queued {
        return error_response(StatusCode::CONFLICT, "run already started");
    }
    let mut value = Map::new();
    if let Some(title) = input.title {
        value.insert("title".to_owned(), Value::String(title.clone()));
        value.insert("titleSummary".to_owned(), Value::String(title));
        value.insert("titleOrigin".to_owned(), Value::String("user".to_owned()));
    }
    if let Some(task) = input.task {
        value.insert("task".to_owned(), Value::String(task));
    }
    match manager.update_run_value(&id, Value::Object(value)) {
        Ok(Some(run)) => Json(run).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "not found"),
        Err(error) => error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

async fn cancel_run(State(state): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    if manager.get_run(&id).is_none() {
        return error_response(StatusCode::NOT_FOUND, "not found");
    }
    match manager.cancel(&id) {
        Ok(cancelled) => Json(coducktor_contract::CancelResponse { cancelled }).into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn finish_run(State(state): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    if manager.get_run(&id).is_none() {
        return error_response(StatusCode::NOT_FOUND, "not found");
    }
    match manager.finish(&id) {
        Ok(true) => Json(coducktor_contract::FinishResponse { finished: true }).into_response(),
        Ok(false) => error_response(StatusCode::CONFLICT, "no open session"),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn continue_run(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    body: Option<Json<ContinueInput>>,
) -> Response {
    let input = body.map(|Json(value)| value).unwrap_or_default();
    let options = ContinueOptions {
        text: input.text,
        runner: input.runner,
        model: input.model,
    };
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    if manager.get_run(&id).is_none() {
        return error_response(StatusCode::NOT_FOUND, "not found");
    }
    match manager.continue_run(&id, options) {
        Ok(result) if result.ok => {
            Json(coducktor_contract::ContinueResponse { continued: true }).into_response()
        }
        Ok(result) => error_response(
            StatusCode::CONFLICT,
            result.error.as_deref().unwrap_or("cannot continue run"),
        ),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn send_message(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(input): Json<MessageInput>,
) -> Response {
    let Some(text) = input.text.filter(|value| !value.trim().is_empty()) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "message needs text or at least one image",
        );
    };
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    if manager.get_run(&id).is_none() {
        return error_response(StatusCode::NOT_FOUND, "not found");
    }
    match manager.send_message(&id, text) {
        Ok(true) => {
            Json(coducktor_contract::MessageResponse::Delivered { delivered: true }).into_response()
        }
        Ok(false) => error_response(StatusCode::CONFLICT, "session closed"),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn cancel_auto_resume(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    if manager.get_run(&id).is_none() {
        return error_response(StatusCode::NOT_FOUND, "not found");
    }
    let mut patch = Map::new();
    patch.insert("autoResumeAt".to_owned(), Value::Null);
    patch.insert("autoResumeAttempts".to_owned(), Value::Null);
    match manager.update_run_value(&id, Value::Object(patch)) {
        Ok(Some(_)) => {
            Json(coducktor_contract::CancelAutoResumeResponse { cancelled: true }).into_response()
        }
        Ok(None) => error_response(StatusCode::NOT_FOUND, "not found"),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn delete_run(State(state): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    if manager.get_run(&id).is_none() {
        return error_response(StatusCode::NOT_FOUND, "not found");
    }
    if manager.is_active(&id) {
        return error_response(StatusCode::CONFLICT, "cannot delete an active run");
    }
    match manager.remove_run(&id) {
        Ok(deleted) => Json(DeleteRunResponse { deleted }).into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

fn workflow_for_input(
    config: &ServerConfig,
    input: &CreateRunInput,
) -> Result<coducktor_contract::WorkflowDef, (StatusCode, String)> {
    if let Some(steps) = &input.steps {
        if steps.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                "steps must not be empty".to_owned(),
            ));
        }
        return Ok(coducktor_contract::WorkflowDef {
            name: "(planned)".to_owned(),
            description: None,
            steps: steps.clone(),
            source: coducktor_contract::WorkflowSource::BuiltIn,
            path: None,
        });
    }
    let Some(name) = input.workflow.as_deref() else {
        return Err((
            StatusCode::BAD_REQUEST,
            "workflow or steps is required".to_owned(),
        ));
    };
    load_workflows(&config.repo_root)
        .0
        .into_iter()
        .find(|workflow| workflow.name == name)
        .ok_or((StatusCode::NOT_FOUND, format!("unknown workflow: {name}")))
}

fn api_run(record: RunRecord) -> ApiRun {
    ApiRun {
        record,
        usage: None,
    }
}

fn health_payload(config: &ServerConfig) -> HealthResponse {
    let repo_root = config.repo_root.to_string_lossy().into_owned();
    let branch = git_output(&config.repo_root, &["branch", "--show-current"]);
    let remote = git_output(&config.repo_root, &["config", "--get", "remote.origin.url"]);
    let repo = if branch.is_some() || remote.is_some() {
        Some(RepoInfo {
            root: repo_root.clone(),
            branch: branch.unwrap_or_default(),
            remote,
        })
    } else {
        None
    };

    HealthResponse {
        version: config.version.clone(),
        latest_version: None,
        repo_root,
        repo,
        checks: [
            (BackendCheckName::Claude, "claude"),
            (BackendCheckName::Codex, "codex"),
            (BackendCheckName::OpenCode, "opencode"),
            (BackendCheckName::Pi, "pi"),
            (BackendCheckName::Gh, "gh"),
            (BackendCheckName::Git, "git"),
        ]
        .into_iter()
        .map(|(name, binary)| backend_check(name, binary))
        .collect(),
        default_runner: RunnerSelection::Auto,
        forge: Some(ForgeInfo {
            kind: ForgeKind::GitHub,
            available: None,
            reason: Some("Rust server forge routes are not installed yet".to_owned()),
        }),
        capabilities: Capabilities {
            followups: std::env::var("DUCK_FOLLOWUPS")
                .or_else(|_| std::env::var("CEZ_FOLLOWUPS"))
                .is_ok_and(|value| value == "1"),
        },
        projects: vec![HealthProject {
            id: "default".to_owned(),
            name: config
                .repo_root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("project")
                .to_owned(),
        }],
        boot_project: "default".to_owned(),
    }
}

fn backend_check(name: BackendCheckName, binary: &str) -> BackendCheck {
    match Command::new(binary).arg("--version").output() {
        Ok(output) if output.status.success() => BackendCheck {
            name,
            available: true,
            version: first_line(&output.stdout).or_else(|| first_line(&output.stderr)),
            hint: None,
        },
        Ok(_) => BackendCheck {
            name,
            available: false,
            version: None,
            hint: Some(format!("{binary} --version failed")),
        },
        Err(error) => BackendCheck {
            name,
            available: false,
            version: None,
            hint: Some(if error.kind() == std::io::ErrorKind::NotFound {
                format!("{binary} CLI not found")
            } else {
                error.to_string()
            }),
        },
    }
}

fn git_output(repo_root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    first_line(&output.stdout)
}

fn first_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}

async fn request_origin_guard(
    State(state): State<ServerState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !request.uri().path().starts_with("/api/") {
        return next.run(request).await;
    }

    let host = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok());
    let host_name = host.and_then(hostname);
    if !host_name.is_some_and(is_loopback_host) {
        return error_response(
            StatusCode::FORBIDDEN,
            "forbidden: unexpected Host header — this request did not originate from this machine",
        );
    }

    if request.uri().path() == HEALTH_PATH {
        return next.run(request).await;
    }

    if let Some(project) = scoped_project(request.uri().path())
        && !project_alias_allowed(state.config(), project)
    {
        return error_response(StatusCode::NOT_FOUND, "not found");
    }

    if is_mutating(request.method()) {
        if let Some(origin) = request
            .headers()
            .get(ORIGIN)
            .and_then(|value| value.to_str().ok())
        {
            let same_origin = authority(origin)
                .zip(host)
                .is_some_and(|(origin, host)| origin == host);
            let is_dev_proxy = request
                .headers()
                .get(SEC_FETCH_SITE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value == "same-origin")
                && origin
                    .strip_prefix("http://")
                    .or_else(|| origin.strip_prefix("https://"))
                    .and_then(hostname)
                    .is_some_and(is_loopback_host);
            if !same_origin && !is_dev_proxy {
                return error_response(
                    StatusCode::FORBIDDEN,
                    "forbidden: cross-origin request rejected (same-origin only)",
                );
            }
        }
        if request
            .headers()
            .get(SEC_FETCH_SITE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == "cross-site")
        {
            return error_response(
                StatusCode::FORBIDDEN,
                "forbidden: cross-site request rejected (same-origin only)",
            );
        }
    }

    next.run(request).await
}

fn scoped_project(path: &str) -> Option<&str> {
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    if segments.next() != Some("api")
        || segments.next() != Some("v1")
        || segments.next() != Some("p")
    {
        return None;
    }
    segments.next()
}

fn project_alias_allowed(config: &ServerConfig, project: &str) -> bool {
    project == "default"
        || config
            .repo_root
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == project)
}

fn is_mutating(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn hostname(value: &str) -> Option<&str> {
    if let Some(rest) = value.strip_prefix('[') {
        return rest.split(']').next();
    }
    value.rsplit_once(':').map_or(Some(value), |(host, port)| {
        port.parse::<u16>().ok().map(|_| host)
    })
}

fn authority(value: &str) -> Option<&str> {
    let without_scheme = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))?;
    Some(
        without_scheme
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default(),
    )
}

fn is_loopback_host(value: &str) -> bool {
    if value.eq_ignore_ascii_case("localhost") {
        return true;
    }
    value
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

fn error_response(status: StatusCode, error: &str) -> Response {
    #[derive(Serialize)]
    struct ErrorBody<'a> {
        error: &'a str,
    }
    (status, Json(ErrorBody { error })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request;
    use coducktor_core::workflows::run::{CreateRunInput as CoreCreateRunInput, RunManager};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tower::ServiceExt;

    static TEST_REPO_ID: AtomicU64 = AtomicU64::new(0);

    fn test_router() -> Router {
        router(ServerConfig::new("/tmp/not-a-repo", "test"))
    }

    fn test_repo() -> std::path::PathBuf {
        let id = TEST_REPO_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("coducktor-server-test-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).expect("test repo directory");
        path
    }

    fn seeded_router() -> (Router, std::path::PathBuf, String) {
        let repo = test_repo();
        let data_dir = repo.join(".ai").join("coducktor");
        let mut manager = RunManager::open(&data_dir);
        let run = manager
            .create_run(CoreCreateRunInput {
                title: "original".to_owned(),
                workflow: "manual".to_owned(),
                task: "queued task".to_owned(),
                ..CoreCreateRunInput::default()
            })
            .expect("seed run");
        let state = ServerState::with_manager(ServerConfig::new(&repo, "test"), manager);
        (router_with_state(state), repo, run.id)
    }

    async fn send(router: &Router, method: Method, uri: &str, body: Option<Value>) -> Response {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(HOST, "127.0.0.1:4321");
        let request_body = match body {
            Some(body) => {
                builder = builder.header("content-type", "application/json");
                Body::from(serde_json::to_vec(&body).expect("request JSON"))
            }
            None => Body::empty(),
        };
        router
            .clone()
            .oneshot(builder.body(request_body).expect("test request"))
            .await
            .expect("router response")
    }

    async fn json_body(response: Response) -> Value {
        let bytes = response_body(response).await;
        serde_json::from_slice(&bytes).expect("response JSON")
    }

    async fn response_body(response: Response) -> Vec<u8> {
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec()
    }

    #[tokio::test]
    async fn health_is_versioned_and_cors_readable() {
        let request = Request::builder()
            .uri(HEALTH_PATH)
            .header(HOST, "127.0.0.1:4321")
            .body(Body::empty())
            .expect("test request is valid");
        let response = test_router()
            .oneshot(request)
            .await
            .expect("router response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("*"))
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let parsed: HealthResponse = serde_json::from_slice(&body).expect("health JSON");
        assert_eq!(parsed.version, "test");
        assert_eq!(parsed.boot_project, "default");
    }

    #[tokio::test]
    async fn non_loopback_hosts_are_rejected_before_route_dispatch() {
        let request = Request::builder()
            .uri(HEALTH_PATH)
            .header(HOST, "127.0.0.1.evil.example:4321")
            .body(Body::empty())
            .expect("test request is valid");
        let response = test_router()
            .oneshot(request)
            .await
            .expect("router response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn cross_origin_writes_are_rejected() {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/runs")
            .header(HOST, "127.0.0.1:4321")
            .header(ORIGIN, "http://127.0.0.1:9999")
            .body(Body::empty())
            .expect("test request is valid");
        let response = test_router()
            .oneshot(request)
            .await
            .expect("router response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn runs_routes_cover_create_list_patch_receipts_archive_and_delete() {
        let (router, repo, run_id) = seeded_router();

        let response = send(&router, Method::GET, "/api/v1/runs", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let runs = json_body(response).await;
        assert_eq!(runs.as_array().map(Vec::len), Some(1));
        assert_eq!(runs[0]["id"], run_id);

        let response = send(
            &router,
            Method::PATCH,
            &format!("/api/v1/runs/{run_id}"),
            Some(serde_json::json!({ "title": "renamed", "task": "edited task" })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let patched = json_body(response).await;
        assert_eq!(patched["title"], "renamed");
        assert_eq!(patched["titleSummary"], "renamed");
        assert_eq!(patched["titleOrigin"], "user");
        assert_eq!(patched["task"], "edited task");

        let response = send(
            &router,
            Method::POST,
            &format!("/api/v1/runs/{run_id}/read"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(json_body(response).await["seenAt"].is_string());

        let response = send(
            &router,
            Method::POST,
            &format!("/api/v1/runs/{run_id}/archive"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["archived"], true);

        let response = send(
            &router,
            Method::DELETE,
            &format!("/api/v1/runs/{run_id}"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json_body(response).await,
            serde_json::json!({ "deleted": true })
        );

        let response = send(
            &router,
            Method::GET,
            &format!("/api/v1/runs/{run_id}"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(response).await["error"], "not found");
        fs::remove_dir_all(repo).expect("remove test repo");
    }

    #[tokio::test]
    async fn runs_routes_keep_default_and_boot_scope_aliases_byte_identical() {
        let (router, repo, _) = seeded_router();
        let boot = repo
            .file_name()
            .and_then(|name| name.to_str())
            .expect("test repo name");
        let mut bodies = Vec::new();
        for path in [
            "/api/v1/runs".to_owned(),
            "/api/v1/p/default/runs".to_owned(),
            format!("/api/v1/p/{boot}/runs"),
        ] {
            let response = send(&router, Method::GET, &path, None).await;
            assert_eq!(response.status(), StatusCode::OK);
            bodies.push(response_body(response).await);
        }
        assert_eq!(bodies[0], bodies[1]);
        assert_eq!(bodies[0], bodies[2]);

        let response = send(&router, Method::GET, "/api/v1/p/no-such-project/runs", None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        fs::remove_dir_all(repo).expect("remove test repo");
    }

    #[tokio::test]
    async fn runs_routes_validate_workflow_selection_and_create_response() {
        let (router, repo, _) = seeded_router();

        let response = send(
            &router,
            Method::POST,
            "/api/v1/runs",
            Some(serde_json::json!({ "task": "missing workflow" })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            json_body(response).await["error"],
            "workflow or steps is required"
        );

        let response = send(
            &router,
            Method::POST,
            "/api/v1/runs",
            Some(serde_json::json!({
                "task": "run inline",
                "steps": [{ "id": "work", "prompt": "{{task}}" }]
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let created = json_body(response).await;
        assert!(created["id"].is_string());
        assert_eq!(created["task"], "run inline");

        let response = send(&router, Method::POST, "/api/v1/runs/read-all", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get("content-type").is_some());
        assert!(json_body(response).await["read"].is_number());

        fs::remove_dir_all(repo).expect("remove test repo");
    }

    #[tokio::test]
    async fn task_patch_is_rejected_after_a_run_leaves_the_queue() {
        let (router, repo, _) = seeded_router();
        let response = send(
            &router,
            Method::POST,
            "/api/v1/runs",
            Some(serde_json::json!({
                "task": "starts and fails without a session factory",
                "steps": [{ "id": "work", "prompt": "{{task}}" }]
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let run_id = json_body(response).await["id"]
            .as_str()
            .expect("created run id")
            .to_owned();

        let response = send(
            &router,
            Method::PATCH,
            &format!("/api/v1/runs/{run_id}"),
            Some(serde_json::json!({ "task": "too late" })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(json_body(response).await["error"], "run already started");
        fs::remove_dir_all(repo).expect("remove test repo");
    }
}
