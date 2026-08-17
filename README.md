<div align="center">

# Coducktor

**A local terminal cockpit for coding-agent workflows.**

Coducktor gives you one place to start coding tasks, watch agent work as it
happens, review the resulting changes, and keep a durable history of every run.

[What it is](#what-it-is) | [Install](#install) | [Use the cockpit](#use-the-cockpit) |
[CLI](#headless-cli) | [Workflows and skills](#workflows-and-skills) |
[Development](#development)

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Rust 1.97.1](https://img.shields.io/badge/Rust-1.97.1-orange)
![Local first](https://img.shields.io/badge/local--first-yes-success)
![No database](https://img.shields.io/badge/database-none-success)

</div>

---

## What it is

Coducktor is a single Rust binary with two names: `coducktor` and the shorter
`duck`. Its main interface is an interactive terminal UI backed by an in-process
engine. It is not a browser application, hosted service, or background server.
Coducktor itself does not require Node.js or npm.

The cockpit can:

- Start a plain task, apply a Markdown skill, or run a saved YAML workflow.
- Run Claude Code, Codex, OpenCode, or pi sessions through one normalized runner interface.
- Select a model, reasoning effort, base branch, agent account, and autonomous mode.
- Run up to three competing Git variants and compare their diffs before choosing one.
- Use an isolated Git worktree by default in Git repositories, or run directly in the checkout.
- Stream agent text, tool activity, plans, questions, images, token counts, costs, and step status.
- Persist run history and event transcripts so completed work remains inspectable after restart.
- Continue, cancel, finish, archive, delete, or hand a completed session back to its native CLI.
- Inspect task changes, files, and commits, then commit and push from the task Git view.
- Browse and edit project files in the built-in IDE view.
- Read GitHub issues and pull requests, hand them to an agent, inspect PR changes, create draft PRs,
  and merge PRs when the local GitHub CLI is available.
- Manage multiple local project folders from one workspace registry.

Coducktor itself needs no account, database, web server, or project-specific service. Agent
authentication remains owned by the agent CLIs, and GitHub access is optional.

## Install

Coducktor is source-first. There is no hosted installer or release archive.

### Requirements

- [Rustup](https://rustup.rs). The repository pins Rust `1.97.1` in
  [`rust-toolchain.toml`](rust-toolchain.toml).
- Git for worktrees and the Git views. Non-Git folders can still run tasks in place.
- At least one logged-in agent CLI:
  [Claude Code](https://claude.com/claude-code),
  [Codex](https://github.com/openai/codex),
  [OpenCode](https://opencode.ai), or
  [pi](https://github.com/badlogic/pi-mono).
- The [GitHub CLI](https://cli.github.com/) with `gh auth login`, or a GitHub token, only if you want
  the GitHub view or GitHub actions.

### Install from a checkout

```bash
git clone https://github.com/blakepiper/coducktor.git
cd coducktor
./install.sh
```

The installer builds the release binary and installs both `coducktor` and `duck` with
`cargo install`. Ensure `$CARGO_HOME/bin` is on your `PATH` if the installer reports that it is
not.

Run the cockpit from the project you want to work on:

```bash
cd /path/to/your/project
duck
```

The long name works everywhere too:

```bash
coducktor
```

To run directly from the source checkout without installing:

```bash
cargo run -p coducktor-tui
```

To update a source installation, pull the checkout and run the installer again:

```bash
git pull && ./install.sh
```

## Use the cockpit

### Start a task

1. Launch `duck` from a project folder. If you launch it from a subdirectory of a Git repository,
   Coducktor resolves the repository root.
2. Press `c` to open **New task**.
3. Enter what the agent should do.
4. Choose a baseline task, skill, or workflow. Then choose an available runner, model, reasoning
   effort, base branch, and agent account as needed.
5. Leave autonomous mode enabled to let the run continue through its workflow without waiting for
   confirmation, or toggle it off when you want a more interactive handoff.
6. Choose **Start**, or choose **Plan first** and accept the plan preview.

In a Git repository, the default run mode for normal tasks uses a task worktree under
`.ai/coducktor/worktrees/` on a `duck/<run-id-prefix>` branch. The original checkout is left alone while
the agent works. A configured or skill-specific in-place mode can opt out. The New Task screen shows
whether a run will use a worktree or modify the checkout directly. Without Git, the run executes in
place.

### Follow a run

Open a task from **Tasks** to see its thread. The thread shows the live normalized transcript,
workflow steps, tool calls, agent questions, plan progress, diff statistics, token usage, and cost
when the selected backend reports them.

When a run stops, the available actions depend on its state. You can answer questions, continue a
closed session, finish a waiting or review run, cancel active work, open the native agent CLI, or
archive and delete terminal runs.

Use the task Git tabs to inspect **Changes**, **Files**, and **Commits**. The **Compare** view appears
for multi-variant runs; selecting one variant archives the others and removes their worktrees.

### Navigate

The global keymap is intentionally small:

| Key | Action |
|---|---|
| `q` | Quit |
| `c` | New task |
| `t` | Project tasks |
| `g` | All tasks across registered projects |
| `Ctrl+K` | Command palette |
| `?` | Contextual help |
| `:` | Command line |
| `Ctrl+O` / `Ctrl+I` | Back / forward |

Use mouse navigation where supported. The complete screen-by-screen reference is in
[`docs/tui/keymap.md`](docs/tui/keymap.md), and terminal capability notes are in
[`docs/tui/terminals.md`](docs/tui/terminals.md).

### Projects

The current folder is the boot project. Register additional folders in the project switcher or
from the command line:

```bash
duck projects add /path/to/another/project
duck projects list
duck --repo /path/to/another/project
```

`--repo` selects an already registered project. Removing a project only removes its registry entry;
it does not delete the folder or its `.ai/coducktor/` data.

## Headless CLI

The non-interactive commands never open the TUI, alternate screen, browser, or Coducktor listener.

```text
duck                         Launch the interactive cockpit
duck tui                     Same as the default invocation
duck run "<task>"            Run a task and stream events to the terminal
duck init                    Scaffold an example workflow and skill
duck doctor [--json]         Check installation and available CLIs
duck projects [list]         List registered projects
duck projects add [DIR]      Register a folder
duck projects remove ID      Remove a registry entry
duck projects rm ID          Alias for remove
duck usage [OPTIONS]         Quota telemetry compatibility command
```

The global launch options are:

```text
--repo DIR                   Select a registered project
--workflow NAME              Preselect a workflow on New Task, or select it for run
--model MODEL                Preselect a model on New Task, or select it for run
```

Examples:

```bash
# Uses the built-in quick-task workflow and the default Claude runner.
duck run "Fix the flaky login test"

# Use a saved workflow and model.
duck run "Implement the cache invalidation change" \
  --workflow implement-and-verify \
  --model sonnet

# Run against a registered project without opening the cockpit.
duck --repo /path/to/project run "Update the changelog"

# Print a machine-readable installation report.
duck doctor --json
```

`run` exits `0` for a completed or review-ready run and `1` for other terminal states or an
execution error. The headless default runner is Claude; a workflow step can request another runner.
`init` is idempotent: it creates
`.ai/coducktor/workflows/fix-and-verify.yaml`,
`.ai/coducktor/skills/project-conventions.md`, and runtime Git ignore entries without overwriting
existing example files.

The `usage` command is retained for command compatibility, but provider quota telemetry is not
available in this build. It reports that limitation and exits nonzero. Per-run token and cost
fields are still shown when a backend supplies them.

## Workflows and skills

The built-in `quick-task` workflow is always available. Custom workflows are loaded from
`.ai/coducktor/workflows/*.yaml` and `.yml`. A workflow may contain agent steps, shell check steps,
per-step skills, runner or model overrides, and bounded retry links to earlier steps.

```yaml
name: implement-and-verify
steps:
  - id: implement
    name: Implement
    prompt: "{{task}}"
    runner: claude
  - id: verify
    name: Verify
    command: "cargo test --workspace --all-targets"
    onFail:
      retry: implement
      max: 2
```

Each step is either an agent step (`prompt` or `skill`) or a check step (`command`). The
`onFail.retry` target must name an earlier step. The Workflows screen can create, reorder, save,
delete, import, and export workflow YAML without editing it by hand.

A skill is a Markdown playbook with optional frontmatter:

```markdown
---
name: project-conventions
description: Apply this repository's style and testing rules.
---

# Project conventions

Describe the conventions the agent should follow here.
```

Project skills are commonly kept in `.ai/coducktor/skills/` or `.ai/skills/`. Coducktor also reads
the supported agent skill directories in the project and the user's global skill directories. The
Skills screen lets you inspect the merged catalog and the New Task screen lets you select a skill.

## Agent backends

Coducktor keeps backend-specific wire formats inside the runner layer while exposing the same task
and thread behavior in the cockpit.

| Runner | Executable | Session transport |
|---|---|---|
| Claude Code | `claude` | Headless stream JSON |
| Codex | `codex` | `app-server` JSON-RPC |
| OpenCode | `opencode` | Local `serve` process with HTTP and SSE |
| pi | `pi` | Persistent JSONL RPC |

Select a runner in New Task or set a default in Settings. The agent CLIs own provider login and
credentials. Coducktor does not load a `.env` file automatically; shell environment overrides are
documented in [`.env.example`](.env.example). Useful overrides include:

```bash
DUCK_CLAUDE_BIN=/path/to/claude
DUCK_CODEX_BIN=/path/to/codex
DUCK_OPENCODE_BIN=/path/to/opencode
DUCK_PI_BIN=/path/to/pi
DUCK_HOME=/path/to/coducktor-state
```

`DUCK_APPROVAL_GATE=1` opts Claude back into its interactive edit approval mode. By default,
headless Claude runs deny tools that are not allowed by the workflow. `DUCK_CODEX_NETWORK=0`
selects Codex's network-blocked workspace-write sandbox instead of its default full-access mode.

## Git and GitHub

Git is optional for basic in-place tasks, but it enables the safest and richest workflow:

- Per-task worktrees and `duck/` task branches.
- Task diffs anchored to the configured base branch.
- Task-local file browsing, commits, pushes, and commit diffs.
- Repository changes, commit history, and branch creation in the Git screen.
- Worktree retention and cleanup from Settings.

The GitHub screen is an optional local integration. It resolves the repository's `origin` remote and
uses the `gh` CLI for issues, pull requests, comments, checks, PR changes, draft PR creation, and
manual merge actions. If `gh`, authentication, the remote, or the network is unavailable, the rest
of Coducktor continues to work and the GitHub screen explains why it is unavailable.

The review gate is optional and off by default. Enable it in Settings or set
`DUCK_REVIEW_GATE=1` to park changed, non-autonomous runs at `review` so you can inspect the diff,
send feedback, accept the changes, or create a draft PR. Coducktor never merges a task
automatically.

## Local state and optional capabilities

Per-project data lives under `.ai/coducktor/`, including configuration, run records, normalized event
history, UI state, workflows, skills, and managed worktrees. Runtime files are ignored
by the `.ai/coducktor/.gitignore` created by `duck init`; workflows and skills can be committed to
share them with a project.

Workspace data lives under `~/.coducktor/`:

- `config.json` contains the project registry, workspace defaults, resource limits, and agent defaults.
- `ui-state.json` contains workspace UI preferences.
- `agent-accounts.json` contains additional Claude or Codex account directories.

Set `DUCK_HOME` to move the workspace state directory. Missing, corrupt, or unwritable state is
treated as a degraded capability rather than a reason to prevent startup.

The following features are opt-in or limited in this build:

- Intelligent context refresh is disabled by default and can be enabled in Settings -> Resources.
- Provider quota dashboards and the `usage` command are not implemented. Run-local usage remains
  available when the backend reports it.
- Coducktor itself opens no listening socket and no browser. OpenCode may start its own short-lived
  local `serve` process when that runner is selected.

## Development

The workspace is Rust-only and lives under `crates/`. Common commands:

```bash
just build       # release build of coducktor and duck
just test        # cargo test --workspace --all-targets
just lint        # fmt check plus clippy with -D warnings
just fmt         # format the workspace
just snapshots   # review insta TUI snapshots
cargo run -p coducktor-tui
```

The final repository checks are:

```bash
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo tree --workspace
```

See [`docs/tui/screenshots.md`](docs/tui/screenshots.md) for deterministic text renders from the
TUI snapshot tests.

## License

MIT. See [LICENSE](LICENSE).
