//! Mirrors `packages/coducktor/src/paths.ts` (deleted at B12). Every path the workspace layer
//! touches goes through here so `DUCK_HOME` (see [`coducktor_home_dir`]) is resolved in
//! exactly one place — do not re-derive `home_dir()` elsewhere.

use std::env;
use std::path::{Path, PathBuf};

/// An environment lookup, so path resolution is testable without mutating process env
/// (the TS source takes an optional `env: NodeJS.ProcessEnv` parameter for the same
/// reason — every workspace test pins its own sandbox rather than touching the real one).
pub trait EnvSource {
    fn get(&self, key: &str) -> Option<String>;
}

/// Reads the real process environment. The production default everywhere below.
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        env::var(key).ok()
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty())
}

/// The user's real home directory. The one place this crate calls into `$HOME`/`$USERPROFILE`
/// directly — everything else goes through [`coducktor_home_dir`] or [`agent_home_paths`].
pub fn real_home_dir(env: &dyn EnvSource) -> PathBuf {
    if let Some(home) = non_empty(env.get("HOME")) {
        return PathBuf::from(home);
    }
    if let Some(profile) = non_empty(env.get("USERPROFILE")) {
        return PathBuf::from(profile);
    }
    // Last resort: the OS-reported home. `std` has no built-in for this (that's what the
    // `dirs` crate is for), but every supported platform here sets `$HOME`/`$USERPROFILE`.
    PathBuf::from(".")
}

/// Per-user coducktor home. Literal `~/.coducktor` on every platform — no XDG, no
/// `%LOCALAPPDATA%` branch, mirroring `paths.ts::coducktorHomeDir`'s one rule.
///
/// `DUCK_HOME` (spec §2.2.1 decision 6) wins if set, else the real home directory. An
/// **empty** override falls back rather than resolving to a relative cwd path, matching the
/// TS `env.DUCK_HOME || undefined` guard's own empty-string handling.
///
/// B12 dropped the `DUCK_HOME` dual-read this function carried through the whole of Phase B —
/// it existed only so the still-live Node server (`packages/coducktor`, which read `DUCK_HOME`
/// and nothing else) and this crate resolved the SAME `~/.coducktor` while both implementations
/// coexisted. `packages/coducktor` is deleted now, so there is no second reader left to stay in
/// sync with.
pub fn coducktor_home_dir(env: &dyn EnvSource) -> PathBuf {
    if let Some(duck_home) = non_empty(env.get("DUCK_HOME")) {
        return PathBuf::from(duck_home);
    }
    real_home_dir(env).join(".coducktor")
}

/// Per-user workspace config: schema version, global defaults, and the project registry.
/// Mirrors `paths.ts::workspaceConfigPath`.
pub fn workspace_config_path(env: &dyn EnvSource) -> PathBuf {
    coducktor_home_dir(env).join("config.json")
}

/// Global GUI state — the workspace twin of the per-repo `.ai/coducktor/ui-state.json`.
/// Mirrors `paths.ts::workspaceUiStatePath`.
pub fn workspace_ui_state_path(env: &dyn EnvSource) -> PathBuf {
    coducktor_home_dir(env).join("ui-state.json")
}

/// Extra config dirs for a second login of the same agent CLI, plus which one each
/// project uses. Mirrors `paths.ts::agentAccountsPath`.
pub fn agent_accounts_path(env: &dyn EnvSource) -> PathBuf {
    coducktor_home_dir(env).join("agent-accounts.json")
}

/// Expand a leading `~` to the user's home. Mirrors `paths.ts::expandTilde` — the
/// workspace checkout root is stored as the user wrote it (a literal `~`), so every
/// consumer that must touch the real directory expands it through this one helper.
pub fn expand_tilde(path: &str, env: &dyn EnvSource) -> PathBuf {
    if path == "~" {
        return real_home_dir(env);
    }
    match path.strip_prefix("~/") {
        Some(rest) => real_home_dir(env).join(rest),
        None => PathBuf::from(path),
    }
}

/// Where each coding agent keeps its per-user config. Mirrors `paths.ts::agentHomePaths`.
pub struct AgentHomePaths {
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub opencode_config: PathBuf,
}

/// Honors the env vars the vendors document: `$CLAUDE_CONFIG_DIR` relocates Claude
/// Code's home; `$CODEX_HOME` relocates Codex's; `$XDG_CONFIG_HOME` relocates OpenCode's
/// config dir (falling back to `~/.config`). These are the DEFAULT profile's dirs — a
/// second login of the same CLI is an agent account and resolves through
/// `workspace::agent_accounts` instead.
pub fn agent_home_paths(env: &dyn EnvSource) -> AgentHomePaths {
    let home = if let Some(home) = non_empty(env.get("HOME")) {
        PathBuf::from(home)
    } else if let Some(profile) = non_empty(env.get("USERPROFILE")) {
        PathBuf::from(profile)
    } else {
        real_home_dir(env)
    };
    let xdg_config = non_empty(env.get("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    AgentHomePaths {
        claude: non_empty(env.get("CLAUDE_CONFIG_DIR"))
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude")),
        codex: non_empty(env.get("CODEX_HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex")),
        opencode_config: xdg_config.join("opencode"),
    }
}

/// True when `path` is absolute on `windows` (posix otherwise) — the account routes' one
/// path rule. Mirrors `workspace/agent-accounts.ts::isAbsoluteConfigDir`: a leading-`/`
/// test alone wrongly refuses real Windows paths (`C:\Users\me\...`) and UNC paths.
pub fn is_absolute_config_dir(path: &str, windows: bool) -> bool {
    if windows {
        // Drive-letter (`C:\...`, `C:/...`) or UNC (`\\server\share`, `//server/share`).
        let bytes = path.as_bytes();
        let drive = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/');
        let unc = path.starts_with("\\\\") || path.starts_with("//");
        drive || unc
    } else {
        Path::new(path).is_absolute()
    }
}

#[cfg(test)]
pub(crate) mod test_env {
    use super::EnvSource;
    use std::collections::BTreeMap;

    /// A fixed env snapshot for tests — mirrors passing an explicit `env` object in the
    /// TS test suite instead of mutating `process.env`.
    #[derive(Default)]
    pub struct FixedEnv(pub BTreeMap<String, String>);

    impl FixedEnv {
        pub fn new(pairs: &[(&str, &str)]) -> Self {
            Self(
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            )
        }
    }

    impl EnvSource for FixedEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_env::FixedEnv;
    use super::*;

    #[test]
    fn duck_home_is_honored_when_set() {
        let env = FixedEnv::new(&[("DUCK_HOME", "/duck")]);
        assert_eq!(coducktor_home_dir(&env), PathBuf::from("/duck"));
    }

    #[test]
    fn an_empty_override_falls_back_to_the_default() {
        let env = FixedEnv::new(&[("DUCK_HOME", ""), ("HOME", "/home/pat")]);
        assert_eq!(
            coducktor_home_dir(&env),
            PathBuf::from("/home/pat/.coducktor")
        );
    }

    #[test]
    fn workspace_paths_live_under_the_home_dir() {
        let env = FixedEnv::new(&[("DUCK_HOME", "/duck")]);
        assert_eq!(
            workspace_config_path(&env),
            PathBuf::from("/duck/config.json")
        );
        assert_eq!(
            workspace_ui_state_path(&env),
            PathBuf::from("/duck/ui-state.json")
        );
        assert_eq!(
            agent_accounts_path(&env),
            PathBuf::from("/duck/agent-accounts.json")
        );
    }

    #[test]
    fn expand_tilde_only_touches_a_leading_tilde() {
        let env = FixedEnv::new(&[("HOME", "/home/pat")]);
        assert_eq!(expand_tilde("~", &env), PathBuf::from("/home/pat"));
        assert_eq!(
            expand_tilde("~/projects", &env),
            PathBuf::from("/home/pat/projects")
        );
        assert_eq!(
            expand_tilde("/already/absolute", &env),
            PathBuf::from("/already/absolute")
        );
        assert_eq!(
            expand_tilde("relative/path", &env),
            PathBuf::from("relative/path")
        );
    }

    #[test]
    fn agent_home_paths_honor_vendor_env_vars() {
        let env = FixedEnv::new(&[
            ("HOME", "/home/pat"),
            ("CLAUDE_CONFIG_DIR", "/opt/claude"),
            ("CODEX_HOME", "/opt/codex"),
        ]);
        let paths = agent_home_paths(&env);
        assert_eq!(paths.claude, PathBuf::from("/opt/claude"));
        assert_eq!(paths.codex, PathBuf::from("/opt/codex"));
        assert_eq!(
            paths.opencode_config,
            PathBuf::from("/home/pat/.config/opencode")
        );
    }

    #[test]
    fn agent_home_paths_default_under_home() {
        let env = FixedEnv::new(&[("HOME", "/home/pat")]);
        let paths = agent_home_paths(&env);
        assert_eq!(paths.claude, PathBuf::from("/home/pat/.claude"));
        assert_eq!(paths.codex, PathBuf::from("/home/pat/.codex"));
        assert_eq!(
            paths.opencode_config,
            PathBuf::from("/home/pat/.config/opencode")
        );
    }

    #[test]
    fn is_absolute_config_dir_handles_windows_drive_and_unc_paths() {
        assert!(is_absolute_config_dir(r"C:\Users\me\.claude-work", true));
        assert!(is_absolute_config_dir(r"\\server\share", true));
        assert!(!is_absolute_config_dir("relative\\path", true));
        assert!(is_absolute_config_dir("/home/pat/.claude", false));
        assert!(!is_absolute_config_dir("relative/path", false));
    }
}
