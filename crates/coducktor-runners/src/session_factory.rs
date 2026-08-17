//! A concrete `coducktor_core::workflows::run::SessionFactory` dispatching to the four real
//! backends (claude/codex/opencode/pi) by `RunnerSelection`.
//!
//! Binary resolution follows each runner's supported configuration:
//! - claude/pi: `DUCK_CLAUDE_BIN`/`DUCK_PI_BIN` override, else — when `DUCK_DRY_RUN=1` — the bundled
//!   bundled mock script, else the bare binary name on PATH.
//! - codex/opencode: `DUCK_CODEX_BIN`/`DUCK_OPENCODE_BIN` override, else the bare binary name on
//!   PATH — these runners have no `DUCK_DRY_RUN` fallback.
//!
//! The dry-run mock scripts live under the repository's root-level `fixtures/` directory. They
//! are resolved relative to `SessionRequest.cwd`, which is the repository root for a run.

use std::collections::BTreeMap;
use std::path::Path;

use coducktor_contract::{Runner, RunnerSelection};
use coducktor_core::workflows::run::{AgentSession, SessionFactory, SessionRequest};

use crate::agent_runner::AgentRunSpec;
use crate::claude_runner::{self, ClaudeSpawnConfig};
use crate::codex_runner::{self, CodexSpawnConfig};
use crate::opencode_runner::{self, OpencodeSpawnConfig};
use crate::pi_runner::{self, PiSpawnConfig};

const MOCK_CLAUDE_RELATIVE: &str = "fixtures/scripts/mock-claude.mjs";
const MOCK_PI_RELATIVE: &str = "fixtures/scripts/mock-pi-rpc.mjs";

/// Production `SessionFactory`: spawns the real agent CLI (or, for claude/pi under
/// `DUCK_DRY_RUN=1`, the bundled mock) for whichever backend a [`SessionRequest`] names.
pub struct DefaultSessionFactory {
    host_env: BTreeMap<String, String>,
}

impl DefaultSessionFactory {
    /// Captures the current process environment once — every backend spawn reads from this
    /// snapshot rather than re-querying `std::env` per session, matching how every backend's own
    /// test suite already passes a fixed `host_env` map rather than the live environment.
    pub fn new() -> Self {
        Self::with_env(std::env::vars().collect())
    }

    /// Same as [`Self::new`], but over an explicit env snapshot rather than the live process
    /// environment — the seam a caller (a test, or a future non-CLI embedder) uses to get
    /// deterministic backend resolution without mutating global process state.
    pub fn with_env(host_env: BTreeMap<String, String>) -> Self {
        Self { host_env }
    }

    fn dry_run(&self) -> bool {
        self.host_env.get("DUCK_DRY_RUN").map(String::as_str) == Some("1")
    }

    fn mock_node_config(&self, repo_root: &Path, relative: &str) -> (String, Vec<String>) {
        let script = repo_root.join(relative);
        (
            "node".to_owned(),
            vec![script.to_string_lossy().into_owned()],
        )
    }

    fn claude_config(&self, repo_root: &Path) -> ClaudeSpawnConfig {
        let mut config = ClaudeSpawnConfig::default();
        if let Some(bin) = self.host_env.get("DUCK_CLAUDE_BIN") {
            config.program = bin.clone();
        } else if self.dry_run() {
            let (program, args) = self.mock_node_config(repo_root, MOCK_CLAUDE_RELATIVE);
            config.program = program;
            config.prefix_args = args;
        }
        config
    }

    fn codex_config(&self) -> CodexSpawnConfig {
        let mut config = CodexSpawnConfig::default();
        if let Some(bin) = self.host_env.get("DUCK_CODEX_BIN") {
            config.program = bin.clone();
        }
        config
    }

    fn opencode_config(&self) -> OpencodeSpawnConfig {
        let mut config = OpencodeSpawnConfig::default();
        if let Some(bin) = self.host_env.get("DUCK_OPENCODE_BIN") {
            config.program = bin.clone();
        }
        config
    }

    fn pi_config(&self, repo_root: &Path) -> PiSpawnConfig {
        let mut config = PiSpawnConfig::default();
        if let Some(bin) = self.host_env.get("DUCK_PI_BIN") {
            config.program = bin.clone();
        } else if self.dry_run() {
            let (program, args) = self.mock_node_config(repo_root, MOCK_PI_RELATIVE);
            config.program = program;
            config.prefix_args = args;
        }
        config
    }
}

impl Default for DefaultSessionFactory {
    fn default() -> Self {
        Self::new()
    }
}

/// `RunnerSelection::Auto` falls back to claude — the same default `RunManager::execute_job`
/// itself already applies (`.unwrap_or(RunnerSelection::Claude)`) when nothing more specific was
/// requested; a factory should never actually observe `Auto` in practice; this exists so `open`
/// is total rather than failing on it.
fn resolve_runner(selection: RunnerSelection) -> Runner {
    match selection {
        RunnerSelection::Claude | RunnerSelection::Auto => Runner::Claude,
        RunnerSelection::Codex => Runner::Codex,
        RunnerSelection::OpenCode => Runner::OpenCode,
        RunnerSelection::Pi => Runner::Pi,
    }
}

fn to_agent_run_spec(request: &SessionRequest) -> AgentRunSpec {
    AgentRunSpec {
        system_prompt: request.system_prompt.clone(),
        user_prompt: request.prompt.clone(),
        images: Vec::new(),
        cwd: request.cwd.clone(),
        allowed_tools: request.allowed_tools.clone(),
        bash_allowlist: request.bash_allowlist.clone(),
        additional_directories: Vec::new(),
        env: BTreeMap::new(),
        model: request.model.clone(),
        reasoning_effort: request.reasoning_effort,
        timeout_ms: None,
        session_id: request.session_id.clone(),
        resume: request.continuation,
    }
}

impl SessionFactory for DefaultSessionFactory {
    fn open(&mut self, request: SessionRequest) -> Result<Box<dyn AgentSession + Send>, String> {
        let repo_root = request.cwd.clone();
        let spec = to_agent_run_spec(&request);
        match resolve_runner(request.runner) {
            Runner::Claude => {
                let config = self.claude_config(&repo_root);
                let session = claude_runner::open_claude_session(&config, &spec, &self.host_env)?;
                Ok(Box::new(session))
            }
            Runner::Codex => {
                let config = self.codex_config();
                let session = codex_runner::open_codex_session(&config, spec, &self.host_env)?;
                Ok(Box::new(session))
            }
            Runner::OpenCode => {
                let config = self.opencode_config();
                let session =
                    opencode_runner::open_opencode_session(&config, spec, &self.host_env)?;
                Ok(Box::new(session))
            }
            Runner::Pi => {
                let config = self.pi_config(&repo_root);
                let session = pi_runner::open_pi_session(&config, &spec, &self.host_env)?;
                Ok(Box::new(session))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn factory_with_env(pairs: &[(&str, &str)]) -> DefaultSessionFactory {
        DefaultSessionFactory::with_env(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        )
    }

    #[test]
    fn resolve_runner_defaults_auto_to_claude() {
        assert_eq!(resolve_runner(RunnerSelection::Auto), Runner::Claude);
        assert_eq!(resolve_runner(RunnerSelection::Claude), Runner::Claude);
        assert_eq!(resolve_runner(RunnerSelection::Codex), Runner::Codex);
        assert_eq!(resolve_runner(RunnerSelection::OpenCode), Runner::OpenCode);
        assert_eq!(resolve_runner(RunnerSelection::Pi), Runner::Pi);
    }

    #[test]
    fn claude_config_prefers_the_env_override_over_dry_run() {
        let factory =
            factory_with_env(&[("DUCK_CLAUDE_BIN", "/opt/claude"), ("DUCK_DRY_RUN", "1")]);
        let config = factory.claude_config(Path::new("/repo"));
        assert_eq!(config.program, "/opt/claude");
        assert!(config.prefix_args.is_empty());
    }

    #[test]
    fn claude_config_falls_back_to_the_bundled_mock_under_dry_run() {
        let factory = factory_with_env(&[("DUCK_DRY_RUN", "1")]);
        let config = factory.claude_config(Path::new("/repo"));
        assert_eq!(config.program, "node");
        assert_eq!(
            config.prefix_args,
            vec!["/repo/fixtures/scripts/mock-claude.mjs".to_owned()]
        );
    }

    #[test]
    fn claude_config_defaults_to_the_bare_binary_name_outside_dry_run() {
        let factory = factory_with_env(&[]);
        let config = factory.claude_config(Path::new("/repo"));
        assert_eq!(config.program, "claude");
        assert!(config.prefix_args.is_empty());
    }

    #[test]
    fn pi_config_follows_the_same_dry_run_convention_as_claude() {
        let factory = factory_with_env(&[("DUCK_DRY_RUN", "1")]);
        let config = factory.pi_config(Path::new("/repo"));
        assert_eq!(config.program, "node");
        assert_eq!(
            config.prefix_args,
            vec!["/repo/fixtures/scripts/mock-pi-rpc.mjs".to_owned()]
        );
    }

    #[test]
    fn codex_config_has_no_dry_run_fallback() {
        let factory = factory_with_env(&[("DUCK_DRY_RUN", "1")]);
        let config = factory.codex_config();
        assert_eq!(config.program, "codex");
        assert!(config.prefix_args.is_empty());
    }

    #[test]
    fn opencode_config_has_no_dry_run_fallback() {
        let factory = factory_with_env(&[("DUCK_DRY_RUN", "1")]);
        let config = factory.opencode_config();
        assert_eq!(config.program, "opencode");
        assert!(config.prefix_args.is_empty());
    }

    #[test]
    fn codex_config_honors_its_own_env_override() {
        let factory = factory_with_env(&[("DUCK_CODEX_BIN", "/opt/codex")]);
        assert_eq!(factory.codex_config().program, "/opt/codex");
    }

    #[test]
    fn opencode_config_honors_its_own_env_override() {
        let factory = factory_with_env(&[("DUCK_OPENCODE_BIN", "/opt/opencode")]);
        assert_eq!(factory.opencode_config().program, "/opt/opencode");
    }

    #[test]
    fn to_agent_run_spec_carries_the_session_request_fields_through() {
        let request = SessionRequest {
            run_id: "run-1".to_owned(),
            step_id: "step-1".to_owned(),
            prompt: "do the thing".to_owned(),
            runner: RunnerSelection::Claude,
            model: Some("sonnet".to_owned()),
            session_id: Some("sess-1".to_owned()),
            continuation: true,
            cwd: PathBuf::from("/repo"),
            allowed_tools: vec!["Read".to_owned()],
            bash_allowlist: vec!["npm test".to_owned()],
            system_prompt: Some("Be careful.".to_owned()),
            reasoning_effort: Some(coducktor_contract::ConcreteReasoningEffort::High),
        };
        let spec = to_agent_run_spec(&request);
        assert_eq!(spec.user_prompt, "do the thing");
        assert_eq!(spec.cwd, PathBuf::from("/repo"));
        assert_eq!(spec.allowed_tools, vec!["Read".to_owned()]);
        assert_eq!(spec.bash_allowlist, vec!["npm test".to_owned()]);
        assert_eq!(spec.system_prompt.as_deref(), Some("Be careful."));
        assert_eq!(spec.model.as_deref(), Some("sonnet"));
        assert_eq!(spec.session_id.as_deref(), Some("sess-1"));
        assert!(spec.resume);
        assert_eq!(
            spec.reasoning_effort,
            Some(coducktor_contract::ConcreteReasoningEffort::High)
        );
    }

    /// End-to-end proof that the factory's dry-run path resolution actually finds the real
    /// `mock-claude.mjs` in this checkout and opens a working session through it — not just that
    /// the string-building helpers above compute the right path.
    #[test]
    fn open_spawns_a_working_claude_session_under_dry_run() {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let mut factory = factory_with_env(&[("DUCK_DRY_RUN", "1")]);
        let request = SessionRequest {
            run_id: "run-1".to_owned(),
            step_id: "step-1".to_owned(),
            prompt: "investigate the login redirect bug mock:done".to_owned(),
            runner: RunnerSelection::Claude,
            model: None,
            session_id: None,
            continuation: false,
            cwd: repo_root,
            allowed_tools: vec!["Read".to_owned(), "Bash".to_owned()],
            bash_allowlist: Vec::new(),
            system_prompt: None,
            reasoning_effort: None,
        };
        let mut session = factory.open(request).unwrap();
        let mut event_types = Vec::new();
        let outcome = session
            .turn(&mut |event| {
                event_types.push(event.event_type.clone());
                Ok(())
            })
            .unwrap();
        assert!(event_types.contains(&"text".to_owned()));
        assert!(matches!(
            outcome,
            coducktor_core::workflows::run::SessionOutcome::Completed(_)
        ));
        session.finish(&mut |_| Ok(())).unwrap();
    }
}
