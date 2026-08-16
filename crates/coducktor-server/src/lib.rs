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

use axum::body::Body;
use axum::extract::State;
use axum::http::header::HeaderName;
use axum::http::header::{ACCESS_CONTROL_ALLOW_ORIGIN, HOST, ORIGIN};
use axum::http::{HeaderValue, Method, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use coducktor_contract::{
    BackendCheck, BackendCheckName, Capabilities, ForgeInfo, ForgeKind, HealthProject,
    HealthResponse, RepoInfo, RunnerSelection,
};
use serde::Serialize;

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
#[derive(Debug, Clone)]
pub struct ServerState {
    config: Arc<ServerConfig>,
}

impl ServerState {
    pub fn new(config: ServerConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }
}

/// Build the versioned HTTP application.
pub fn router(config: ServerConfig) -> Router {
    let state = ServerState::new(config);
    Router::new()
        .route(HEALTH_PATH, get(health))
        .fallback(not_found)
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, request_origin_guard))
}

/// Build a router from already constructed state. This is the seam used by route-family tests.
pub fn router_with_state(state: ServerState) -> Router {
    Router::new()
        .route(HEALTH_PATH, get(health))
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

async fn request_origin_guard(request: Request<Body>, next: Next) -> Response {
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
    use tower::ServiceExt;

    fn test_router() -> Router {
        router(ServerConfig::new("/tmp/not-a-repo", "test"))
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
}
