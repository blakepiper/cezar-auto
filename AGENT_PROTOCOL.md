# AGENT_PROTOCOL.md — Coducktor agent protocol

Coducktor runs coding-agent CLIs behind one backend-agnostic Rust seam and persists a normalized
event stream. This document is the operational contract for a runner: its input, session
lifecycle, event mapping, teardown behavior and test obligations.

The implementation lives in `crates/coducktor-runners/src/`; the durable event shapes live in
`crates/coducktor-contract/src/events.rs` and the normalized UI vocabulary in
`crates/coducktor-protocol/src/ui_events.rs`. Golden fixtures live under `fixtures/` and are
checked by `crates/coducktor-runners/tests/golden.rs` and `tests/ui_parity.rs`.

## Runner seam

Every backend is selected through `SessionFactory` and implements the session contract used by
`coducktor-core::workflows::run`. Backend-specific command-line and wire details stay inside the
runner crate.

The supported backends are Claude, Codex, OpenCode and pi. The factory resolves their executable
overrides from the `DUCK_*_BIN` environment variables, while `DUCK_DRY_RUN=1` selects the bundled
offline fixtures where that backend's contract supports it. The child environment is assembled
by `agent_env`; it is an explicit least-privilege allowlist, not an accidental copy of the host
environment.

Each session accepts a backend-neutral request containing the prompt, optional system prompt,
working directory, model, reasoning preference, tool policy, timeout and session/resume data.
It returns turn-scoped reports and emits live v1 and v2 events through the engine's in-process
event bus. A runner must preserve the session id, stream partial content, surface tool activity,
report usage/cost when the backend provides it, and distinguish an agent failure from a teardown
the runner initiated.

## Event layers

The v1 flat stream remains readable for existing NDJSON recordings. Its event kinds are text,
tool call, tool result, image, token usage, cost, session, turn end, note, done and error. Do not
rename or remove an existing kind.

The v2 stream is item-oriented and is defined by the Rust protocol crate. It includes:

- session started, ended and error;
- turn started and completed;
- item started, delta, updated and completed for messages, reasoning and tools;
- complete plan replacement snapshots;
- cumulative usage updates and images;
- structured user questions and the reserved permission events.

Stable item ids are required. Child work is nested with `parent_item_id`. Token fields remain raw
provider values; weighting belongs to presentation. A backend may omit a capability only when its
wire cannot provide it, and the UI must degrade that capability rather than the whole backend.

## Teardown and timeouts

The shared child-process helper owns stdin closure, stdout line delivery, stderr collection and
best-effort termination escalation. Cooperative finish closes input and waits briefly; a stuck
child receives termination and then a hard kill. Cancellation and timeout are reported according
to the run lifecycle, not as a fabricated provider failure. Every process is dropped safely even
if a test or caller abandons a session.

The Rust session API is turn-scoped. Claude, Codex, OpenCode and pi each document the small
transport differences in their module docs; those differences must not leak into core lifecycle
or TUI code.

## Compatibility markers

New prompts and fixtures emit the current marker vocabulary. The parser retains a permanent
dual-read compatibility regex for the previous marker spelling because already-running agents
and old skills can still send it. The same rule applies to task branches: writers create the
current prefix and readers accept both generations. A marker is parsed from assembled agent text,
not tool output, and marker precedence remains completion, structured question, monitoring.

## Adding a backend

1. Add one runner module and one `SessionFactory` branch; do not add backend conditionals to
   screens or core.
2. Map the backend's stream into both event layers, including session, turn, tool, text, usage,
   failure and teardown behavior.
3. Add committed input/expected golden fixtures and a parity assertion for every capability the
   backend claims.
4. Add subprocess coverage for missing executables, first-turn streaming, follow-up turns,
   cancellation/finish and any structured question or child-thread behavior.
5. Run the workspace test, clippy and format gates and update this document if the seam changes.
