# Rust TUI refactor — cezar as a terminal cockpit

Status: **DRAFT — awaiting owner approval**
Author: agent evaluation of `coducktor@de290b87`
Date: 2026-08-15

## TLDR

Replace the React browser cockpit with a Rust terminal cockpit (`ratatui` +
`crossterm`) that is fully usable by **mouse** (click, drag, scroll, exactly as the
web app works today) and by **keyboard** (modal, vim-literate, discoverable). Do it
in three phases: **(A)** ship the TUI against the existing, exhaustively-tested Node
`/api/v1` service; **(B)** port that service to Rust crate-by-crate behind the *same*
HTTP contract, using the existing TypeScript suite and golden fixtures as an
executable oracle; **(C)** collapse the two into one binary that talks to the engine
in-process and only listens on a port when it needs to.

Phase A is the whole feature set. Phase B is what makes the project "a Rust
project". Phase C is the payoff (`cez` is one static binary, no Node, no npm).
Each phase is independently shippable and independently valuable.

## Resolved decisions (owner, 2026-08-15)

All four open questions are answered. **This spec is executable as written.**

1. **Scope: all three phases.** Plan and build through the single-binary collapse.
   The practical consequence for Phase A is that the `Engine` trait seam (§12, C1)
   is introduced **now**, in `cezar-client` at step A2, not retrofitted later — the
   TUI's screens only ever speak that trait, and swapping the HTTP backend for an
   in-process one becomes a two-line change instead of a refactor.
2. **Dictation: dropped.** The composer's mic button and
   `components/composer/dictation.ts` are not ported. No command hook, no
   `whisper-rs`. Recorded as an accepted capability loss in §9.3.
3. **The React cockpit is deleted** at the end of Phase B, together with
   `packages/contract` and `packages/api-client` (its only other consumers). cezar
   becomes terminal-only. This waives most of `BACKWARD_COMPATIBILITY.md` §2's
   *page*-URL contracts and has three consequences that are specified rather than
   discovered — see §1.4a and §9.3. (Decision 5 below then goes further and removes
   the network surface entirely.)
4. **Distribution is source-first.** Users clone the GitHub repo and build/install
   locally. **No npm distribution, no `npx cezar-cli`, no prebuilt-binary shim.**
   `BACKWARD_COMPATIBILITY.md` §6 (npm package surface) is **waived by the owner** —
   the package name, the `bin` aliases as *npm* entries, the published `files` list,
   `engines.node`, and the `check-pack` build gate stop being contracts. The `cezar`
   and `cez` **command names** are kept, because they are what the docs, the skills
   and users' muscle memory say; they are just installed by `cargo install --path`
   rather than by npm.
5. **Single-machine, single-user, terminal-only.** cezar is a tool a developer runs
   in a terminal on their own work machine. **No remote access in any form**: no
   hosted mode, no VPS deployment, no serving anything to any browser. This is the
   largest simplification in the spec and it deletes whole subsystems rather than
   porting them — see §1.4a. Its end state is the important part: **Phase C ships with
   no HTTP server, no listening port, and no network surface at all.**

### The one call this spec makes on the owner's behalf

Decision 5 says "no remote access". The **bookmarklet** is not remote — it is a local
browser on the same machine opening `http://localhost:4321/new?…`. So it does not
*literally* fall to decision 5, and §11.3 previously specified keeping it.

**This revision drops it anyway.** The reasoning, so it can be reversed in one
sentence if wrong:

- It is the *only* thing that requires a listening port to exist after Phase B. Keeping
  it means keeping `cez serve`, the launch-key, the CORS-open health route, and the
  DNS-rebinding guards (`origin-guard`, `host-guard`) forever — a network surface
  maintained for one feature.
- Its function is **already replicated natively**: §8.9's GitHub screen lists issues
  and PRs and carries the same "Hand this to the agent" card the bookmarklet targets.
  The workflow survives; only its browser entry point goes.
- It is the last thing standing between this design and "no network surface at all",
  which is a materially simpler and safer product.

**To reverse:** restore §11.3 (a ~100-line server-rendered `/new` handler), keep the
launch-key and the loopback guards, and keep `cez serve` as a Phase C mode. Say so and
I will put it back.

---

## 1. Evaluation of the repo as it stands

### 1.1 What this is

`coducktor` (package name `cezar`, bins `cezar` / `cez` / `cezar-cli`) is a local
cockpit for autonomous coding agents. It runs Claude Code, Codex, OpenCode and `pi`
against isolated git worktrees, streams their normalized events into a live UI, and
gates every change behind a human review step. Everything is local: no database, no
hosted account, state is plain JSON/NDJSON/Markdown under `.ai/cezar/` and
`~/.cezar/`.

### 1.2 Scale and shape

A TypeScript npm monorepo, four workspaces:

| Workspace | Purpose | Non-test LOC | Test files |
|---|---|---|---|
| `packages/contract` | zod schemas for the whole HTTP API; types inferred from them | ~2k | — |
| `packages/api-client` | Node-free typed client + the agent event protocol mirror | ~1.5k | few |
| `packages/cezar` | the CLI + the Hono service: run engine, runners, git, skills, workflows, automations, forge | ~44k | ~180 |
| `packages/web` | the React 19 / Vite / Tailwind 4 SPA cockpit | ~43k | ~165 |

Totals: **~90k lines of non-test source, ~347 test files, ~83k lines of tests.**
The test-to-source ratio is close to 1:1 and the tests are behavioral, not
snapshot-only. This is the single most important fact for the refactor: **there is a
working oracle for almost everything.**

### 1.3 Architecture today

```
  cezar CLI (index.ts)
      │  serve | run | init | usage | projects | server-install/deploy/uninstall
      ▼
  Hono service (server/server.ts, 5.8k lines)
      │  every route under /api/v1, mirrored at /api/v1/p/:projectId
      │  SSE: /events, /runs/:id/events, /workspace/events
      │  WS:  /ws (demand-driven topic bus)
      ├── RunManager (workflows/run.ts, 4.2k lines) ── the orchestration engine
      │      ├── runner seam (core/agent-runner.ts) → claude | codex | opencode | pi
      │      ├── UI event mappers (core/*-ui-mapper.ts) → normalized v1 + v2 streams
      │      ├── git worktrees, autosave commits, diff base resolution
      │      ├── review gate, handoff/context refresh, auto-resume, quota routing
      │      └── workspace semaphore (cross-project concurrency)
      ├── RunStore (runs/store.ts) ── runs.json + runs/<id>.ndjson, atomic writes
      ├── workspace/ ── ~/.cezar registry, agent accounts, migrations, ui-state
      ├── server/forge/github.ts (2.5k lines) ── `gh` CLI driver
      ├── automations/ ── GitHub event poller → task launcher
      └── skills / workflows / agent-config / server-install
      ▲
      │  HTTP + SSE + WS
  React SPA (packages/web) ── 14 routed surfaces, ⌘K palette, shared composer
```

### 1.4 Contract surfaces that are formally protected

`BACKWARD_COMPATIBILITY.md` (1.4k lines) enumerates nine protected surfaces. Any
refactor is bound by all of them. The load-bearing ones for this work:

1. **CLI** — bins `cezar`/`cez`, commands `serve`(default)/`run`/`init`/`usage`/`projects`,
   flags `-p/--port` (default 4321, auto-picks next free), `--repo`, `--workflow`,
   `--model`, `--no-open`, `-h/--help`; `run` exits 0 on `done` **and** `review`,
   1 otherwise. ~12 `CEZ_*` env vars.
2. **HTTP API** — ~120 routes under `/api/v1`, three-way scope aliasing
   (`/api/v1/x` ≡ `/api/v1/p/<boot>/x` ≡ `/api/v1/p/default/x`), SSE event
   vocabulary (`run`, `run-event`, `ui-event`, `run-deleted`, `todos`, `usage`,
   `ping`, `project-added`, `project-removed`, `checkout-progress`,
   `provider-status`), `seq` dedup, `/api/v1/health` is the only CORS-open route.
3. **`.ai/cezar/` state files** — `runs.json` (zod, additive-only, `.catch()`
   per-field salvage), append-only `runs/<id>.ndjson`, `handoff.md`, `ui-state.json`
   (`.passthrough()` — unknown keys must survive round-trips), `config.json`,
   `todos.json`, `launch-key`.
4. **Workflow YAML** — `.ai/cezar/workflows/*.yaml`, `steps` XOR `skills`,
   `{{task}}`, `onFail: {retry, max}`.
5. **Skills Markdown** — frontmatter, `SKILL.md`-in-a-directory, six discovery
   locations with fixed precedence.
6. **Agent event protocol** — v1 `AgentEvent` (11 flat types) + v2 `UiEvent`
   (13 dotted types, 3 item kinds, 4 enum vocabularies), emitted together, pinned by
   golden fixtures per backend and by a parity test that asserts *every* backend
   emits *every* capability.
7. **In-band markers** — `CEZ:DONE`, `CEZ:MONITORING`, `CEZ:PR=`, `CEZ:ISSUE=`,
   `CEZ:TITLE=`, `CEZ:ASK`.
8. **`~/.cezar/`** — `config.json`, `agent-accounts.json`, `ui-state.json`, ordered
   idempotent additive migrations keyed on `schemaVersion`.
9. **Bookmarklet** — `<origin>/p/<id>/new?skill=&auto=&key=&ref=` lives in users'
   browsers and cannot be reached to be updated. **Waived and removed** (decision 5 /
   "the one call"); its capability is replaced by the native GitHub screen, §8.9.

Surface **6 (npm package)** is waived — see "Resolved decisions" above. Everything
else stands. Note that waiving 6 also retires the machinery that enforced it:
`scripts/check-pack.mjs`, `scripts/sync-readme.mjs`, `scripts/inline-contract.mjs`,
`scripts/install-as-command.mjs`, `src/pack-check.ts` and `src/install-as-command.ts`
are all deleted at the end of Phase B rather than ported.

Surface **2 (HTTP API)** keeps every one of its *API* contracts through Phases A and
B — the `/api/v1` routes, the three-way scope aliasing, the SSE vocabulary, the `seq`
dedup — because a Rust TUI and a Node service still have to agree on them. What it
loses is everything that existed **for a browser**: the `GET /` SPA shell,
`/assets/:file`, `/open-mercato.svg`, the cockpit page URLs under `/p/:projectId/*`,
the legacy-flat redirect, the settings-section redirects, `/new?…`, the CORS-open
health route, and `GET /api/v1/launch-key`. At **Phase C the entire HTTP surface is
retired** — the contract's job was to be a boundary between two processes, and after
C2 there is only one.

Surface **1 (CLI)** loses `-p/--port` and `--no-open`, which mean nothing without a
server and a browser. Both are **waived**; `--repo`, `--workflow`, `--model`, `-h`
and the `run` exit-code semantics are unaffected and stay protected.

### 1.4a What decision 5 deletes rather than ports

This is the largest single reduction in the project and it should be taken as
license to delete, not to carefully port and then hide behind a flag:

| Subsystem | Size | Fate |
|---|---|---|
| `src/server-install/` + `platforms/{ubuntu-vps,macosx-ngrok}` + `engine.ts` + `steps.ts`, and the `server-install` / `server-deploy` / `server-uninstall` commands | ~1.4k src + ~0.9k tests | **Deleted.** Never ported. There is no VPS. |
| `docs/server-install/{README,ubuntu-vps,macosx-ngrok}.md` | 3 docs | **Deleted.** |
| `CEZ_REMOTE` hosted mode and every `capabilities.localHandoff` gate | threaded through server, capabilities, ~20 call sites | **Deleted.** Local is the only mode, so every `open-in-*`, agent-account editor and absolute-path disclosure is simply allowed. The `/health` `repoRoot` basename-vs-absolute split (#431) collapses to always-absolute. |
| `origin-guard.ts`, `host-guard.ts`, DNS-rebinding protection, loopback origin checks, the WS `trusted`/`loopbackReadable` topic split | ~0.7k src + ~0.7k tests | **Kept through Phase B** (a port is open, so the threat is real), **deleted at C2** with the listener. |
| `launch-key.ts`, `GET /api/v1/launch-key`, `.ai/cezar/launch-key` | small | **Deleted.** Its only consumer was the bookmarklet. |
| `web/src/lib/bookmarklet.ts`, Settings → Bookmarklets | ~0.2k | **Deleted.** Replaced by §8.9's native GitHub screen. |
| `static-ui.ts`, `/assets/:file`, `/open-mercato.svg`, the build-hint page | ~0.2k | **Deleted** with the SPA. |
| `--no-open` and the "open your browser" startup behavior | small | **Deleted.** The TUI *is* the UI; there is nothing to open. |

Rough total: **~3.5k lines of source and ~2.5k lines of tests that never need to be
written in Rust.** Together with decision 3 this removes more work than Phase A adds.

One thing decision 5 does **not** touch: the multi-project workspace. Registering
several repos and switching between them is a local, single-user feature and stays
exactly as it is.

### 1.5 What the browser cockpit actually does

Fourteen routed surfaces, all deep-linkable, all project-scoped under `/p/:projectId`
except global Tasks and global Settings:

`/` tasks overview · `/tasks` all-projects · `/new` composer · `/tasks/:id` thread ·
`/tasks/:id/{changes,files,commits,commits/:sha}` · `/compare/:groupId` ·
`/git{,/commits,/commits/:sha,/branches}` · `/ide` · `/github{,/prs,/issues/:n,/prs/:n,/prs/:n/changes}` ·
`/automations{,/new,/:id,/:id/log}` · `/skills` · `/inbox` · `/workflows{,/:name}` ·
`/settings/*` (12 sections across project and global scope).

Chrome: collapsible/resizable sidebar with per-project groups, each carrying its own
nav + grouped task quick-list (NEEDS YOU / WORKING / RECENT); a ⌘K command palette
(cross-project run finder, views, actions); a Tools menu; provider-status banner;
theme toggle; version/update chip; toasts; desktop notifications.

### 1.6 Honest assessment

**Strengths that make this refactor tractable:**

- The API contract is the boundary, is versioned, is documented to an unusual depth,
  and is enforced by `route-parity`, `contract-parity.*`, `versioned-surface` and
  `bc-route-inventory` tests. A second client is a *supported* thing to build.
- The agent event protocol is normalized and has hand-verified golden fixtures per
  backend. Porting mappers to Rust has a byte-exact target.
- `packages/api-client` is already Node-free and structurally isolated — it is a
  spec for what a Rust client must implement.
- State is plain files with documented schemas.

**Difficulties that must be planned around:**

- `server/server.ts` is 5.8k lines and `workflows/run.ts` is 4.2k lines of dense,
  stateful, well-commented logic (recovery, leases, semaphores, quota routing,
  auto-resume, context refresh, review gating, variant groups). This is the hard
  part of a Rust port and it is not mechanical.
- zod semantics that serde does not have natively: `.passthrough()` (unknown keys
  must round-trip), `.catch()` (per-field salvage — a bad field degrades, it does not
  fail the record), `.default()`, and `safeParse`-per-entry array salvage. Getting
  this wrong silently deletes users' data. It needs a deliberate, tested pattern.
- Three browser-only features have no clean terminal analogue: **dictation**
  (Web Speech API — dropped, decision 2), **image paste from the OS clipboard into a
  textarea** (solved with `arboard`), and the **bookmarklet's browser origin**
  (dropped — its capability lives in the native GitHub screen instead, §8.9).
- Inline **screenshot rendering** in the transcript is a real feature, not chrome.
  Terminals only do this via kitty/iTerm2/sixel graphics protocols.
- The web app is genuinely *dense* — foldable table columns, drag-reorder workflow
  steps, split diffs, a file-tree IDE editor. Each needs a terminal design, not a
  transliteration.

---

## 2. Goals

1. A Rust TUI that reaches **every** capability of the browser cockpit — every
   surface, every action, every live update.
2. **Full mouse parity.** Clicking a button, a row, a tab, a nav item, a file in a
   tree, a diff hunk header, a step in the rail must work. Scroll wheel scrolls.
   Drag resizes the sidebar and reorders workflow steps.
3. **First-class keyboard.** Discoverable defaults for newcomers (arrows, Tab, Enter,
   Esc, `?`), a full vim-literate layer (modal editing, `hjkl`, `gg`/`G`, `/`, `n`,
   `:` command line, `g`-prefixed go-to chords) for advanced users, and a remappable
   keymap file.
4. **No behavioral regressions** against any of the nine protected surfaces.
5. The project ends up **primarily Rust**, with Node retained only for as long as a
   phase needs it.
6. The work is executable by less-capable coding agents: numbered steps, each with a
   verifiable acceptance criterion, each leaving the tree green.

## 2.2 Renaming: `cezar` → `coducktor`

The project is `coducktor`; `cezar` is the name of the upstream it forked from and
appears **~4,400 times** across the tree. The refactor is the only cheap moment to
fix this — every one of these strings is being rewritten in Rust anyway — so the rule
is: **no new Rust file contains the string `cezar` or `cez`, in any casing, for any
purpose.** The TypeScript tree keeps its names until each module is ported.

### 2.2.1 The full rename surface

**`duck` is the short token** (owner, decision 6). One abbreviation used everywhere —
command, env prefix, marker prefix, branch prefix — rather than three competing ones
(`coducktor` / `cod` / `duck`). `coducktor` remains the long command and the project
name.

| Thing | Now | Becomes | Count |
|---|---|---|---|
| Commands | `cezar`, `cez`, `cezar-cli` | `coducktor`, `duck` | 3 → 2 |
| Env vars | `CEZ_*` | `DUCK_*` | **43 distinct → ~28 after §16a** |
| Agent markers | `CEZ:DONE`, `CEZ:MONITORING`, `CEZ:PR=`, `CEZ:ISSUE=`, `CEZ:TITLE=`, `CEZ:ASK` | `DUCK:*` | 6 |
| Per-repo state dir | `.ai/cezar/` | `.ai/coducktor/` | 292 refs |
| Per-user state dir | `~/.cezar/` | `~/.coducktor/` | 63 refs |
| Task branch prefix | `cez/<id8>` | `duck/<id8>` | 66 refs |
| Crates | `cezar-contract`, `cezar-tui`, … | `coducktor-contract`, `coducktor-tui`, … | 8 crates |
| Docs | `AGENTS.md`, `AGENT_PROTOCOL.md`, `README.md`, `SDLC.md`, `CHANGELOG.md`, `.ai/**` | prose rewrite | 152+ refs |

Directories use the full `coducktor` (they are read by humans browsing a repo and are
typed rarely); everything typed or emitted repeatedly uses `duck`. The skills cache
(`~/.cache/cez/skills/`) is not in the table because it is deleted outright — see
§16a Q7.

### 2.2.2 The three that need care

Everything above is mechanical except three, each of which will silently break things
if renamed naively:

1. **The agent markers are a live contract with running agents.** `CEZ:DONE` and
   friends are emitted by agent sessions whose instructions were composed when the
   session started, and by every skill file in `.ai/skills/` and `~/.agents/skills/`.
   **Required:** the marker parser accepts **both** spellings permanently (it is a
   regex alternation — the cost is one `|`), while everything cezar *emits* —
   `HANDOFF_INSTRUCTIONS`, the skill bodies, `AGENTS.md` — uses only `DUCK:`. A
   hard cut here breaks any in-flight run and every skill the user has not yet
   rewritten, for no benefit.
2. **Task branches already exist in git.** `cez/a4096b17` branches are real refs in
   the user's repos and cannot be renamed without rewriting history. **Required:** the
   branch-prefix *reader* matches `^(cez|duck)/`, the *writer* only ever emits
   `duck/`. Same one-alternation cost.
3. **State directories hold live data.** `.ai/coducktor/` and `~/.coducktor/` need a
   one-shot migration, not a flag day. **Required:** on startup, if the new path is
   absent and the old one exists, `rename()` it and log one line; if **both** exist,
   use the new one and warn about the stray. This slots in as workspace migration
   `002` and inherits that framework's contract (ordered, idempotent, additive,
   non-blocking — a failure logs and boots degraded, per §9 of the compat doc).

Env vars need no compatibility shim: they are set by the user in their own shell, and
a wrong one simply has no effect. Document the mapping in the CHANGELOG.

### 2.2.3 When

- **Phase A** — every new Rust crate is born `coducktor-*` with `DUCK_*` env vars. The
  TUI reads `.ai/cezar/` paths through a single `paths` module so the rename is one
  file later, not a sweep.
- **Step A15** — rename the *user-facing* surfaces in the Node tree while it is still
  the thing under test: the marker vocabulary (with the dual-read shim), the branch
  prefix (same), the state-dir migration, `AGENTS.md`/`AGENT_PROTOCOL.md`/`README.md`
  prose, and the skills in `.ai/skills/`. Internal TypeScript identifiers are **not**
  renamed — that code is being deleted.
- **Each Phase B step** — the ported module lands with `coducktor` naming; the
  TypeScript it replaces is deleted, so no dual-naming period exists per module.
- **Step B12 / C3** — a CI check (`rg -i 'cezar|\bcez\b'` over `crates/` and the docs)
  fails the build. Add it the moment the last TypeScript file is gone.

*Accept (final):* `rg -i "cezar|\bcez\b|CEZ_|CEZ:"` returns hits only in `CHANGELOG.md`
and `BACKWARD_COMPATIBILITY.md` history, and in the two compatibility regexes named
above (each with a comment pointing here).

---

## 3. Non-goals

- Redesigning the product. The TUI mirrors the current information architecture.
  Minor visual differences are expected and fine; missing capabilities are not.
- Improving the engine's behavior. Ports are behavior-preserving; behavior changes
  are separate work with their own specs.
- Supporting terminals older than "modern": we require 256-color minimum, target
  truecolor, and degrade gracefully. No `TERM=vt100` story.
- Windows-native (non-WSL) support as a launch requirement. Keep the code portable
  (`crossterm` is), but the supported matrix is macOS + Linux + WSL, matching where
  the agent CLIs actually run today.
- **Any form of remote, hosted, multi-user or browser access** (decision 5). cezar is
  a single-user tool on a single machine. No VPS deployment, no `--server <url>`, no
  auth tokens, no hosted mode, no browser. If a user wants cezar on another box they
  `ssh` to it and run `cez` there — that is a property of SSH, not a feature this
  project builds or tests.
- A network surface at all, past Phase B. Phase C listens on nothing. Treat any
  proposal to "just expose a small endpoint for X" as out of scope for this spec.

---

## 4. Strategy — three phases

### Why phased, and not a single rewrite

A single-shot rewrite of 90k lines of behavior-dense TypeScript with no intermediate
verifiable state is the failure mode this spec exists to avoid. The API contract
gives us a seam that splits the work in half, and each half has an oracle:

- The **TUI half** can be verified against a running, known-good Node service.
- The **engine half** can be verified against the existing test suite, the golden
  fixtures, and — decisively — the still-working React cockpit, which is 43k lines
  of independent client that will scream if a response shape drifts.

### Phase A — the Rust TUI over the existing service

`cez-tui` is a Rust binary that speaks `/api/v1` (HTTP + SSE + WS). It launches
`cezar serve` as a managed child process (or attaches to a running one), then owns
the terminal. Node is still there; the user never sees it.

**Deliverable:** feature-complete terminal cockpit (minus what decisions 2, 5, 7 and
8 removed — see §16a). This is a legitimate stopping
point — if Phase B is never funded, the product is still "a TUI you drive from your
terminal", which is the stated goal.

**Estimated size:** ~18–22k lines of Rust.

### Phase B — port the service to Rust behind the same contract

Replace `packages/cezar` module-by-module with Rust crates, exposing a byte-identical
`/api/v1` from an `axum` server. Order chosen so each step is independently testable
and the React cockpit keeps working the whole way through.

**Deliverable:** `cezar serve` is a Rust binary. Node is deleted.

**Estimated size:** ~35–45k lines of Rust.

### Phase C — collapse into one binary

The TUI links the engine crate directly and calls it in-process for local use; the
HTTP server, the `cez serve` command and the listening port are **deleted outright**
(decision 5) — there is nothing left to serve to. One binary, no network surface,
built from the checkout.

**Estimated size:** ~2–3k lines of Rust (mostly deleting the HTTP hop).

### Recommendation

Do A, then B, then C. Do **not** start B until A is shipped and used daily — A is
what proves the TUI's information architecture works, and B is much cheaper when the
only remaining consumer of a route is code you control.

---

## 5. Target repository layout

The repo becomes a Cargo workspace at the root, with the npm workspaces retained
until Phase B retires them.

```
Cargo.toml                     # [workspace] members = ["crates/*"]
rust-toolchain.toml            # pinned stable, edition 2024
crates/
  cezar-contract/              # A: serde mirror of packages/contract
    src/{runs,workflows,skills,github,workspace,ide,automations,events,health}.rs
  cezar-protocol/              # A: v1 AgentEvent + v2 UiEvent + tool-display
  cezar-client/                # A: HTTP + SSE + WS client over /api/v1
  cezar-tui/                   # A: the binary
    src/
      main.rs  app/  input/  theme/  widgets/  screens/  service/
  cezar-core/                  # B: run store, workflows, git, skills, config, workspace
  cezar-runners/               # B: claude | codex | opencode | pi + UI mappers
  cezar-forge/                 # B: gh driver
  cezar-server/                # B: axum service exposing /api/v1
  cezar-cli/                   # B/C: clap CLI, the `cez` bin
packages/                      # retired at the end of Phase B
```

**Rules for agents working in this tree:**

- `cezar-contract` and `cezar-protocol` are *derived* artifacts. They must never
  invent a field. Every type cites the TypeScript file and line it mirrors in a
  doc-comment, and a parity test proves it.
- `cezar-tui` may not contain business logic. If the TUI needs to know a rule
  (e.g. "which actions a `review` run permits"), that rule lives in a pure function
  in `cezar-contract` or `cezar-core`, mirroring where it lives in TS today
  (`web/src/routes/task-thread/run-actions.ts`, `web/src/lib/tasks-table.ts`, …).
- No `unwrap()`/`expect()` outside tests and `main.rs` startup.

---

## 6. Dependencies

Chosen to avoid reinventing wheels, with an explicit reason and a named fallback for
each. Versions are floors, not pins; the implementing agent pins actual versions in
`Cargo.lock` at Step A0.

### 6.1 Core TUI

| Crate | Why | Fallback |
|---|---|---|
| `ratatui` | The TUI framework. Immediate-mode, huge widget set, active. | — |
| `crossterm` | Backend. Mouse capture, bracketed paste, kitty keyboard protocol, focus events, resize. | `termion` (loses Windows) |
| `tokio` (rt-multi-thread, macros, process, sync, time, signal) | Async for HTTP/SSE/WS and child processes. | — |
| `tui-textarea` | Multi-line editor widget: the composer, prompt fields, the IDE editor. Mouse-aware, supports a vim-ish keymap. | hand-rolled |
| `edtui` | *Optional*, evaluated at Step A7: true modal vim editing for the IDE surface if `tui-textarea` proves too thin. | — |
| `tui-tree-widget` | File trees (IDE explorer, changes tree, files tab). | hand-rolled |
| `ratatui-image` | Inline images via kitty/iTerm2/sixel with halfblock fallback — agent screenshots. | placeholder box + "open externally" |
| `tui-markdown` | Markdown → `ratatui::Text`. Replaces Streamdown for agent text, skill bodies, issue/PR bodies. | `termimad` (own renderer, harder to embed) |
| `syntect` + `two-face` | Syntax highlighting for diffs and the IDE. `two-face` bundles `bat`'s syntax + theme assets. Replaces Shiki. | `tree-sitter-highlight` (faster, more setup) |
| `nucleo` | Fuzzy matching for the palette, skill picker, file mentions. Helix's matcher — fast and good ranking. | `fuzzy-matcher` |

### 6.2 Transport and data

| Crate | Why |
|---|---|
| `reqwest` (json, stream) | HTTP client. |
| `eventsource-stream` | SSE frame parsing over a `reqwest` byte stream, incl. `id:`/`retry:` for the cursor-resume contract. |
| `tokio-tungstenite` | The `/api/v1/ws` topic bus. |
| `serde`, `serde_json` | The contract types. |
| `serde_yaml_ng` | Workflow YAML (Phase B). Maintained fork of `serde_yaml`. |
| `time` or `chrono` | ISO-8601 timestamps; the wire format is RFC 3339 strings. Pick one, workspace-wide. |
| `url` | Parsing repo/PR URLs. |

### 6.3 Platform integration

| Crate | Why |
|---|---|
| `arboard` | Clipboard read/write, **including image read** — this is how "paste a screenshot" survives the port. |
| `open` | Open URLs and files in the OS default handler (`open-in-app`, PR links). |
| `notify-rust` | Desktop notifications (replaces the web Notification API). |
| `sysinfo` | Process-tree RSS/count telemetry (replaces `core/process-usage.ts`, Phase B). |
| `portable-pty` | Interactive terminal handoff ("take over interactively") and the E2E harness. |
| `clap` (derive) | CLI parsing — must reproduce the protected flag surface exactly. |
| `directories` | `~/.cezar` resolution honoring `CEZ_HOME`. |

### 6.4 Server (Phase B)

| Crate | Why |
|---|---|
| `axum` + `tower-http` | The HTTP service. Native SSE (`axum::response::sse`) and WS upgrade. Closest structural match to Hono. |
| `tokio-stream`, `async-stream` | Event fan-out. |
| `garde` or hand-written | Request validation. **See §11.2** — zod semantics need care; prefer explicit `Deserialize` impls over a validation crate. |

### 6.5 Testing and tooling

| Crate | Why |
|---|---|
| `insta` | Snapshot tests of rendered `TestBackend` buffers — **the primary UI verification mechanism**. |
| `ratatui::backend::TestBackend` | Deterministic offscreen rendering at a fixed size. |
| `wiremock` | Mock the `/api/v1` service for TUI unit tests. |
| `expectrl` + `portable-pty` | Drive the real binary in a pty for E2E. |
| `criterion` | Guard render-frame cost on large transcripts. |
| `proptest` | Fuzz the zod-compat deserializers (Phase B) — round-trip unknown keys, salvage bad fields. |

### 6.6 Prerequisite

**No Rust toolchain is installed on this machine.** Step A0 installs it and pins it
via `rust-toolchain.toml`.

Because distribution is source-first (see "Resolved decisions"), a Rust toolchain is
now a **user-facing prerequisite**, not just a contributor one. `rust-toolchain.toml`
therefore does double duty: it pins the contributor toolchain *and* it is what makes
a user's `rustup`-managed build reproducible. Keep the pinned version current with
stable, and keep the MSRV story simple — "install rustup, run `cargo install --path`".
Every dependency in §6 must build on stable with no nightly features; that is a
review rule, not a preference.

---

## 7. TUI design

### 7.1 Frame layout

```
┌────────────────────────────────────────────────────────────────────────────┐
│ cezar  coducktor / main                              [running 2] [⌘K]      │ header (1)
├──────────────┬─────────────────────────────────────────────────────────────┤
│ + New task  c│                                                             │
│  Tasks       │                                                             │
│  Inbox    5  │                  ROUTED SCREEN                              │
│  IDE         │                                                             │
│  Git         │                                                             │
│  GitHub      │                                                             │
│  Skills      │                                                             │
│  Workflows   │                                                             │
│  Settings    │                                                             │
│ ─────────────│                                                             │
│ Active  Arch │                                                             │
│ NEEDS YOU    │                                                             │
│  • task…  ×2 │                                                             │
│ WORKING      │                                                             │
│  • task…     │                                                             │
│ RECENT       │                                                             │
│  • task…     │                                                             │
├──────────────┴─────────────────────────────────────────────────────────────┤
│ NORMAL  coducktor  v0.9.2  ● claude ok  ● codex 62%      :  ? help         │ status (1)
└────────────────────────────────────────────────────────────────────────────┘
                                                          ↑ sidebar drag edge
```

- **Header** — brand, project/branch chip, live counters, palette hint. Clickable:
  project chip opens the project switcher.
- **Sidebar** — mirrors the web sidebar exactly: per-project collapsible groups (or
  the flat single-project nav), nav items with badges, Active/Archived toggle, and
  the grouped task quick-list. Width persisted, resizable by dragging the right
  edge, toggled with `Ctrl+B`. Auto-hides below 100 columns (the `md` breakpoint
  analogue) and becomes an overlay drawer.
- **Screen** — one routed surface, owning its own scroll and focus ring.
- **Status bar** — current mode, project, version (+ update dot), provider status
  dots, transient messages, command line when `:` is active.

### 7.2 Routing

Keep URLs as the internal identity — it is how the web app is organized, how deep
links work, and how the palette addresses things:

```rust
enum Route {
    Tasks { project: ProjectId },
    GlobalTasks,
    NewTask { project: ProjectId },
    Thread { project: ProjectId, run: RunId },
    TaskGit { project: ProjectId, run: RunId, tab: GitTab, sha: Option<String> },
    Compare { project: ProjectId, group: GroupId },
    RepoGit { project: ProjectId, tab: RepoGitTab, sha: Option<String> },
    Ide { project: ProjectId, path: Option<PathBuf> },
    Github { project: ProjectId, view: GithubView, number: Option<u64>, changes: bool },
    Automations { project: ProjectId, mode: AutomationsMode },
    Skills { project: ProjectId, selected: Option<String> },
    Inbox { project: ProjectId },
    Workflows { project: ProjectId, name: Option<String> },
    Settings { scope: SettingsScope, section: Option<SettingsSectionId> },
}
```

A history stack gives `Esc`/`Ctrl+O` = back and `Ctrl+I` = forward. `:open <url>`
accepts a literal cockpit path so pasted links from a teammate still work. The last
location is persisted per-machine, matching `readStoredLastLocation`.

### 7.3 Keyboard model

Three modes, shown in the status bar.

**NORMAL** — navigation and commands.

| Key | Action |
|---|---|
| `h j k l` / arrows | move focus within the focused pane |
| `Tab` / `S-Tab` | cycle focus between panes |
| `Enter` | activate focused element |
| `Esc` | close overlay → leave pane → navigate back |
| `gg` / `G` | top / bottom of list |
| `Ctrl+D` / `Ctrl+U` | half-page |
| `/` | search within screen; `n` / `N` next / prev |
| `:` | command line |
| `Ctrl+K` (and `Cmd+K` where the terminal delivers it) | command palette |
| `c` | new task (matches the web's bare-`c`) |
| `?` | keymap help overlay, context-aware |
| `g` then `t i d r h s w e a` | go to Tasks, Inbox, IDE, Git(repo), githubHub, Skills, Workflows, sEttings, Automations |
| `[` / `]` | previous / next tab within a screen |
| `1`–`9` | jump to nth sidebar task |
| `Ctrl+B` | toggle sidebar |
| `Ctrl+W` then `h/j/k/l` | move focus by direction (vim window semantics) |
| `q` | close screen / quit from Tasks (with confirm if runs are live) |

**INSERT** — text entry. Entered with `i`/`a` on a focused text field, or by clicking
into one, or automatically on the New Task screen (which is a hero composer).
`Esc` leaves. Inside INSERT the composer keeps the web's contract: `Enter` sends,
`Shift+Enter` newline, `Ctrl+Enter`/`Alt+Enter` also send, `/` opens the skills
autocomplete, `@` opens file mentions, `Alt+A`/`Alt+C` fire the quick replies.

**COMMAND** — `:`-prefixed line with completion. Every palette action is also a
command, so everything is scriptable and greppable:
`:new`, `:cancel`, `:continue`, `:finish`, `:archive`, `:pr`, `:commit`, `:push`,
`:open <route>`, `:project <id>`, `:theme dark|light|auto`, `:reload`, `:quit`.

**Bindings are data.** A default keymap ships as an embedded TOML file; a user
override at `~/.cezar/keymap.toml` is merged over it. The keymap is
mode → key-sequence → action-id. `?` renders the *effective* map, so the help is
never stale. This also gives non-vim users a documented path to remap everything.

### 7.4 Mouse model

Mouse must be as complete as the keyboard — this is an explicit product requirement.

- Enable `crossterm::event::EnableMouseCapture` plus SGR extended coordinates (so it
  works past column 223).
- **Hit-testing.** Each frame, widgets register clickable regions into a
  `HitMap { rects: Vec<(Rect, z: u8, ActionId, Option<Payload>)> }` while rendering.
  A click resolves to the highest-`z` rect containing the point. This is the one
  piece of TUI infrastructure worth building carefully: every screen depends on it.
  `HitMap` also drives **hover** (mouse-move → highlight the row under the cursor,
  matching the web's `:hover` states) and the tooltip layer.
- **Scroll.** Wheel events route to the scrollable region under the cursor, not to
  the focused one — matching browser behavior. `Shift`+wheel scrolls horizontally
  (wide diffs, wide tables).
- **Drag.** Sidebar edge resize; workflow step reorder (replaces dnd-kit); diff
  split-pane divider; scrollbar thumbs.
- **Double-click** opens (a task row → thread, a file row → preview). **Right-click**
  opens the context menu equivalent to the row's `⋯` menu in the web app.
- **Text selection escape hatch.** Mouse capture disables the terminal's own
  selection. Provide (a) `F12` to toggle capture off temporarily, with a status-bar
  hint, (b) explicit copy actions (`y` in NORMAL copies the focused item's canonical
  text — a command, a diff hunk, a PR URL, the whole transcript), and (c) honor the
  common terminal convention that holding `Shift` bypasses application capture.
  Document all three in `?`.

### 7.5 Theme

Port `docs/mockups/tokens.css` to a Rust palette with dark and light variants:
`bg`, `surface`, `border`, `fg`, `soft-fg`, `accent` (lime), `add` (green),
`del` (red), plus the status colors used by `StatusDot` (queued/running/waiting/
review/done/failed/cancelled). Detect terminal capability
(`COLORTERM=truecolor` → RGB; else 256-color quantization; else 16-color). Follow
the OS/terminal light-dark hint where available, honor the persisted
`appearance` setting from `~/.cezar/ui-state.json` (same store the web app uses, so
the preference is shared), and expose `:theme`.

### 7.6 The hard rendering problems

**Streaming markdown.** Agent text arrives as `item.delta` events. Re-parsing the
whole message per delta is O(n²) on long turns. Mitigation: keep per-item
`RenderCache { source_len: usize, rendered: Text<'static>, height_at_width: HashMap<u16, u16> }`,
re-render only on delta *and* only at ≤30 fps (coalesce deltas on a tick), and cap
re-render to the tail block when the source grows by an append.

**Transcript virtualization.** Threads reach thousands of items. Maintain a
`Vec<ItemHeight>` cache keyed by (item revision, width); render only the visible
window; invalidate the cache wholesale on resize. Mirror the web's
`thread-scroller.tsx` semantics: stick to bottom while at bottom, preserve anchor
when not, and support the progressive history loading contract
(`GET /runs/:id/history` reverse-paged).

**Images.** `ratatui-image` with the kitty graphics protocol (Ghostty, kitty,
WezTerm), iTerm2 protocol, or sixel; halfblock Unicode fallback everywhere else.
When no protocol is available, render a bordered placeholder with dimensions and an
`o` action that opens the file externally. Detect once at startup, report in `?`.

**Diffs.** Unified by default with a split toggle (`Ctrl+S`) that degrades to unified
below ~140 columns. Syntect-highlight per language, then overlay add/del background
tint. Collapse unchanged regions with an expandable "… N unchanged lines" row
(clickable, `Enter`-able). Word-level intra-line diff via `similar` for changed
lines. Must reproduce `web/src/components/diff/diff-view.tsx`'s behaviors:
per-file collapse, file-level stat, whitespace toggle, and the `?raw=` image path.

**Tables.** The tasks table has foldable columns
(spec `2026-07-30-foldable-task-table-columns`). In the TUI, columns have priorities
and drop out as width shrinks; `z` cycles density; a `Ctrl+click` on a header toggles
that column; sort by clicking a header or `s` then a column key.

---

## 8. Screen-by-screen specification

For each screen: the React source it replaces, the API it reads, the layout, and the
interaction contract. Implementing agents should read the named TSX file before
writing the Rust module — it is the behavioral spec.

### 8.1 Tasks overview — `screens/tasks.rs`
**Replaces** `routes/tasks-overview.tsx`, `lib/tasks-table.ts`, `lib/task-groups.ts`.
**Reads** `GET /runs`, SSE `run`/`run-deleted`/`usage`, `GET /github/ref-status`.
**Layout** Title row with `Active N | Archived` segmented control, "Archive finished"
action, search field. Then the table: STATUS · TASK · WORKFLOW · BRANCH · ± · PR ·
TOKENS · COST · CPU · MEM · STARTED. Below it, one card per unresolved variant group
("2 variants finished — Compare").
**Keyboard** `j/k` row, `Enter` open thread, `o` open changes, `a` archive, `d`
delete (confirm), `r` mark read/unread, `p` open PR, `/` filter, `s` sort, `t`
toggle Active/Archived, `Space` preview.
**Mouse** Click row → thread. Click status pill → filter by status. Click branch chip
→ copy. Click PR chip → open in browser. Click header → sort. Right-click → row menu.
**Live** Rows update in place from SSE; a new run animates in at the top; the ±
column, token and cost columns update per turn-end.

### 8.2 Global tasks — `screens/global_tasks.rs`
**Replaces** `routes/global-tasks.tsx`, `lib/global-tasks.ts`.
**Reads** `GET /workspace/runs-index`, SSE `/workspace/events`.
Same table plus a PROJECT column, project-tag facet filter, and grouping by tag.
Honors `perProjectLimit`/`truncated` — a capped list must say it is capped.

### 8.3 New task — `screens/new_task.rs`
**Replaces** `routes/new-task.tsx`, `routes/new-task-form.ts`, `components/composer/*`.
**Reads** `GET /skills`, `GET /workflows`, `GET /models`, `GET /workspace/agent-profiles`,
`GET /config`, `GET /workspace/config`. **Writes** `POST /runs`.
**Layout** Centered hero: "What should the agent work on?", the composer card
(auto-growing text area, attachment row — **no Dictation control**, per decision 2),
then a pill row —
`skill/workflow ▾` · `runner ▾` · `model ▾` · `reasoning ▾` · `×N variants ▾` ·
`base: <branch> ▾` · `agent account ▾` · `autonomous ☐` — then `Start` / `Plan first`
and the send button. Below: prompt-template suggestion chips.
**Pickers** open as centered overlay lists with `nucleo` fuzzy search, grouped
PROJECT SKILLS / GLOBAL / WORKFLOWS exactly as the web popover does, with usage-based
ranking (`lib/skills.ts` `bumpSkillUsage`).
**Draft persistence** per project, surviving navigation, as
`new-task-project.test.tsx` requires.
**Deep-link entry** `cez new --skill=… --ref=… --auto` keeps the old bookmarklet's
argument grammar as *CLI flags*, so the "start a task about this issue without
opening the UI first" workflow survives the bookmarklet's removal — now scriptable
and pipeable, which the browser version never was.

### 8.4 Task thread — `screens/thread/`
The largest screen. **Replaces** all of `routes/task-thread/` (≈20 modules).
**Reads** `GET /runs/:id`, `GET /runs/:id/history` (+`history-context`), SSE
`GET /runs/:id/events`. **Writes** `POST /runs/:id/{messages,cancel,continue,finish,pr,archive,read,unread,open-in-cli,open-in,git/commit,git/push}`,
`PATCH /runs/:id`, `PATCH|DELETE /runs/:id/queued-messages/:msgId`.

Sub-modules, one Rust module each:

- `header.rs` — title (inline-editable), status pill, workflow · branch · ± ·
  tokens · cost, tab row (Session | Changes | Files | Commits), action row
  (Finish, Continue, Terminal, Notes, Archive, Delete, Cancel), gated by
  `run-actions.ts` rules ported verbatim.
- `step_rail.rs` — the workflow step list with per-step status glyph, kind
  (agent/check) and "step N of M". Clickable → scroll transcript to that step.
- `transcript.rs` — the virtualized item list: user bubbles, agent markdown,
  reasoning blocks (collapsed by default, `Tab` to expand), tool cards (collapsed
  summary + expandable detail, per `core/tool-display.ts`), images, notes, and the
  session-boundary separators.
- `plan_dock.rs` — the `plan.updated` channel as a checklist strip.
- `subagent_dock.rs` / `agents_dock.rs` — nested agent sessions, openable as a
  sheet (a full-screen overlay in the TUI).
- `ask_card.rs` — the `ask.requested` interactive question card: option chips
  selectable by number/click, free-text fallback, multi-select support.
- `review_panel.rs` — the review gate: diff summary, per-file expandable diff, a
  notes field, and `Send back` / `PR` / `Accept`.
- `composer.rs` — the shared composer widget, second host.
- `queued_messages.rs` — the stacked-prompt list with edit/delete.
- `auto_resume_hint.rs` — the usage-limit countdown and its cancel action.

**Keyboard** `[`/`]` tabs, `i` compose, `Ctrl+C` cancel run (confirm), `y` copy
focused item, `Tab` expand/collapse focused card, `zR`/`zM` expand/collapse all,
`gg`/`G`, `Ctrl+E` open in `$EDITOR`, `Ctrl+T` terminal takeover.
**Mouse** Click a tool card to expand; click a step to jump; click a tab; click an
image to open; drag the scrollbar; click the PR chip to open.

### 8.5 Task git tabs — `screens/task_git/`
**Replaces** `routes/task-git/*`. Three tabs.
- **Changes** — a two-pane split: file tree/list on the left (with per-file ± and
  status), diff on the right. Toolbar: base-ref selector, whitespace toggle,
  split/unified, "Commit…" and "Push" actions with the commit dialog.
- **Files** — worktree file browser with preview, including image preview via the
  negotiated `?raw=` path.
- **Commits** — commit list; selecting one shows its diff (`GET /runs/:id/commit/:sha`,
  structured form).

### 8.6 Repo git — `screens/repo_git.rs`
**Replaces** `routes/repo-git/*`. Same three-pane pattern over `GET /repo`,
`/repo/changes`, `/repo/diff`, `/repo/commit/:sha`, plus a Branches tab with
`POST /repo/branch`.

### 8.7 Compare variants — `screens/compare.rs`
**Replaces** `routes/compare-variants.tsx`. Side-by-side (or stacked, when narrow)
columns per variant: progress excerpt, diff stat, full diff, and a `Pick` action
(`POST /groups/:groupId/pick`) that explains what happens to the losers.

### 8.8 IDE — `screens/ide/`
**Replaces** `routes/ide/ide.tsx`, `components/code-editor.tsx`.
**Reads/writes** `GET /ide/tree`, `GET|PUT /ide/file`.
Left: `tui-tree-widget` explorer. Right: editor with syntect highlighting, line
numbers, dirty indicator, `Ctrl+S` save, unsaved-changes guard on navigate. Honors
the server's 1 MB cap, `.git` exclusion and symlink exclusion. This is where `edtui`
gets evaluated for real modal editing; `Ctrl+E` "open in $EDITOR" is always
available as the escape hatch and should be prominent.

### 8.9 GitHub — `screens/github/`
**Replaces** `routes/github/github.tsx` (1.5k lines) + `hand-to-agent.tsx`.
**Reads** `GET /github`, `/github/checks?prs=`, `/github/ref-status`,
`/github/comments/:kind/:number`, `/github/prs/:number/{changes,merge-state}`.
**Writes** `POST /github/prs/:number/merge`, `POST /runs` (hand-to-agent).
Three panes: tab strip (Issues · N / Pull requests · N), list, detail. Detail shows
title, labels, body (markdown), the comment/event timeline, check rollup, and the
"Hand this to the agent" card with workflow/skills pickers and a `Run agent on this
issue` action. PR detail adds a Changes tab and a Merge action with its confirm
dialog. Everything degrades per the `{available, reason}` contract — never an error
screen when `gh` is simply absent.

### 8.10 Automations — **DELETED** (decision 7)

Not built. The screen, its four modes, the nav item and the whole `automations/`
subsystem are gone; see §16a.1. Left here as a numbered placeholder so §8's numbering
matches the route inventory a reader may compare against.

### 8.11 Skills — `screens/skills.rs`
**Replaces** `routes/skills.tsx` and `components/skill-detail.tsx`.
**Reads** `GET /skills` only.
A master/detail browser over locally discovered skills: the list on the left with a
source badge (project / global), the rendered skill body on the right, `/` to filter.
**No import panel, no update banner, no refresh action** — decision 7 deleted the two
network paths those served (§16a.1). Skills are files the user manages with their
editor; this screen reads them.

### 8.12 Inbox — `screens/inbox.rs`
**Replaces** `routes/inbox.tsx`. `GET /todos`, `DELETE /todos/:id`,
`POST /todos/:id/start`. Gated on `capabilities.followups`; degrades to an empty
state with the explainer when off.

### 8.13 Workflows — `screens/workflows.rs`
**Replaces** `routes/workflows/workflows.tsx`. Left: the saved-chain tab strip
(+ new). Center: the ordered step list, each row showing index, kind glyph, name and
prompt/command, with `x` to delete. Right: the skills palette with a filter field.
Reorder by `Alt+j`/`Alt+k` **and** by mouse drag (the dnd-kit replacement).
Import/Export/Save/Delete over `GET|POST /workflows`, `DELETE /workflows/:name`,
`POST /workflows/parse`. The exporter must keep emitting the portable compact
`skills:` form whenever `skillStackOf` says it can — that is a protected format
property.

### 8.14 Settings — `screens/settings/`
**Replaces** `routes/settings/*` (registry-driven). Keep the registry pattern: one
Rust table declares `{id, title, description, scope, hidden}` and the nav, index and
routes derive from it. Sections: **project** — Agents, Agent config, Worktrees,
Prompt templates; **global** — Accounts, Appearance, Notifications, Resources,
Projects. (Bookmarklets is deleted by decision 5; the Resources section loses only
its skills-auto-update toggle, decision 7 — §16a.1.) Writes go to the same routes
(`/config`, `/ui-state`, `/workspace/config`, `/workspace/ui-state`,
`/workspace/agent-profiles*`, `/agent-config/:id`), so the two clients stay
interoperable during Phases A–B.

The **Accounts** section loses its hosted-mode branch entirely: `editable` is always
true, every mutator is always allowed, and the "writing is a local-machine
capability" 409 path in `BACKWARD_COMPATIBILITY.md` §2 becomes dead code to delete
rather than logic to port.

### 8.15 Command palette — `overlay/palette.rs`
**Replaces** `components/command-palette.tsx`. `Ctrl+K`. Groups: Tasks
(cross-project, from `/workspace/runs-index`), Views, Projects, Actions. `nucleo`
scoring. Selecting a task navigates across projects. Every entry has a stable
action-id shared with the `:` command line and the keymap.

### 8.16 Overlays
`confirm.rs` (destructive actions), `toast.rs` (transient, matching the web's
toaster), `help.rs` (`?` — effective keymap, context-filtered), `provider_banner.rs`
(the auth/quota banner with its Connect action), `project_switcher.rs`,
`link_safety.rs` (the external-link confirm the web app shows).

---

## 9. Parity matrix and known gaps

### 9.1 Full parity, no caveats
All 14 surfaces, live SSE/WS updates, run lifecycle actions, review gate, variants,
git tabs and diffs, IDE editing, GitHub read + merge + hand-to-agent, automations,
skills, workflows, inbox, all settings sections, ⌘K palette, project switching,
multi-project sidebar, theming, desktop notifications, external open-in targets,
terminal takeover.

### 9.2 Parity by a different mechanism
| Web | TUI |
|---|---|
| Drag-and-drop workflow steps (dnd-kit) | Mouse drag **and** `Alt+j`/`Alt+k` |
| Image paste into the textarea | `arboard` clipboard-image read on `Ctrl+V`, plus `@path` attach and drag-drop of a path |
| Inline screenshots | kitty/iTerm2/sixel via `ratatui-image`; halfblock or placeholder+`open` fallback |
| Shiki highlighting | `syntect` + `two-face` (bat's assets) |
| Streamdown markdown | `tui-markdown` |
| Browser tab title / favicon badge | Terminal title escape (`OSC 0`) + `notify-rust` |
| Hover tooltips | Hover via `HitMap` + a status-bar hint line |

### 9.3 Accepted losses and relocations

1. **Dictation — DROPPED (owner decision 2).** `components/composer/dictation.ts` and
   the composer's mic button are not ported. The Web Speech API has no terminal
   equivalent and no substitute is being built. This is a real, accepted capability
   loss: record it in the CHANGELOG and remove the Dictation affordance from the
   composer spec, the mockups and the README so nothing promises it.
2. **The bookmarklet — DROPPED (see "The one call this spec makes").** The
   `javascript:` launcher that opened `/new?skill=&auto=&key=&ref=` from a GitHub page
   goes, along with the launch-key that authenticated it. Its capability — hand a
   GitHub issue or PR to an agent without retyping it — is **fully covered by §8.9's
   GitHub screen**, which lists issues and PRs and carries the "Hand this to the
   agent" card with workflow and skill pickers. What is genuinely lost is the *entry
   point*: starting from the browser you were already reading the issue in, rather
   than from the TUI. Record it in the CHANGELOG as a removal, not a relocation.
3. **Remote and hosted access — DROPPED (decision 5).** Not relocated, not replaced:
   removed. `CEZ_REMOTE`, `capabilities.localHandoff`, the `server-install` family and
   `docs/server-install/` are all deleted (§1.4a). Every local-machine affordance is
   unconditionally available, because there is no longer a deployment shape in which
   it would be wrong. A user who wants cezar on another machine SSHes to it.

---

## 10. Phase A — implementation plan

Every step ends with `cargo test`, `cargo clippy -- -D warnings` and `cargo fmt
--check` green, and leaves the tree runnable. Steps A3+ each add `insta` snapshots
for the screens they touch.

**A0 — Toolchain and workspace.**
Install Rust; add root `Cargo.toml` (workspace), `rust-toolchain.toml` (pinned
stable, edition 2024), `.cargo/config.toml`, `clippy.toml`, and CI wiring in
`.github/workflows`. Add `crates/cezar-tui` with a hello-world that opens and
restores the alternate screen cleanly (including on panic — install a panic hook
that restores the terminal, this is the #1 source of "my terminal is broken").
*Accept:* `cargo run -p cezar-tui` opens and `q` exits with the terminal intact.

**A1 — `cezar-contract`.**
Port `packages/contract/src/*.ts` to serde types. One Rust module per TS file, each
type doc-commented with its source path. Establish the **zod-compat conventions**
now (§11.2) even though Phase A only *reads*: `#[serde(default)]` for `.default()`,
`Option<T>` + `deserialize_with` salvage for `.catch()`, and a
`#[serde(flatten)] extra: serde_json::Map` for every `.passthrough()` object.
*Accept:* a test deserializes captured real responses from a live `cezar serve`
(fixtures committed under `crates/cezar-contract/tests/fixtures/`) for every route,
and re-serializes them to a byte-equal (key-order-insensitive) value.

**A2 — `cezar-protocol` + `cezar-client`, behind the `Engine` trait.**
Port `packages/api-client/src/protocol/{ui-events,tool-display}.ts`. Build the client:
typed methods per route, the `/api/v1/p/:projectId` scope prefixing from
`utils/project-scope.ts`, SSE with the `cursor`/`afterSeq` resume contract and `seq`
dedup, and the WS topic bus with subscribe/unsubscribe/reconnect-with-backoff.

**Because the owner approved all three phases, the `Engine` seam is introduced here,
not in Phase C.** Define it now and make it the *only* thing the TUI ever imports:

```rust
#[async_trait]
pub trait Engine: Send + Sync {
    async fn list_runs(&self, scope: &Scope) -> Result<Vec<RunRecord>>;
    async fn start_run(&self, scope: &Scope, input: StartRunInput) -> Result<RunRecord>;
    // … one method per route family …
    fn subscribe(&self, topic: Topic) -> BoxStream<'static, Event>;
}
```

`HttpEngine` is the only implementor in Phases A and B. `InProcessEngine` lands in
C2 and nothing above the trait changes. Two rules make this seam real rather than
decorative, and both are review gates: **(a)** no `reqwest`, `url` or HTTP status
code appears anywhere under `crates/cezar-tui/src/screens/`; **(b)** the trait's
error type is domain-shaped (`EngineError::{NotFound, Conflict{reason}, Unavailable
{reason}, Transport}`), never an HTTP error — a screen must not be able to tell
which backend it is talking to.

*Accept:* the existing golden fixtures under
`packages/cezar/src/core/__fixtures__/**` deserialize into `UiEvent` without loss; a
`wiremock`-backed test covers SSE resume and WS resubscribe-on-reconnect; a compile-time
test (`trybuild` or a grep-based lint in CI) enforces rule (a).

**A3 — App skeleton: event loop, router, theme, `HitMap`.**
The frame loop (input → update → render, with a 30 fps render budget and
input coalescing), the `Route` enum + history, the theme with capability detection,
the keymap loader, and the `HitMap` hit-testing/hover infrastructure. Plus the
`service` module that supervises the `cezar serve` child process (spawn, health-poll,
adopt an already-running instance, restart on crash, kill on exit).
*Accept:* two placeholder screens navigate by key, by `:open`, by mouse click, and by
history back/forward; snapshot tests at 80×24, 120×40 and 200×60.

**A4 — Shell chrome.**
Header, sidebar (project groups, nav, badges, Active/Archived, task quick-list with
its NEEDS YOU / WORKING / RECENT grouping from `lib/task-groups.ts`), status bar,
toast layer, help overlay, confirm dialog, sidebar resize + collapse.
*Accept:* nav by keyboard and by click; badges update live from the workspace SSE
stream; snapshots at three widths incl. the auto-collapse breakpoint.

**A5 — Tasks overview + global tasks.**
The table widget (foldable columns, sort, filter, hover, row menu), both screens,
SSE-driven live updates, archive/read/delete actions.
*Accept:* against `CEZ_DRY_RUN=1 cezar serve`, starting a run makes a row appear and
progress through statuses; the E2E pty test asserts it.

**A6 — Composer widget + New task.**
The shared composer (auto-grow, attachments, `/` skills, `@` files, quick replies,
submit shortcuts, draft persistence), the picker overlays, the New Task screen.
*Accept:* a task can be started end to end from the TUI and appears in the Tasks
table; the picker's grouping and fuzzy ranking match `lib/skills.ts`.

**A7 — Markdown, images, and the transcript.**
`tui-markdown` integration with the render cache, `ratatui-image` with protocol
detection and fallbacks, tool cards from `tool-display`, virtualized scrolling with
the sticky-bottom rule, progressive history loading.
*Accept:* a 5,000-item transcript scrolls at ≥30 fps (criterion bench); an image
event renders or falls back honestly; snapshots cover message/reasoning/tool/image
items in both collapsed and expanded states.

**A8 — Task thread, complete.**
Header + actions, step rail, plan dock, subagent sheet, ask card, review panel,
queued messages, auto-resume hint, composer host, cancel/continue/finish/PR.
*Accept:* the full run lifecycle — start → live → ask → answer → review → send back →
accept → archive — is driven entirely from the TUI in an E2E pty test.

**A9 — Diff engine + task git + repo git + compare.**
The diff widget (unified/split, syntect, intra-line, collapsed context, per-file
fold), file trees, commit lists, the commit dialog, branch actions, variant compare.
*Accept:* a worktree diff renders identically in content to
`GET /runs/:id/diff`; split mode degrades below 140 columns.

**A10 — IDE.**
Explorer, editor, save, dirty guard, `$EDITOR` handoff.
*Accept:* edit-and-save round-trips through `PUT /ide/file`; the 1 MB cap and
symlink/`.git` exclusions are respected and explained in the UI.

**A11 — GitHub, Skills, Inbox, Workflows.**
The four remaining content screens (Automations is deleted, decision 7), each with its
degradation path. Skills is the reduced reader described in §8.11 — do not build an
import panel or an update banner.
*Accept:* with `gh` absent, every GitHub surface shows the `{available:false,reason}`
explainer and no error; with `DUCK_FOLLOWUPS` unset, Inbox shows its opt-in explainer.

**A12 — Settings (all 12 sections) + palette + notifications + external open.**
*Accept:* every setting the web app can change is changeable in the TUI and the two
clients observe each other's writes.

**A13 — CLI surface.**
`clap` parser for the TUI binary reproducing the protected flags, plus `cez tui` /
bare-invocation-launches-TUI wiring. Do **not** change the Node CLI's contract yet.
*Accept:* `bc-route-inventory` and the CLI compatibility tests still pass unchanged.

**A14 — Install path and docs.**
Source-first install, per the owner's decision. Deliver:
- `cargo install --path crates/cezar-tui` as the documented one-liner, installing the
  `cezar` and `cez` command names (both declared as `[[bin]]` targets, or one bin plus
  an alias — either way `cez` must exist, the docs and skills reference it).
- An `install.sh` at the repo root that checks for `rustup`, prints the exact command
  to get it if absent, then runs the build and reports where the binary landed.
  No curl-pipe-to-shell hosting, no release artifacts, no auto-update check — cloning
  and building *is* the update mechanism.
- A `justfile` (or `Makefile`) with `build`, `install`, `test`, `lint`, `snapshots`
  so contributors and users share one entry point.
- README rewritten for the clone-and-build flow; `docs/tui/` with the keymap
  reference, the terminal support matrix (§13.8), and screenshots.
- **Phase A only:** the built TUI still needs the Node service, so the install docs
  must state the Node 20+ prerequisite honestly and `install.sh` must check for it.
  That check is deleted at Phase B cutover, and the README's prerequisite list should
  be written so removing it is a one-line diff.

*Accept:* on a clean machine, `git clone && ./install.sh` yields a working `cez` on
`PATH`, and the README's prerequisite list is complete enough that no step is a
surprise.

**A15 — Retire the npm and remote-access surfaces from the Node tree.**
Two owner decisions (4 and 5) condemn code that Phase A would otherwise keep
maintaining for months. Delete it here, in the *TypeScript* tree, while that tree is
still the thing under test — it is a pure-deletion step with a green suite on the
other side, and it shrinks everything Phase B has to port.

*npm (decision 4):* drop the `install-as-command` / `check:pack` scripts from the root
and `packages/cezar` manifests, delete `scripts/{check-pack,sync-readme,inline-contract,install-as-command}.mjs`
and `src/{pack-check,install-as-command}.ts` with their tests, and remove the
`prepublishOnly`/`check:pack` legs from the build chain.

*Remote access (decision 5):* delete `src/server-install/**` and the
`server-install` / `server-deploy` / `server-uninstall` commands, `docs/server-install/`,
`src/server/launch-key.ts` and `GET /api/v1/launch-key`, `web/src/lib/bookmarklet.ts`
and the Settings → Bookmarklets section, `--no-open` and the browser-launch startup
behavior, and every `CEZ_REMOTE` / `capabilities.localHandoff` branch — collapsing
each to its local-mode behavior rather than leaving a dead flag. **Keep**
`origin-guard` and `host-guard`: a port is still open through all of Phase A and B.

`packages/cezar` stays buildable and runnable from the checkout — Phase A depends on
it. This step removes publishing and remote deployment, not the service.

*Tier 1 + decisions 7–8 (§16a):* delete `src/release/**` and its three env vars,
`alias-cezar/`, the `latestVersion` update chip, `CEZ_API_BASE`/`CEZ_API_PORT`,
`docs/mockups/` (**after** porting `tokens.css` into the Rust theme, §7.5),
`packages/web/src/assets/fonts`, `link-safety-dialog.tsx`; then `skills-update.ts`,
`skills-remote.ts`, `automations/**`, `fs-browse.ts`, `wsl.ts`, the three
`CEZ_HIDE_*` flags, `CEZ_SINGLE_PROJECT`, `CEZ_BROWSE_ROOT` and every route, config
key, capability flag, nav item and UI section listed in §16a.1. **Keep**
`checkout.ts` and `projectsDir` — clone-from-GitHub survives.

*Rename (decision 6):* the user-facing surfaces only — the marker vocabulary with its
dual-read shim, the branch prefix with its dual-read shim, the state-dir migration
`002`, and the `AGENTS.md` / `AGENT_PROTOCOL.md` / `README.md` / `.ai/skills/` prose.
Internal TypeScript identifiers are **not** renamed; that code is being deleted.

*Accept:* `npm run build` succeeds with no pack-check leg; `npm test` is green with
the deleted subsystems' suites removed rather than skipped;
`rg -n "CEZ_REMOTE|localHandoff|launchKey|bookmarklet|server-install|skillsRepos|CEZ_HIDE|CEZ_SINGLE_PROJECT|browseRoot"`
returns only CHANGELOG and `BACKWARD_COMPATIBILITY.md` history; a run started before
the migration still loads, and its `cez/` branch is still found.

---

## 11. Phase B — porting the engine

### 11.1 Order (each step keeps the React cockpit working)

**B0 — Verify the ground is already clear.** The decision-4 and decision-5 deletions
happened back in **step A15**, in the TypeScript tree, so Phase B starts against a
codebase that no longer contains `server-install`, hosted mode, the launch key or the
bookmarklet. B0 is just the check: re-run A15's `rg` assertions and confirm nothing
crept back. If A15 was skipped or partially done, do it now — porting condemned code
is the single most wasteful thing this plan can do.

**B1** `cezar-core::paths`, `config`, `workspace::{config, ui_state, migrations,
agent_accounts}` — the file layer. Port the migration framework first; it is the
riskiest thing to get wrong and the easiest to test in isolation.
**B2** `cezar-core::runs::store` — `runs.json`, the NDJSON log, atomic writes,
`reconcileLoadedRun`, retention. Test against real files written by the Node version.
**B3** `cezar-core::git` — worktrees, base-ref resolution, autosave commits, diff,
shortstat, refs. **Shell out to `git`**, exactly as today; do not swap in `git2` or
`gix` in this step — the current behavior is subtle and the shell-outs are the spec.
**B4** `cezar-core::{skills, workflows::load, handoff, todos, task_markers, task_refs}`.
**B5** `cezar-protocol` mappers → `cezar-runners`: one runner at a time
(claude → codex → opencode → pi), each validated against its committed golden
fixtures **byte-for-byte**, and the `ui-parity` capability matrix re-implemented as a
Rust test. This is the step with the best oracle in the whole project; do it
carefully and it de-risks everything downstream.
**B6** `cezar-core::workflows::run` — the `RunManager`. Split the 4.2k-line file into
`lifecycle`, `session`, `recovery`, `review_gate`, `auto_resume`, `context_refresh`,
`variants`, `quota`, `semaphore` modules. Port `run.test.ts` (2k lines) alongside.
**B7** `cezar-forge` — the `gh` driver, ported against `github.test.ts` (2.3k lines).
**B8** `cezar-core::automations` — store, scheduler, poller, task templates.
**B9** `cezar-server` — `axum`, route by route, family by family. Run the existing
`route-parity`, `contract-parity.*`, `versioned-surface`, `bc-route-inventory`,
`origin-guard`, `host-guard` and `sse-headers` suites **against the Rust server** via
a thin harness (they are HTTP-level tests; point them at a different base URL). This
is the single highest-value verification move available and it should be set up in
B9's first commit, not its last.
**B10** `cezar-cli` — `serve`, `run`, `init`, `usage`, `projects`. Exit codes are
protected; `-p/--port` and `--no-open` are **not** ported (waived, §1.4). No
`--server`, no `--token` — there is no remote mode.
**B10a** *(deleted from the plan — decision 5.)* The `server-install` /
`server-deploy` / `server-uninstall` family is not ported; it was removed from the
Node tree back in step **A15**.
**B11** Cutover and soak. `cez serve` runs the Rust server. **The React bundle is
served from it unchanged for the whole soak period** — this is deliberate and is the
last and best parity check available: 43k lines of independent client exercising the
API, written by people who were not thinking about this port. Run both
implementations side by side on the same repo (a `--legacy-server` flag makes the
comparison one command) until the soak is clean. Only then does B12 run.
**B12** Delete the TypeScript. In this order, so each deletion is separately
revertable: `packages/web` → `packages/api-client` → `packages/contract` →
`packages/cezar` → the root npm workspace files (`package.json`, `package-lock.json`,
`vitest.config.ts`, `node_modules`), `scripts/dev.mjs`, and the `.github` workflows
that ran vitest. Also delete `src/server/static-ui.ts`'s SPA-shell behavior, the
`/assets/:file` and `/open-mercato.svg` routes, and `docs/mockups/` (a mockup of a
deleted UI is a trap for a future reader).
*Accept:* `rg -l "\.tsx?$"` returns nothing outside `docs/`; `cargo test` is the
whole suite; the README's build instructions are Rust-only.

### 11.2 The zod-compat problem — read this before writing any schema

Four zod behaviors that this codebase depends on for data safety, and their required
Rust pattern. Getting these wrong deletes user data silently.

| zod | Meaning | Rust pattern |
|---|---|---|
| `.passthrough()` | Unknown keys survive a read→write round trip (`ui-state.json`, `config.json`, workspace files, shared across cezar versions) | `#[serde(flatten)] extra: serde_json::Map<String, Value>` on every affected struct. A `proptest` round-trip test per struct. |
| `.catch(v)` | A bad *field* degrades to `v`; the record survives | `#[serde(default, deserialize_with = "catch_or_default")]` — a helper that deserializes into `Value` first, then tries `T`, falling back on error. |
| `.default(v)` | Missing field takes `v` | `#[serde(default = "…")]` |
| per-entry `safeParse` salvage | One corrupt entry in `runs.json` / `projects[]` drops *that entry*, not the array | Deserialize to `Vec<Value>`, then `filter_map` each through `T::deserialize`, logging drops. Never `Vec<T>` directly. |

Additionally: **writes are read-modify-write merges**, atomic `tmp`+`rename`, mode
`0600` (dirs `0700`), and a corrupt file is *left on disk* after one warning. Port
those mechanics as a single shared `merge_write` helper, not per call site.

### 11.3 The `cezar-server` crate is temporary — build it that way

`cezar-server` exists for exactly one reason: during Phase B the TUI is a separate
process from the engine, so they need a wire between them. **At C2 that wire is
deleted.** Two consequences for how B9 is written:

- **Handlers stay thin.** Every route is a parse-validate-delegate shim over
  `cezar-core`; no business logic, no state, no caching lives in `cezar-server`.
  If a route seems to need logic, that logic belongs in `cezar-core` where the
  in-process path will also find it. The 5.8k-line `server.ts` is *not* the model
  to imitate here — much of its bulk is exactly the kind of thing that must move
  down a layer.
- **Nothing depends on it upward.** `cezar-tui` depends on the `Engine` trait, never
  on `cezar-server`. Deleting the crate at C2 must be a `Cargo.toml` line and a
  directory removal, not a refactor.

Bind to `127.0.0.1` only, and keep `origin-guard`/`host-guard` and the WS
`trusted`/`loopbackReadable` split for as long as the port exists — a listening port
on a developer machine is reachable by any web page that machine loads, which is the
DNS-rebinding threat those guards were written for (#426). They are deleted at C2 with
the listener, not before.

---

## 12. Phase C — one binary, no network

Decision 5 makes this phase much sharper than it was drafted: the end state is not
"one binary that can also serve", it is **one binary that listens on nothing**.

**C1** Implement `InProcessEngine` in `cezar-core` against the `Engine` trait defined
at A2. Because the trait predates the server, this is an implementation, not an
extraction — and the A2 review gates guarantee no screen leaked an HTTP assumption
that would surface here.
**C2** Switch `cezar-tui`'s default backend to `InProcess`, then **delete
`cezar-server` entirely**: the axum dependency, every handler, the SSE and WS
transports, `origin-guard`, `host-guard`, the WS `trusted`/`loopbackReadable` topic
split, and `HttpEngine` in `cezar-client`. The event streams become in-process
`tokio::sync::broadcast` channels, which is what the SSE/WS layers were emulating.
**C3** Retire the remaining server-shaped concepts: `cez serve` as a command, the
port-selection logic (`pickPort`, the 4321 default, the auto-increment probe), the
health-poll startup wait, and the child-process supervisor the TUI used in Phase A.
Fold what remains of `/api/v1/health`'s payload into a plain `cez doctor` command —
the version/update check and the agent-CLI probe are still useful, they just aren't
an HTTP route.

*Accept:* `ss -ltnp` / `lsof -i` shows **no listening socket** while `cez` runs;
`cargo tree` contains no HTTP server crate; `cez` starts in <150 ms cold on a repo
with 500 runs; one binary, no Node, no port, no browser.

---

## 13. Testing and verification strategy

This section exists because the plan will be executed by agents who cannot hold the
whole system in their head. Every step must be checkable mechanically.

1. **Contract fixtures.** `crates/cezar-contract/tests/fixtures/` holds a captured
   real response per route, generated by a script that drives
   `CEZ_DRY_RUN=1 cezar serve`. Deserialization + re-serialization parity is a test.
   Regenerating them is a documented one-liner.
2. **Golden agent fixtures.** `packages/cezar/src/core/__fixtures__/**` are reused
   verbatim by the Rust mappers in B5. No new fixtures are authored; a diff against
   the committed `.expected.json` is the pass condition.
3. **Snapshot rendering.** `insta` + `TestBackend` at 80×24, 120×40, 200×60 for every
   screen and every notable state (loading, empty, error, degraded, live). Reviewing
   a `cargo insta review` diff is how a non-expert agent verifies UI work.
4. **Interaction tests.** Feed synthetic `KeyEvent`/`MouseEvent` sequences into the
   app's update function and assert on state + snapshot. Every keybinding in the
   default keymap has at least one; every `HitMap` action has a click test.
5. **E2E pty.** `expectrl` drives the real binary against a real `cezar serve` in a
   temp repo with `CEZ_DRY_RUN=1`. Covers the run lifecycle, the review gate, and
   navigation. Slow suite, run in CI only.
6. **HTTP-level suite reuse (Phase B).** Point the existing vitest HTTP suites at the
   Rust server. Set this up in B9's first commit.
7. **Property tests.** The zod-compat deserializers (§11.2) get `proptest`
   round-trip coverage; the diff renderer gets fuzzed against `git diff` output.
8. **Terminal matrix.** A documented manual checklist across Ghostty, kitty, WezTerm,
   iTerm2, Terminal.app, Alacritty, tmux and GNU screen: mouse, images, truecolor,
   kitty keyboard protocol, bracketed paste. Record results in `docs/tui/terminals.md`;
   feature-detect and degrade, never assume.

---

## 14. Compatibility obligations

Five of the nine protected surfaces are **unaffected by design** — the refactor is an
implementation change, and `.ai/cezar/` state, `~/.cezar/` state, workflow YAML,
skills Markdown, the agent event protocol and the `CEZ:*` markers all survive
untouched. Four are **waived by owner decision**, and each waiver is listed below with
what it costs. Nothing is waived silently, and nothing outside this list changes
without following `BACKWARD_COMPATIBILITY.md`'s own required path.

**The four waivers, and what each one gives up:**

| Surface | Waived by | What is actually lost |
|---|---|---|
| §6 npm package | decision 4 | `npx cezar-cli` and `npm i -g @open-mercato/cezar` stop working. Install becomes `git clone` + `cargo install --path`. |
| §2 page URLs (SPA shell, `/assets/*`, `/p/:projectId/*` pages, legacy-flat redirect, settings redirects) | decision 3 | Browser access. There is no cockpit to serve. |
| §2 `/new?…` + §1 bookmarklet contract, `GET /api/v1/launch-key`, CORS-open health | decision 5 + the flagged call | The browser-initiated "hand this issue to an agent" entry point. The capability itself moves to §8.9's GitHub screen and `cez new --ref=…`. |
| §1 CLI `-p/--port`, `--no-open` | decision 5 | Nothing meaningful — both describe a server and a browser that no longer exist. Every other flag and the `run` exit-code semantics stay protected. |
| §1/§3/§9 the `CEZ_*` env vocabulary, `CEZ:*` markers, `.ai/cezar/`, `~/.cezar/`, `cez/` branches | decision 6 | Names only. Markers and branch prefixes keep **reading** the old spelling permanently; state dirs migrate automatically (§2.2.2). Env vars change with no shim. |
| §5 `skillsRepos` source shape, `importedSkills` tri-state | decision 7 | Remote team skills are gone. Frontmatter, the `SKILL.md` convention and discovery precedence stay protected. |
| §2 `/automations*`, `/skills/importable`, `/skills/refresh`, `/workspace/skills-update*`, `/fs/browse`, `capabilities.{automations,singleProject,tokenUsageMetrics,costMetrics,tokenMetrics}` | decisions 7–8 | The features behind them. `RunRecord.automation` stays parseable for old records. |

At **Phase C the whole `/api/v1` surface retires too**, not as a waiver but because
its only purpose was to be a boundary between two processes.
- `.ai/cezar/` and `~/.cezar/` files stay byte-compatible. A user must be able to run
  the Node build and the Rust build against the same repo and home directory in
  either order. Add a test that does exactly this (write with one, read with the
  other, both directions) at B2 and keep it through cutover.
- Old NDJSON transcripts must replay. Old `runs.json` records must load. Legacy
  `claude-cli` must still fold to `claude`.
- The CLI's flags, defaults and exit codes are reproduced exactly by `clap` — with a
  test that asserts `--help` output still names every protected flag. The `cezar` and
  `cez` command names survive; only their *installation mechanism* changes.
- **Every waiver above gets an explicit CHANGELOG breaking entry.** A waiver is the
  owner's call and these are recorded in "Resolved decisions", but the entries are
  still required so none of it is silent to anyone reading history later. Name the
  removed capability, not just the removed code — "the bookmarklet is gone, use the
  GitHub screen or `cez new --ref=`" is useful; "removed launch-key.ts" is not.
- `AGENT_PROTOCOL.md`, `AGENTS.md` and `BACKWARD_COMPATIBILITY.md` are updated in the
  same PR as the code they describe, per this repo's existing rule.

---

## 15. Risks

| Risk | Severity | Mitigation |
|---|---|---|
| `RunManager` port (B6) is subtly wrong — recovery, leases, quota routing | **High** | Port `run.test.ts` first, as the spec. Soak both servers side by side in B11 before cutover. |
| zod-compat mistakes delete user data | **High** | §11.2 patterns, `proptest` round-trips, cross-implementation read/write test from B2 onward. |
| Terminal fragmentation (images, mouse, keys) | Medium | Feature-detect at startup, degrade honestly, document the matrix, never hard-fail. |
| Transcript performance on long runs | Medium | Height cache + virtualization + delta coalescing designed in at A7; criterion bench as a gate. |
| Scope creep — "while we're in here, let's redesign X" | Medium | Non-goal §3.1. Behavior changes need their own spec. |
| Phase B stalls after A ships, leaving a Node dependency **and** a two-toolchain install | Medium | Phase A is still a usable product, but with source-first distribution the stall is more visible than it would have been. Keep the README's prerequisite list honest at all times. |
| **Deleting `packages/web` removes the last independent client** — after B12, nothing but the TUI exercises the API | Medium | This is why B11's soak runs the React cockpit against the Rust server *before* B12 deletes it. Do not reorder those two steps. After B12, the HTTP-level vitest suites (ported per §13.6) are the remaining independent check — port them, do not let them lapse. |
| A15's deletions remove something still in use | Low | A15 is a pure-deletion step with a green suite on the other side; anything it breaks shows up immediately and is one `git revert` away. Ship it as its own PR, not folded into A14 or B1. |
| The `~6k` deleted lines turn out to contain logic something else depended on | Low | The deletions are whole subsystems reached only through their own CLI commands, routes or UI sections — not shared helpers. A15's `rg` assertions plus a green suite are the check. |
| Rust toolchain is now a **user** prerequisite (source-first install) | Medium | `rust-toolchain.toml` pins it; `install.sh` detects `rustup` and prints the exact fix; every dependency must build on stable with no nightly features. Accepted cost of the owner's distribution decision — the audience is developers who already have agent CLIs installed. |
| Phase A users need **both** Rust and Node 20+ | Medium | Stated plainly in the README and checked by `install.sh`; disappears at Phase B cutover. Written so the removal is a one-line diff. |

---

## 16. Alternatives considered

- **Keep React, render it to the terminal (Ink / OpenTUI).** Reuses component logic
  but keeps the whole project TypeScript — fails the stated goal, and Ink's layout
  model is a poor fit for the dense tables and diffs here.
- **Rewrite everything in Rust in one pass.** No intermediate verifiable state
  against 90k lines of behavior-dense source. Rejected.
- **TUI as a thin `curl`-style client with no local state.** Simpler, but the
  cockpit's value is live streaming, optimistic updates and cross-screen state; a
  stateless client would be materially worse than the web app.
- **Embed the Node engine via a sidecar forever (skip Phase B).** Viable, and it *is*
  Phase A. Not chosen — the owner approved all three phases — but Phase A remains a
  working product if the later phases stall.
- **Keep the React cockpit as a second client.** Rejected by the owner (decision 3).
  The cost it would have bought: an independent exerciser of the API after B12, and a
  browser/mobile surface. The first is mitigated by porting the HTTP-level suites
  (§13.6); the second is not replaced, and decision 5 says it should not be.
- **Keeping a small local HTTP endpoint for the bookmarklet.** Rejected — see "the one
  call this spec makes". It is the only thing that would force a network surface to
  survive Phase C, and the GitHub screen already covers the workflow.
- **`git2`/`gix` instead of shelling out to `git`.** Rejected for the port itself
  (behavior risk); revisit later as a performance change with its own spec.

---

## 16a. Further strip-out — RESOLVED

A sweep for everything else that exists **because there was a browser, a registry, or
a shared deployment**. All three tiers are now decided (owner, decisions 7 and 8):
**everything below is deleted except clone-from-GitHub.** The tables are kept as the
record of what went and why, because a future reader will otherwise re-propose them.

### Tier 1 — dead by construction, deleted at A15 (no decision needed)

| What | Size | Why it is dead |
|---|---|---|
| `src/release/{stable,snapshot,manifests}.ts` + tests, and `CEZ_RELEASE_CHANNEL` / `CEZ_RELEASE_ROOT` / `CEZ_SNAPSHOT_ROOT` | ~0.8k src + ~0.6k tests | Pure npm release-channel stamping and version pinning. Nothing to publish (decision 4). |
| `alias-cezar/` | 2 files | The `npx cezar-cli` alias package. |
| The update chip: `latestVersion` on `/health`, the `update` dep, the cockpit's pulsing-dot | small | `server.ts:202` already says *"The CLI does not contact a package registry"* — the field is supplied externally and, with no registry, can never be populated. It has been decorative in local mode all along. |
| `CEZ_API_BASE`, `CEZ_API_PORT`, `VITE_CEZ_API_BASE`, the Vite dev proxy | small | Only exist to point a browser bundle at a server. |
| `docs/mockups/` (5 HTML pages, `tokens.css`, 14 screenshots) | ~1k | Mockups of a deleted UI. Port `tokens.css`'s palette into the Rust theme first (§7.5), then delete the directory. |
| `packages/web/src/assets/fonts` | binary | Web fonts. A terminal uses the user's font. |
| `link-safety-dialog.tsx` | small | An "are you sure you want to leave for this external URL" interstitial — a browser-navigation safety pattern. In a TUI, opening a link is an explicit `o` press with the URL visible. |

### Tier 2 — network features (decision 7: only clone-from-GitHub survives)

| What | Size | Verdict | Reasoning |
|---|---|---|---|
| **Open Mercato skills auto-update** — `skills-update.ts`, `SkillsUpdateCoordinator`/`Service`, 3 API routes, a settings toggle, the nav badge, 6-hour cached checks, a cross-process lock | ~0.4k src + tests + UI | **DELETE** | A default-on network mutation of files on disk at boot, tracking an upstream catalog this fork does not follow. |
| **Team skills from remote git repos** — `skills-remote.ts`, `config.json` `skillsRepos`, bare clones into `~/.cache/cez/skills/` | ~0.5k src + tests | **DELETE** | Built for teams. `~/.agents/skills/` is already a supported discovery location and needs no clone, cache or network. |
| **Clone from GitHub** — `checkout.ts`, `POST /projects/checkout`, `checkout-progress` SSE, the Clone dialog | ~0.35k src + ~0.5k tests | **KEEP** | The one Tier-2 feature retained. Adding a project by URL without leaving the TUI is worth the module. Its path-safety rules stay as written — `projectsDir` and `DUCK_PROJECTS_DIR` survive with it. |
| **GitHub automations** — `automations/{coordinator,scheduler,github-poller,store,task-template}.ts`, 8 API routes, a 4-mode UI screen, receipts + log | ~1.5k src + ~1.2k tests + UI | **DELETE** | A background poller premised on an always-running process. A terminal app is open when you work and closed when you don't. |

### Tier 3 — hosted-deployment settings (decision 8: **all four deleted**)

| What | Why it existed | Why it goes |
|---|---|---|
| `CEZ_HIDE_TOKEN_METRICS`, `CEZ_HIDE_TOKEN_USAGE`, `CEZ_HIDE_COST` + `capabilities.{tokenUsageMetrics,costMetrics,tokenMetrics}` | Hide spend from people looking at a shared/demo instance. | You are the only viewer and it is your spend. Three env vars, three capability flags, a fail-closed combined value, and conditional rendering in ~6 components. |
| `CEZ_SINGLE_PROJECT` + `capabilities.singleProject` | Constrain a deployment to one repo and hide workspace-expansion affordances. | A constrained *deployment* is not a thing anymore. |
| `CEZ_BROWSE_ROOT` / `browseRoot` + all of `fs-browse.ts`'s realpath-containment model | `fs-browse.ts`'s own comment: *"the one route that hands the operator's filesystem shape to a browser"*. | There is no browser. A TUI folder picker reads the filesystem directly, with the user's own permissions. The whole containment apparatus — realpath chains, symlink escape prevention, the configurable root — is defending against a threat that no longer exists. Keep the picker, delete the boundary. |
| Notifications settings — `notificationSupport`, the permission-state machine, the "notifications blocked" explainer | Browser Notification API permission model. | `notify-rust` has no permission handshake. Collapses to a single on/off toggle. |
| `wsl.ts` — WSL↔Windows path translation for `open-in-*` | Open a worktree in a *Windows-side* editor from a WSL server. | Not a workflow this project supports. **Note the precise scope:** running inside WSL still works (it is Linux); what goes is handing a worktree to a *Windows-side* app through interop. `open-in-*` uses the ordinary Linux path there. |

### 16a.1 Knock-on effects of decisions 7 and 8

The deletions above are not isolated modules — four of them reshape a surface this
spec already described. Implementing agents must apply these, or they will build
screens for features that no longer exist:

**The Skills surface collapses to a reader.** With both skills-network features gone,
`skills.ts` (local discovery across the six documented locations, with its precedence
rules) is *all* that remains. Deleted with them: `GET /skills/importable`,
`POST /skills/refresh`, `GET /workspace/skills-update`,
`POST /workspace/skills-update/{check,apply}`, the `skillsRepos` config key, the
`skillsAutoUpdate` / `effectiveSkillsAutoUpdate` workspace keys,
`DUCK_SKILLS_AUTO_UPDATE`, the `importedSkills` tri-state in workspace ui-state, the
`skills-update` nav badge, the import panel and the update banner. **§8.11 is revised:
the Skills screen is a master/detail browser over locally discovered skills — a list,
a rendered body, a source badge. Nothing more.** `BACKWARD_COMPATIBILITY.md` §5's
`skillsRepos` source-shape and `importedSkills` clauses are waived with it; the
frontmatter format, the `SKILL.md` convention and the discovery precedence stay
protected, because that is what the composer and the workflow builder still read.

**§8.10 Automations is deleted outright** — the screen, the nav item, the 8 routes,
`DUCK_AUTOMATIONS`, `capabilities.automations`, `automation-checks`, `automation-log`
and the receipts/high-watermark files. One residue is deliberate: `RunRecord.automation`
(the provenance stamp on tasks a past automation launched) stays **parseable** so old
`runs.json` records still load; nothing writes it and nothing displays it. That is the
§3 additive-read rule, not an exception to it.

**The folder picker loses its boundary, not itself.** `fs-browse.ts` and
`GET /api/v1/fs/browse` are deleted; the TUI's "Add project → local folder" picker
reads the filesystem directly with the user's own permissions. `projectsDir` survives
(clone-from-GitHub needs a destination); `browseRoot` does not.

**Metrics are unconditional.** Token counts and cost always render. Delete the three
capability flags, the fail-closed combined value, and every conditional around them —
this reverts the #737 presentation work rather than porting it.

**Settings loses two more sections.** After decision 5 removed Bookmarklets, decision
7 removes the skills-auto-update toggle from **Resources** (the rest of that section —
`maxParallel`, `memoryLimitMb`, `worktreeRetentionDefault`, `autoResumeOnUsageLimit` —
stays), and there is no Automations section to build. Final section list: **project** —
Agents, Agent config, Worktrees, Prompt templates; **global** — Accounts, Appearance,
Notifications (one on/off toggle), Resources, Projects.

### What is explicitly **not** on this list

Worth naming, so nobody deletes them in a tidying mood: `open-in-app` /
`open-in-terminal` / `open-in-file` / `open-in-project` (local OS handoff, core to the
product), `provider-auth` and the quota/usage subsystem (reads through the agent
CLIs, not the network), the `gh`-driven GitHub read/PR/merge surface (§8.9 — this is
what replaces the bookmarklet), `git`-shelling worktree management, and the
multi-project registry.

---

## 17. Decisions

1. ~~**Scope of commitment**~~ — **all three phases.** The `Engine` trait seam moves
   into Phase A (step A2) with two CI-enforced review gates.
2. ~~**Dictation**~~ — **dropped.** No port, no hook, no substitute. §9.3.1.
3. ~~**The React cockpit**~~ — **deleted** at B12, after the B11 soak uses it as the
   final parity check. `packages/contract` and `packages/api-client` go with it.
   Decision 5 then removes the browser entry points that would have outlived it.
4. ~~**Distribution**~~ — **source-first** (clone + `cargo install --path`). No npm,
   no prebuilt binaries, no `npx` compatibility.
5. ~~**Deployment shape**~~ — **single-machine, single-user, terminal-only.** No
   remote access of any kind; Phase C ships with no listening port. §1.4a lists the
   ~6k lines this deletes rather than ports.

6. ~~**Rename**~~ — `cezar` → `coducktor`, with **`duck`** as the single short token:
   commands `coducktor` + `duck`, env `DUCK_*`, markers `DUCK:*`, branches
   `duck/<id8>`. Directories use the full `coducktor`. §2.2.
7. ~~**Tier 2 network features**~~ — **keep clone-from-GitHub only.** Skills
   auto-update, remote team skills and GitHub automations are all deleted. §16a.
8. ~~**Tier 3 hosted-deployment settings**~~ — **none survive.** All four deleted.
   §16a.

**One judgment call is flagged for reversal** — dropping the bookmarklet (see "The
one call this spec makes on the owner's behalf", above). Everything else is directly
owner-decided.

**All eight decisions are resolved.** Q7–Q9 changed *how much gets deleted*, not the
shape of the plan. Step **A15** is where every deletion and the user-facing half of
the rename land, so it is the largest single step in Phase A — plan it as its own PR
series, one subsystem per commit.

The natural plan boundaries are the numbered steps in §10 (A0–A15), §11.1 (B0–B12)
and §12 (C1–C3); each already carries an acceptance criterion, and §13 defines how a
non-expert agent verifies its own work.
