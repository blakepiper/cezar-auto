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
use std::sync::{Arc, Mutex};

use coducktor_contract::{Runner, RunnerSelection};
use coducktor_core::workflows::run::{
    AgentSession, CancellationToken, EventInput, PromptImage, SessionFactory, SessionOutcome,
    SessionRequest,
};

use crate::agent_runner::{AgentRunSpec, ContentBlock, ImageSource};
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
    cancellations: Arc<Mutex<BTreeMap<String, CancellationToken>>>,
}

struct RegisteredSession {
    run_id: String,
    cancellation: CancellationToken,
    cancellations: Arc<Mutex<BTreeMap<String, CancellationToken>>>,
    inner: Box<dyn AgentSession + Send>,
}

impl Drop for RegisteredSession {
    fn drop(&mut self) {
        self.cancellation.deactivate();
        if let Ok(mut cancellations) = self.cancellations.lock()
            && cancellations
                .get(&self.run_id)
                .is_some_and(|current| current == &self.cancellation)
        {
            cancellations.remove(&self.run_id);
        }
    }
}

impl AgentSession for RegisteredSession {
    fn turn(
        &mut self,
        on_event: &mut dyn FnMut(EventInput) -> std::io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        self.inner.turn(on_event)
    }

    fn send_message(
        &mut self,
        prompt: &str,
        images: &[PromptImage],
        on_event: &mut dyn FnMut(EventInput) -> std::io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        self.inner.send_message(prompt, images, on_event)
    }

    fn finish(
        &mut self,
        on_event: &mut dyn FnMut(EventInput) -> std::io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        self.inner.finish(on_event)
    }

    fn cancel(&mut self) {
        self.inner.cancel();
    }

    fn session_id(&self) -> Option<String> {
        self.inner.session_id()
    }
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
        Self {
            host_env,
            cancellations: Arc::new(Mutex::new(BTreeMap::new())),
        }
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
        cancellation: request.cancellation.clone(),
        system_prompt: request.system_prompt.clone(),
        user_prompt: request.prompt.clone(),
        images: request
            .images
            .iter()
            .map(|image| ContentBlock::Image {
                source: ImageSource {
                    kind: "base64".to_owned(),
                    media_type: image.media_type.clone(),
                    data: image.data.clone(),
                },
            })
            .collect(),
        cwd: request.cwd.clone(),
        allowed_tools: request.allowed_tools.clone(),
        bash_allowlist: request.bash_allowlist.clone(),
        additional_directories: Vec::new(),
        env: request.env.clone(),
        model: request.model.clone(),
        reasoning_effort: request.reasoning_effort,
        timeout_ms: None,
        session_id: request.session_id.clone(),
        resume: request.continuation,
    }
}

impl SessionFactory for DefaultSessionFactory {
    fn open(&self, request: SessionRequest) -> Result<Box<dyn AgentSession + Send>, String> {
        if let Ok(mut cancellations) = self.cancellations.lock() {
            cancellations.insert(request.run_id.clone(), request.cancellation.clone());
        }
        let repo_root = request.cwd.clone();
        let spec = to_agent_run_spec(&request);
        let opened: Result<Box<dyn AgentSession + Send>, String> =
            match resolve_runner(request.runner) {
                Runner::Claude => {
                    let config = self.claude_config(&repo_root);
                    claude_runner::open_claude_session(&config, &spec, &self.host_env)
                        .map(|session| Box::new(session) as Box<dyn AgentSession + Send>)
                }
                Runner::Codex => {
                    let config = self.codex_config();
                    codex_runner::open_codex_session(&config, spec, &self.host_env)
                        .map(|session| Box::new(session) as Box<dyn AgentSession + Send>)
                }
                Runner::OpenCode => {
                    let config = self.opencode_config();
                    opencode_runner::open_opencode_session(&config, spec, &self.host_env)
                        .map(|session| Box::new(session) as Box<dyn AgentSession + Send>)
                }
                Runner::Pi => {
                    let config = self.pi_config(&repo_root);
                    pi_runner::open_pi_session(&config, &spec, &self.host_env)
                        .map(|session| Box::new(session) as Box<dyn AgentSession + Send>)
                }
            };
        let inner = match opened {
            Ok(session) => session,
            Err(error) => {
                if let Ok(mut cancellations) = self.cancellations.lock()
                    && cancellations
                        .get(&request.run_id)
                        .is_some_and(|current| current == &request.cancellation)
                {
                    cancellations.remove(&request.run_id);
                }
                return Err(error);
            }
        };
        Ok(Box::new(RegisteredSession {
            run_id: request.run_id,
            cancellation: request.cancellation,
            cancellations: self.cancellations.clone(),
            inner,
        }))
    }

    fn request_cancel(&self, run_id: &str) -> bool {
        let Ok(cancellations) = self.cancellations.lock() else {
            return false;
        };
        let Some(token) = cancellations.get(run_id) else {
            return false;
        };
        token.request()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct NoopSession;

    impl AgentSession for NoopSession {
        fn turn(
            &mut self,
            _on_event: &mut dyn FnMut(EventInput) -> std::io::Result<()>,
        ) -> Result<SessionOutcome, String> {
            Ok(SessionOutcome::Completed(Default::default()))
        }
    }

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
            cancellation: CancellationToken::default(),
            images: vec![coducktor_core::workflows::run::PromptImage {
                media_type: "image/png".to_owned(),
                data: "AQID".to_owned(),
            }],
            run_id: "run-1".to_owned(),
            step_id: "step-1".to_owned(),
            prompt: "do the thing".to_owned(),
            runner: RunnerSelection::Claude,
            model: Some("sonnet".to_owned()),
            session_id: Some("sess-1".to_owned()),
            continuation: true,
            agent_profile: Some("work".to_owned()),
            env: BTreeMap::from([("CLAUDE_CONFIG_DIR".to_owned(), "/profiles/work".to_owned())]),
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
            spec.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/profiles/work")
        );
        assert_eq!(spec.images.len(), 1);
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
        let factory = factory_with_env(&[("DUCK_DRY_RUN", "1")]);
        let request = SessionRequest {
            cancellation: CancellationToken::default(),
            images: Vec::new(),
            run_id: "run-1".to_owned(),
            step_id: "step-1".to_owned(),
            prompt: "investigate the login redirect bug mock:done".to_owned(),
            runner: RunnerSelection::Claude,
            model: None,
            session_id: None,
            continuation: false,
            agent_profile: None,
            env: BTreeMap::new(),
            cwd: repo_root,
            allowed_tools: vec!["Read".to_owned(), "Bash".to_owned()],
            bash_allowlist: Vec::new(),
            system_prompt: None,
            reasoning_effort: None,
        };
        let mut session = factory.open(request).unwrap();
        assert_eq!(factory.cancellations.lock().unwrap().len(), 1);
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
        drop(session);
        assert!(factory.cancellations.lock().unwrap().is_empty());
    }

    #[test]
    fn registered_session_drop_returns_cancellation_registry_to_baseline_after_repeated_cycles() {
        let cancellations = Arc::new(Mutex::new(BTreeMap::new()));
        for index in 0..1_000 {
            let run_id = format!("run-{index}");
            let cancellation = CancellationToken::default();
            cancellations
                .lock()
                .unwrap()
                .insert(run_id.clone(), cancellation.clone());
            let session = RegisteredSession {
                run_id,
                cancellation: cancellation.clone(),
                cancellations: cancellations.clone(),
                inner: Box::new(NoopSession),
            };

            drop(session);

            assert!(cancellations.lock().unwrap().is_empty());
            assert!(!cancellation.request());
        }
    }
}
