//! Which browser UI a GET request gets. Ported from
//! `packages/cezar/src/server/static-ui.ts` for B11's cutover/soak step — the React cockpit
//! served from `coducktor-server` is the soak's whole point (the browser is the last
//! independent exerciser of the API before Phase B deletes it). Pure decision helpers only;
//! the actual file I/O lives in `lib.rs`'s route handlers, same split as the TS source.
//!
//! This module (and the routes it backs) is deleted whole at C2/C3 along with the rest of
//! `coducktor-server` — it exists only for the Phase B soak, never for Phase C.

/// Which target `/` (and every other non-API, non-static-asset GET) resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexTarget {
    /// The built React app (`web/dist/index.html`).
    Dist,
    /// No build present — a self-contained built-in hint page, never a 404.
    BuildHint,
}

/// [`IndexTarget`] plus `Passthrough` — paths owned by routes registered before the catch-all
/// (`/api/*` and the static asset routes), which keep their own behavior (including their own
/// 404s) untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GetTarget {
    Dist,
    BuildHint,
    Passthrough,
}

/// Pick the response for `/`, given whether the build exists.
pub fn resolve_index_html(dist_exists: bool) -> IndexTarget {
    if dist_exists {
        IndexTarget::Dist
    } else {
        IndexTarget::BuildHint
    }
}

/// Paths owned by routes registered before the catch-all: the built app's hashed bundles and
/// the favicon.
fn is_static_asset(path: &str) -> bool {
    path.starts_with("/assets/") || path == "/open-mercato.svg"
}

/// Decide what any GET gets, so every deep-linkable route (`/tasks/:id/changes`,
/// `/settings/skills`, …) cold-loads and survives a refresh. Unknown paths deliberately
/// resolve to the shell, not a 404 — react-router owns the 404, it is the only side that knows
/// the route map. `/api/*` and the static asset paths pass through untouched.
pub fn resolve_get_request(path: &str, dist_exists: bool) -> GetTarget {
    if path == "/api" || path.starts_with("/api/") || is_static_asset(path) {
        return GetTarget::Passthrough;
    }
    match resolve_index_html(dist_exists) {
        IndexTarget::Dist => GetTarget::Dist,
        IndexTarget::BuildHint => GetTarget::BuildHint,
    }
}

/// The dev fallback page served for every shell route when `web/dist` is missing. Built into
/// the binary — needs no files on disk.
pub const BUILD_HINT_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>coducktor — build the cockpit</title>
<style>
  body { margin: 0; display: grid; place-items: center; min-height: 100dvh;
         font: 15px/1.6 system-ui, sans-serif; background: #101014; color: #e8e8ea; }
  main { max-width: 34rem; padding: 2rem; }
  code { font-family: ui-monospace, monospace; background: #1c1c22; border-radius: 6px; padding: 2px 6px; }
  p { color: #a0a0aa; }
</style>
</head>
<body>
<main>
  <h1>The cockpit isn&rsquo;t built yet</h1>
  <p>This checkout has no <code>web/dist</code>. Run <code>npm run build:web</code>
  and reload &mdash; or use <code>npm run dev:web</code> for the live dev server.</p>
</main>
</body>
</html>
"#;

/// Content type for a hashed file under `web/dist/assets/`.
pub fn asset_content_type(file: &str) -> &'static str {
    match file.rsplit('.').next().map(str::to_lowercase).as_deref() {
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("png") => "image/png",
        Some("jpg") => "image/jpeg",
        Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("json") => "application/json; charset=utf-8",
        Some("map") => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// True only for a plain filename the `/assets/:file` route may serve. `basename("..")` is
/// `".."` in the TS oracle — not a guard on its own, since that joins back to the assets dir
/// itself. Dot-segments, separators, and NUL all mean "not a file we ship".
pub fn is_safe_asset_filename(file: &str) -> bool {
    !file.is_empty()
        && file != "."
        && file != ".."
        && !file.contains('/')
        && !file.contains('\\')
        && !file.contains('\0')
}

/// Vite fingerprints every filename under `assets/`, so the bytes behind a URL can never
/// change — cache them for a year.
pub const ASSET_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_index_html_matches_dist_presence() {
        assert_eq!(resolve_index_html(true), IndexTarget::Dist);
        assert_eq!(resolve_index_html(false), IndexTarget::BuildHint);
    }

    #[test]
    fn resolve_get_request_sends_deep_links_and_unknown_paths_to_the_shell() {
        for path in [
            "/",
            "/tasks/x",
            "/tasks/x/changes",
            "/settings/skills",
            "/nope",
            "/new",
            "/apidocs",
            "/app.js",
            "/style.css",
        ] {
            assert_eq!(
                resolve_get_request(path, true),
                GetTarget::Dist,
                "path {path:?} should resolve to the shell"
            );
        }
    }

    #[test]
    fn resolve_get_request_never_shadows_the_api_or_static_assets() {
        for path in [
            "/api/v1/runs",
            "/api/v1/runs/x/events",
            "/api/v1/nope",
            "/api",
            "/assets/index-abc123.js",
            "/open-mercato.svg",
        ] {
            assert_eq!(
                resolve_get_request(path, true),
                GetTarget::Passthrough,
                "path {path:?} should pass through"
            );
        }
        // Passthrough is about ownership, not about the build being there.
        assert_eq!(
            resolve_get_request("/assets/x.js", false),
            GetTarget::Passthrough
        );
        assert_eq!(
            resolve_get_request("/api/v1/runs", false),
            GetTarget::Passthrough
        );
    }

    #[test]
    fn resolve_get_request_falls_back_to_the_hint_page_with_no_build() {
        for path in ["/", "/tasks/x", "/new"] {
            assert_eq!(resolve_get_request(path, false), GetTarget::BuildHint);
        }
    }

    #[test]
    fn build_hint_html_names_both_build_commands_and_is_self_contained() {
        assert!(BUILD_HINT_HTML.contains("npm run build:web"));
        assert!(BUILD_HINT_HTML.contains("npm run dev:web"));
        assert!(BUILD_HINT_HTML.contains("<!doctype html>"));
        assert!(!BUILD_HINT_HTML.contains("src="));
        assert!(!BUILD_HINT_HTML.contains("href="));
    }

    #[test]
    fn is_safe_asset_filename_rejects_traversal_and_separators() {
        assert!(is_safe_asset_filename("index-D1sxO2Tm.js"));
        assert!(is_safe_asset_filename(
            "inter-latin-wght-normal-Dx4kXJAl.woff2"
        ));
        assert!(is_safe_asset_filename(".hidden"));
        assert!(!is_safe_asset_filename(".."));
        assert!(!is_safe_asset_filename("."));
        assert!(!is_safe_asset_filename(""));
        assert!(!is_safe_asset_filename("../index.html"));
        assert!(!is_safe_asset_filename("..\\index.html"));
        assert!(!is_safe_asset_filename("sub/dir.js"));
        assert!(!is_safe_asset_filename("file\0.js"));
    }

    #[test]
    fn asset_content_type_matches_the_documented_table() {
        assert_eq!(
            asset_content_type("index-D1sxO2Tm.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            asset_content_type("index-VovY6R-i.css"),
            "text/css; charset=utf-8"
        );
        assert_eq!(
            asset_content_type("open-mercato-toBr6SOa.svg"),
            "image/svg+xml"
        );
        assert_eq!(
            asset_content_type("inter-latin-wght-normal-Dx4kXJAl.woff2"),
            "font/woff2"
        );
        assert_eq!(asset_content_type("logo-abc123.PNG"), "image/png");
        assert_eq!(
            asset_content_type("something-abc123.bin"),
            "application/octet-stream"
        );
        assert_eq!(
            asset_content_type("noextension"),
            "application/octet-stream"
        );
    }

    #[test]
    fn asset_cache_control_marks_hashed_assets_immutable_for_a_year() {
        assert_eq!(ASSET_CACHE_CONTROL, "public, max-age=31536000, immutable");
    }
}
