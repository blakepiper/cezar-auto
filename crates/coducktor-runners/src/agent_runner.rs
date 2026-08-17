//! The backend-agnostic seam shared by every agent-CLI backend runner. Ported from
//! `packages/cezar/src/core/agent-runner.ts`, ahead of the concrete backends
//! (B9a.2b-2e) so each one plugs into the same spawn/signal/termination-tracking
//! primitives instead of re-deriving them. `RunnerId`/`AgentBackend` are not
//! re-ported here: `coducktor_contract::Runner`/`RunnerSelection` already cover
//! that enumeration (A1).

use std::collections::BTreeMap;
use std::path::PathBuf;

use coducktor_contract::ConcreteReasoningEffort;
use serde::{Deserialize, Serialize};

/// Everything one agent-CLI backend needs to spawn and drive a session. Ported from
/// `agent-runner.ts`'s `AgentRunSpec`, shared by every backend (claude first, at B9a.2b;
/// codex/opencode/pi follow at 2c-2e).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentRunSpec {
    /// Appended to the CLI's default system prompt (`--append-system-prompt` for claude;
    /// prepended to the opening message via [`prepend_system_prompt`] for backends with no
    /// dedicated channel).
    pub system_prompt: Option<String>,
    pub user_prompt: String,
    /// Image blocks delivered with the first user message — screenshots pasted into the new-task
    /// form, at task start.
    pub images: Vec<ContentBlock>,
    /// The directory the agent runs in — also the only writable root.
    pub cwd: PathBuf,
    /// Tool allowlist; the CLI is default-deny for anything not listed.
    pub allowed_tools: Vec<String>,
    /// When `Bash` is allowed, restrict it to commands starting with one of these.
    pub bash_allowlist: Vec<String>,
    /// Extra directories the agent may read/write besides `cwd`.
    pub additional_directories: Vec<String>,
    /// Extra env vars for the agent process (merged over the curated child env from
    /// `agent_env::build_child_env`).
    pub env: BTreeMap<String, String>,
    pub model: Option<String>,
    /// Concrete reasoning level for this session — the run manager resolves `auto` before spawn.
    pub reasoning_effort: Option<ConcreteReasoningEffort>,
    /// Wall-clock kill switch for the run (ms). `None` uses the backend's own default; `Some(0)`
    /// disables it entirely (interactive sessions).
    pub timeout_ms: Option<u64>,
    /// Stable session id (UUID) so the user can take over interactively later.
    pub session_id: Option<String>,
    /// Spawn with `--resume <sessionId>` instead of starting a fresh session — picks up the
    /// on-disk conversation (used by "Continue" after a run ends).
    pub resume: bool,
}

/// One content block of a user message — mirrors the Anthropic wire format so it can be
/// written to the claude CLI's stdin verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    Image { source: ImageSource },
}

/// The nested `source` object of an image content block. `kind` is always `"base64"` on the
/// wire — kept as a field rather than a hardcoded literal so a round-tripped block serializes
/// byte-identical to what it deserialized from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub kind: String,
    pub media_type: String,
    pub data: String,
}

/// Backends without a dedicated system-prompt channel (codex app-server, opencode serve)
/// deliver `system_prompt` as a leading block of the opening user message — the documented
/// per-backend mapping (spec protocol v2: claude = `--append-system-prompt`, codex/opencode =
/// prepended here).
pub fn prepend_system_prompt(system_prompt: Option<&str>, user_prompt: &str) -> String {
    match system_prompt {
        Some(system_prompt) => format!("{system_prompt}\n\n---\n\n{user_prompt}"),
        None => user_prompt.to_owned(),
    }
}

/// True for the `128 + signal` exit codes an agent CLI reports when it handles a stop signal
/// itself instead of dying from it (SIGINT/SIGKILL/SIGTERM).
///
/// Every backend arms a SIGTERM->SIGKILL watchdog on a session's `finish()` and signals on
/// `cancel()` (#703): the CLIs install their own handlers, so a session the runner tore down on
/// purpose comes back as a NON-ZERO exit. Paired with a "we sent the signal" flag, this
/// predicate keeps that teardown out of the error path — an exit coducktor caused is never an
/// agent failure.
pub fn is_signal_termination_exit(exit_code: Option<i32>) -> bool {
    matches!(exit_code, Some(130) | Some(137) | Some(143))
}

/// The slice of a spawned child process a termination tracker needs — keeps the helper usable
/// from a real OS process and from test fakes alike.
pub trait TrackableChild {
    /// A snapshot check for whether the process has exited — analogous to Node's combined
    /// `exitCode`/`signalCode` read. Real backends implement this over
    /// `std::process::Child::try_wait`, which stays `true` once the child is reaped, so this is
    /// safe to call repeatedly after exit.
    fn has_exited(&mut self) -> bool;
}

/// Returns a predicate that answers "has this child actually terminated?".
///
/// A plain "was a signal delivered" flag answers a different question than "did the process
/// actually die" — every agent CLI installs its own SIGTERM handler, so a SIGTERM->SIGKILL
/// watchdog gated on delivery alone would disable its own escalation for exactly the child it
/// exists for (#844, the same defect fixed for the discovery probe in #841). This tracker only
/// ever reports `has_exited`.
///
/// Seeded eagerly so a child that has already exited before the watchdog is armed is recognized
/// on the first call, without waiting for a later poll. Node's original re-checks lazily via a
/// one-shot `'exit'` listener; this port re-checks lazily via re-polling instead, since
/// `std::process::Child` has no push-based exit notification — the observable contract (once
/// true, always true; true as soon as the child has actually exited) is the same.
pub fn track_child_exit<C: TrackableChild>(mut child: C) -> impl FnMut() -> bool {
    let mut exited = child.has_exited();
    move || {
        if !exited {
            exited = child.has_exited();
        }
        exited
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_the_128_plus_signal_codes_a_signalled_cli_reports() {
        assert!(is_signal_termination_exit(Some(130))); // SIGINT
        assert!(is_signal_termination_exit(Some(137))); // SIGKILL
        assert!(is_signal_termination_exit(Some(143))); // SIGTERM
    }

    #[test]
    fn leaves_genuine_failures_and_clean_exits_alone() {
        for code in [Some(0), Some(1), Some(2), Some(127), None] {
            assert!(!is_signal_termination_exit(code));
        }
    }

    #[test]
    fn prepend_system_prompt_joins_with_the_documented_separator() {
        assert_eq!(
            prepend_system_prompt(Some("Extra rules."), "do it"),
            "Extra rules.\n\n---\n\ndo it"
        );
    }

    #[test]
    fn prepend_system_prompt_passes_the_user_prompt_through_untouched_when_absent() {
        assert_eq!(prepend_system_prompt(None, "do it"), "do it");
    }

    #[test]
    fn content_block_wire_shape_matches_the_anthropic_format() {
        let text = ContentBlock::Text {
            text: "hi".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(&text).unwrap(),
            serde_json::json!({ "type": "text", "text": "hi" })
        );

        let image = ContentBlock::Image {
            source: ImageSource {
                kind: "base64".to_owned(),
                media_type: "image/png".to_owned(),
                data: "AAAA".to_owned(),
            },
        };
        assert_eq!(
            serde_json::to_value(&image).unwrap(),
            serde_json::json!({
                "type": "image",
                "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" }
            })
        );
    }

    struct ScriptedChild {
        remaining_false: u32,
        polls: std::rc::Rc<std::cell::Cell<u32>>,
    }

    impl TrackableChild for ScriptedChild {
        fn has_exited(&mut self) -> bool {
            self.polls.set(self.polls.get() + 1);
            if self.remaining_false == 0 {
                return true;
            }
            self.remaining_false -= 1;
            false
        }
    }

    #[test]
    fn recognizes_a_child_that_already_exited_before_the_tracker_was_armed() {
        let mut has_exited = track_child_exit(ScriptedChild {
            remaining_false: 0,
            polls: Default::default(),
        });
        assert!(has_exited());
        assert!(has_exited());
    }

    #[test]
    fn recognizes_a_child_that_exits_on_a_later_poll() {
        // `track_child_exit` seeds with one eager poll, so three `false` results are needed
        // to cover the seed plus the two `false` calls below before the third call sees exit.
        let mut has_exited = track_child_exit(ScriptedChild {
            remaining_false: 3,
            polls: Default::default(),
        });
        assert!(!has_exited());
        assert!(!has_exited());
        assert!(has_exited());
    }

    #[test]
    fn stops_polling_once_the_child_has_been_recognized_as_exited() {
        let polls = std::rc::Rc::new(std::cell::Cell::new(0));
        let child = ScriptedChild {
            remaining_false: 2,
            polls: polls.clone(),
        };
        let mut has_exited = track_child_exit(child);
        assert!(!has_exited());
        assert!(has_exited());
        assert_eq!(polls.get(), 3);
        // A further poll must not touch the child again — the flag latches.
        assert!(has_exited());
        assert_eq!(polls.get(), 3);
    }
}
