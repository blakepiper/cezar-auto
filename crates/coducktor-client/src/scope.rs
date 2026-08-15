use url::Url;

use crate::error::EngineError;

/// The single version prefix used by every new request.
pub const API_PREFIX: &str = "/api/v1";

/// Whether a request targets the workspace or one registered project.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    Workspace,
    Project(String),
}

/// Build a versioned URL from a route and scope.
pub fn api_path(base: &Url, scope: &Scope, route: &str) -> Result<Url, EngineError> {
    let route_url = Url::parse(&format!("http://scope.invalid{}", normalized_route(route)))
        .map_err(|error| EngineError::Transport(error.to_string()))?;
    let route_path = route_url.path();
    let scoped_route = match scope {
        Scope::Workspace if is_workspace_route(route_path) => route_path.to_owned(),
        Scope::Workspace => route_path.to_owned(),
        Scope::Project(project_id) if is_workspace_route(route_path) => route_path.to_owned(),
        Scope::Project(project_id) => {
            format!(
                "/p/{}/{}",
                encode_path_segment(project_id),
                route_path.trim_start_matches('/')
            )
        }
    };

    let base_path = base.path().trim_end_matches('/');
    let mut result = base.clone();
    result.set_path(&format!("{base_path}{API_PREFIX}{scoped_route}"));
    result.set_query(route_url.query());
    Ok(result)
}

/// Workspace-level routes have one mount and are never project-prefixed.
pub fn is_workspace_route(route: &str) -> bool {
    let path = route.split(['?', '#']).next().unwrap_or(route);
    path == "/health"
        || path == "/models"
        || path.starts_with("/models/")
        || path == "/projects"
        || path.starts_with("/projects/")
        || path == "/workspace"
        || path.starts_with("/workspace/")
        || path == "/fs"
        || path.starts_with("/fs/")
        || path == "/providers"
        || path.starts_with("/providers/")
}

/// Encode one project id as a URL path segment.
pub fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(hex(byte >> 4));
            encoded.push(hex(byte & 0x0f));
        }
    }
    encoded
}

fn normalized_route(route: &str) -> String {
    if route.starts_with('/') {
        route.to_owned()
    } else {
        format!("/{route}")
    }
}

fn hex(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'A' + value - 10) as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_project_routes_but_not_workspace_routes() {
        let base = Url::parse("http://127.0.0.1:4321").unwrap();
        assert_eq!(
            api_path(&base, &Scope::Project("a b/c".into()), "/runs")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:4321/api/v1/p/a%20b%2Fc/runs"
        );
        assert_eq!(
            api_path(&base, &Scope::Project("shop".into()), "/workspace/config")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:4321/api/v1/workspace/config"
        );
    }
}
