# AGENTS.md — working in this repository

Coducktor is a local terminal cockpit for coding-agent workflows. The shipped product is one
Rust binary (`coducktor`, with the short `duck` alias) and the workspace crates under `crates/`.
It owns durable JSON, NDJSON, Markdown and YAML state under `.ai/coducktor/` and
`~/.coducktor/`; there is no database, browser cockpit, npm workspace or service to start.

## Git workflow

This is a solo-maintainer repository. Work directly on `main`, commit the completed change, and
push it to `origin main`. Do not force-push, create feature branches, or batch completed numbered
steps from `.ai/specs/`.

For the Rust TUI refactor plan, one numbered step is one commit and one push. Mark its checkbox
only after its acceptance criterion and checks pass, then record the pushed commit hash. Keep
unrelated worktree changes intact.

## Zero configuration

The default binary discovers the current repository, local skills, workflows, Git, available
agent CLIs and the per-user registry. Missing GitHub CLI, agent CLI, credentials, network access,
or writable state degrades to the smaller capability; it must not prevent startup. Optional
environment overrides use the `DUCK_*` namespace and are documented in `.env.example`. The
binary never loads `.env` automatically.

State is written, never required. Per-repository state lives under `.ai/coducktor/`; per-user
workspace state lives under `~/.coducktor/`. Startup runs the ordered, additive, idempotent,
non-blocking workspace migrations before the engine is constructed. The rename migration moves
an old state directory when the new one is absent, prefers the new directory when both exist,
and reports the stray directory without deleting it.

## Architecture

| Area | Source of truth |
| --- | --- |
| CLI and startup migrations | `crates/coducktor-tui/src/main.rs`, `src/cli.rs`, `src/headless.rs`, `crates/coducktor-core/src/workspace/migrations.rs` |
| Engine seam and live events | `crates/coducktor-client/src/engine.rs`, `src/in_process.rs`, `src/events.rs` |
| Durable files, workflows and run lifecycle | `crates/coducktor-core/src/` |
| Contract and normalized events | `crates/coducktor-contract/src/`, `crates/coducktor-protocol/src/` |
| Agent backends | `AGENT_PROTOCOL.md`, `crates/coducktor-runners/src/` |
| Terminal UI | `crates/coducktor-tui/src/`, `docs/tui/` |
| Git and worktrees | `crates/coducktor-core/src/git/` |
| GitHub integration | `crates/coducktor-forge/src/`, client/TUI adapters |

Screens depend on the `Engine` trait, never on subprocess, filesystem or transport details. The
in-process engine is the default and the only production engine. Agent-specific wire types stop
at the runner seam. New request/response or persisted shapes belong in the contract crate and
must be serde-compatible with existing state.

The writer emits the current command, state directory, environment, marker and branch spellings.
Readers retain the two compatibility regexes for existing marker text and task branches; do not
remove those shims or widen them into a second writer vocabulary.

## Safety and quality rules

- Never use `unwrap()` or `expect()` in production paths except the documented startup boundary
  in `main.rs`; tests may use them.
- Preserve unknown JSON keys, per-entry salvage and atomic `0600` read-modify-write behavior.
  A corrupt file is left in place after one warning and the process boots with defaults.
- Shell commands use argument arrays and bounded input. Git helpers degrade where their API says
  they do; worktree creation is the deliberate loud exception.
- Agent child output is never inherited by the user's terminal. The final product has no service
  child, no listening socket and no browser startup path.
- Do not reintroduce deleted network, hosted-deployment, browser, release-publishing or remote
  skill surfaces. Clone-from-GitHub and the local GitHub read/PR surface are intentionally kept.
- While editing a file, remove stale dead code or comments only after a reference search proves
  it is unused. Keep behavior changes in a separately named plan/spec.

## Checks

Run the focused crate tests while iterating, then the final gate before committing:

```text
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo tree --workspace
```

Review affected `insta` snapshots rather than accepting them blindly. For terminal behavior,
record real manual results in `docs/tui/terminals.md`; headless output is not evidence for an
interactive terminal.

When the final refactor checklist is being completed, also verify the repository-wide rename
scan, the state-directory migration, the waiver entries in `CHANGELOG.md`, the compatibility
docs, the terminal matrix, the deleted-surface inventory, and the absence of an npm tree.
