<div align="center">

# Coducktor 🦆

**A personal cockpit for autonomous coding agents.**

Coducktor turns a task into a visible, reviewable workflow. Run Claude Code,
Codex, OpenCode, or pi against isolated worktrees, watch every step live, and
let the queue keep moving while you are away.

Everything runs locally. Your agent logins, files, Git history, and credentials
stay on your machine.

[Features](#features) · [Quick start](#quick-start) · [How it works](#how-it-works) ·
[Using the cockpit](#using-the-cockpit) · [Backends](#agent-backends) ·
[Development](#development)

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Rust stable](https://img.shields.io/badge/Rust-stable-orange)
![Node 20+](https://img.shields.io/badge/Node-20%2B-339933)
![Zero config](https://img.shields.io/badge/config-zero-success)
![No database](https://img.shields.io/badge/database-none-success)

</div>

---

Coducktor's cockpit is a **terminal UI** — `duck` (or its long name, `coducktor`)
is a single binary you run in a terminal, not a browser tab. It's a Rust
rewrite in progress; see [`docs/tui/`](docs/tui/) for the keymap reference and
terminal support matrix.

## Features

- **Autonomous by default.** Tasks keep working through follow-ups, checks, and
  context refreshes instead of stopping after every increment.
- **Intelligent context refresh.** Long workflows can refresh their context
  window and continue in a fresh session without making the user manage the
  handoff.
- **Per-model reasoning controls.** Choose Auto, Low, Medium, High, or Max for
  each selected model. Auto chooses a level for each chunk of work.
- **Multiple agent backends.** Mix Claude Code, Codex, OpenCode, and pi across
  tasks or workflow steps.
- **Skills are optional.** Pick a Markdown skill when you want a specialized
  playbook, or leave the skill selector blank for a plain baseline task.
- **Live cockpit.** Follow agent text, tool calls, screenshots, token usage,
  costs, diffs, and step status as they happen.
- **Parallel worktrees.** Run several tasks at once without agents fighting
  over the same checkout.
- **Usage visibility.** See available weekly and session limits, remaining
  percentages, and reset times from inside the app.
- **Review before merge.** Changed tasks end with a diff you can inspect,
  continue, or send as a draft pull request. Nothing auto-merges.
- **Local-first operation.** No hosted account, database, or project-specific
  configuration is required.

## Quick start

Coducktor is source-first: clone and build, no hosted installer, no release
tarballs. `git pull && ./install.sh` again is how you update.

### Prerequisites

- [Rust](https://rustup.rs) (stable) — `install.sh` checks for `rustup` and
  tells you the one command to get it if it's missing. The toolchain version
  itself is pinned by [`rust-toolchain.toml`](rust-toolchain.toml); rustup
  reads that automatically.
- Git for isolated worktrees
- At least one logged-in agent CLI:
  [Claude Code](https://github.com/anthropics/claude-code),
  [Codex](https://github.com/openai/codex),
  [OpenCode](https://opencode.ai), or
  [pi](https://github.com/badlogic/pi-mono)
- **Node.js 20 or newer — Phase A only.** The Rust TUI still runs the existing
  Node service underneath for now ([spec §7.7](.ai/specs/2026-08-15-rust-tui-refactor.md#77-one-terminal-the-supervised-child-process-is-silent));
  this requirement disappears once the port to Rust completes. `install.sh`
  checks for it in the same isolated step.

### Install and run

```bash
git clone https://github.com/blakepiper/coducktor.git
cd coducktor
./install.sh
duck
```

`install.sh` builds the release binary and installs it — as both `duck` and
its long name `coducktor` — via `cargo install`. Either name launches the
terminal cockpit. See [`docs/tui/keymap.md`](docs/tui/keymap.md) for
navigation and [`docs/tui/terminals.md`](docs/tui/terminals.md) for terminal
support.

Prefer to run it in place without installing? `cargo run -p coducktor-tui`
from the checkout does the same thing.

## How it works

```text
  task prompt
      │
      ▼
  workflow ───────► agent step ───────► checks
      │                 │                  │
      │                 ▼                  │
      │          fresh context             │
      │          when needed               │
      ▼                 │                  ▼
  local state ◄──── live events ─────── review gate
      │
      ▼
  terminal cockpit
```

Each task runs in its own worktree when Git is available. A workflow can contain
agent steps, shell checks, retries, and backend overrides. The event log is
persisted locally so the cockpit can replay a run after a refresh or restart.

The default task path is deliberately simple: write the task, optionally pick
a skill, and run it. More structured work can use a saved YAML workflow.

## Using the cockpit

### Start a task

1. Choose the project and agent backend.
2. Optionally choose a model and reasoning level.
3. Leave the skill selector empty for a baseline task, or choose a skill to
   apply a specialized playbook.
4. Submit the task and follow the live run.

The **Auto** reasoning level is the recommended default. It evaluates the
current task and workflow chunk independently, using lighter reasoning for
small verification work and deeper reasoning for complex, risky, or expansive
changes.

### Workflows and skills

Skills are Markdown instructions stored with the project or supplied by a
shared skills source. Workflows connect skills, agent prompts, and shell checks
into a repeatable chain:

```yaml
name: implement-and-verify
steps:
  - id: implement
    name: Implement
    prompt: "{{task}}"
    runner: codex
  - id: verify
    name: Verify
    command: "npm test"
    onFail: { retry: implement, max: 2 }
```

The workflow builder can create and reorder these steps without editing YAML by
hand. Built-in workflows remain available even if custom workflow files are
removed.

### Context refresh

Enable **Intelligently refresh context window** in Settings → Resources. When a
workflow increment needs more room, Coducktor starts a fresh session, carries
forward the relevant task and progress, and continues the same run. The handoff
is recorded in the run history rather than hidden as a new task.

### Usage limits

The usage panel reports the limits exposed by the connected providers, including
weekly and session percentages and reset times. When one provider reaches its
limit, automatic backend routing can continue through another available
backend when the task permits it.

## Agent backends

| Backend | Transport | Notes |
|---|---|---|
| **Claude Code** | Headless stream JSON | Tool permissions and native effort controls. |
| **Codex** | App-server JSON-RPC | Native model discovery and reasoning effort support. |
| **OpenCode** | Local HTTP + SSE server | Multi-provider model selection and model variants. |
| **pi** | Persistent JSONL RPC | Native model and thinking-level selection. |

Every backend implements the same runner seam, so workflow behavior, live
events, review, usage tracking, and context refresh work consistently across
providers.

## Local state

The app stores run history, event logs, workflow data, and temporary worktrees
locally. Runtime data is ignored by Git automatically; skills and workflow
definitions can be committed when you want them shared with a project.

No cloud service is required. Provider authentication remains owned by the
agent CLIs themselves.

## Development

### The Rust TUI (`crates/`)

```bash
just build       # release build
just test        # cargo test --workspace --all-targets
just lint         # fmt --check + clippy -D warnings — same gate as CI
just snapshots    # review pending insta UI-snapshot changes
cargo run -p coducktor-tui   # run in place without installing
```

No `just`? Every recipe is a one-line `cargo` command — see the
[`justfile`](justfile).

### The Node service (`packages/`) — Phase A only

The Rust TUI still drives the existing Node/TypeScript service underneath
(see [Prerequisites](#prerequisites)). Working on that side directly still
uses its own npm scripts:

```bash
npm run dev          # API server and Vite cockpit (browser, for comparison/debugging)
npm run dev:server   # API server only
npm run build        # production server and cockpit build
npm run typecheck    # all workspace typechecks
npm test             # Vitest suite
```

This whole section — and the package tree it points at — goes away once the
Rust port replaces the Node service.

The app is intended as a personal fork. Publishing, release automation, and
upstream contribution guidance are intentionally outside this README.

## License

MIT. See [LICENSE](LICENSE).
