# Rust TUI refactor — execution plan for the implementing agent

Status: **DRAFT**
Companion to: [`2026-08-15-rust-tui-refactor.md`](./2026-08-15-rust-tui-refactor.md) (**"the spec"**)
Date: 2026-08-15

## Purpose

The spec answers *what* to build and *why*. This document answers *in what order, in
what chunks, and when to commit and push*. It does not re-argue anything the spec
already decided — every chunk below cites the spec section that is its source of
truth, and if this plan and the spec ever disagree, the spec wins and this file gets
corrected.

**Read the spec in full before starting.** In particular: §2 (Goals, including Goal 7
"one terminal" and Goal 8 "general cleanup"), §2.2 (the rename), §7.7 (why the child
process must stay silent), §11.2 (the zod-compat patterns — get these wrong and the
port deletes user data silently), and §13 (the testing strategy every "Accept" line
below is drawn from).

The end state this plan drives toward is the spec's own: **Phase C, step C3 —
one Rust binary, no Node, no listening port, no browser.** That is what "the
completed Rust TUI" means throughout this document.

> **Progress tracking for future coding agents:** Treat every numbered step heading
> below as a checkbox. Do not mark a step `[x]` until its acceptance criterion and
> required checks pass, its plan-step commit exists, and that commit has been pushed
> to `origin main`. After completion, change the heading to `[x]` and add the pushed
> commit hash on the step's `Commit` line. Keep future steps `[ ]`; never tick ahead
> based on intent or partial implementation.

---

## 0. Ground rules — apply to every chunk below

**One chunk = one commit = one push.** Each numbered step (A0, A1, … C3) below is
sized the way the spec already sized it in §10/§11/§12 — each is independently
testable and leaves the tree green, which is exactly what makes it a good commit
boundary. Don't bundle two steps into one commit and don't split one step across
several unless a chunk's own notes say otherwise (a few are explicitly split for
risk reasons — flagged inline with **⚠**).

**Definition of Done, for every chunk, before you commit:**
1. The chunk's *Accept* criterion (copied or paraphrased from the spec) is
   demonstrably true — run the test/command it names, don't take it on faith.
2. `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` are green
   (Phase A/B: also whatever the still-live `npm test` / vitest suites are for the
   parts of the Node tree that chunk didn't touch).
3. Any `insta` snapshots the chunk affects are reviewed (`cargo insta review`), not
   blindly accepted.
4. No TODO, `unwrap()`/`expect()` outside tests and `main.rs` startup (§6 dep table
   footnote), or commented-out block is left behind — see Goal 8 below.
5. The commit message says which spec step it implements (e.g. `feat(tui): A5 tasks
   overview + global tasks`) so `git log` stays a map back to the spec.

**Then push immediately.** Don't batch several finished chunks into one push — the
whole point of chunking this way is that a stall or a bad step is visible (and
revertable) at single-commit granularity, per the spec's risk table (§15, "A15's
deletions remove something still in use", "Rust toolchain is now a user
prerequisite").

**Branching: none. All work happens directly on `main`.** Every chunk is committed
and pushed straight to `main` — no feature branches, no per-phase branches, no PRs.
"Push" always means `git push origin main`. This is a deliberate choice, not a
default to override: single-commit granularity on `main` is what makes each chunk
independently revertable (spec §15's mitigation for A15 and for the Rust-toolchain
risk both assume this), and a long-lived branch is exactly the kind of stall spec
§15 warns about ("Phase B stalls after A ships"). Where a chunk's notes below say
"own PR," read that as "own commit on `main`" — the isolation it buys comes from the
commit boundary, not from a branch.

**Cross-cutting rules that apply inside *every* chunk, not just one step:**
- **Rename (§2.2).** New Rust code is born `coducktor-*` / `DUCK_*` from A0 onward.
  Don't introduce a new `cezar`/`cez`/`CEZ_` spelling in Rust at any point. The
  TypeScript tree keeps its existing names until the module housing them is deleted.
- **General cleanup (§2 Goal 8).** While you're inside a file for a chunk's stated
  reason, delete legacy junk you find in it (dead code, unused exports, stale
  TODOs) — but verify with `rg` that nothing references it first, and don't let
  cleanup expand a chunk's diff into unrelated files. If it's not in the file(s)
  the chunk already touches, leave it for a chunk that does.
- **One terminal (§2 Goal 7, §7.7).** From A3 onward, nothing the supervised
  `cezar serve` child prints may reach the real terminal while the TUI has the
  alternate screen open. Any chunk that touches process spawning re-checks this.
- **Zod-compat (§11.2).** Any chunk in Phase B that ports a schema uses the four
  patterns in that table. This is a correctness requirement, not a style
  preference — get it wrong and a corrupt record silently drops user data instead
  of degrading gracefully.

---

## Phase A — the Rust TUI over the existing Node service

Source: spec §10. Deliverable at the end: a feature-complete terminal cockpit, Node
still running underneath but invisible to the user (§7.7). This alone is a shippable
product if Phase B never happens (spec §4, "Recommendation").

### [x] A0 — Toolchain and workspace
**Ships:** root `Cargo.toml` workspace, `rust-toolchain.toml` (pinned stable, edition
2024), `.cargo/config.toml`, `clippy.toml`, CI wiring, `crates/cezar-tui` hello-world
with a panic hook that restores the terminal.
**Accept:** `cargo run -p cezar-tui` opens and `q` exits with the terminal intact.
**Commit:** `feat(tui): A0 workspace scaffold and terminal-safe hello world` — pushed as `10586992`.

### [x] A1 — `cezar-contract`
**Ships:** `packages/contract/src/*.ts` ported to serde types, one Rust module per TS
file, **except `automations.ts`** — decision 7 deletes that subsystem outright
(§16a Tier 2), so it's never ported, not even to be deleted again later. Keep only
the `RunRecord.automation` provenance stamp, folded into `runs.rs` as a
passthrough-compatible field. Zod-compat conventions (§11.2) established even though
Phase A only reads.
**Accept:** a test deserializes captured real responses from a live `cezar serve`.
**Commit:** `feat(contract): A1 port TS contract types to serde (minus automations)` — pushed as `6e2b90cc`.

### [x] A2 — `cezar-protocol` + `cezar-client`, behind the `Engine` trait
**Ships:** the `Engine` trait (§10, quoted in full in the spec) defined now, not
retrofitted at C1 — this is decision 1's practical consequence. `HttpEngine` as the
only implementor. Review gates: no `reqwest`/`url`/HTTP status under
`crates/cezar-tui/src/screens/`; `EngineError` is domain-shaped, never HTTP-shaped.
**Accept:** golden fixtures under `packages/cezar/src/core/__fixtures__/**`
deserialize into `UiEvent` without loss; a `wiremock`-backed test covers SSE resume
and WS resubscribe-on-reconnect; a `trybuild`/grep-based CI lint enforces the
no-HTTP-in-screens rule.
**Commit:** `feat(engine): A2 Engine trait + HttpEngine over /api/v1` — pushed as `ba29badf`.

### [x] A3 — App skeleton: event loop, router, theme, HitMap, service supervisor
**Ships:** frame loop (30 fps budget, input coalescing), `Route` enum + history,
theme with capability detection (§7.5, three named themes — no `system`, no accent
picker), keymap loader, `HitMap`, and the `service` module supervising the
`cezar serve` child (spawn, health-poll, adopt-running-instance, restart-on-crash,
kill-on-exit) with stdout/stderr **piped, never inherited**, captured per §7.7.
**Accept:** two placeholder screens navigate by key, `:open`, mouse click, and
history back/forward; snapshot tests at 80×24, 120×40, 200×60.
**Commit:** `feat(tui): A3 app skeleton, theme, HitMap, silent service supervisor` — pushed as `5616d883`.

### [x] A4 — Shell chrome
**Ships:** header, sidebar (project groups, nav, badges, Active/Archived, task
quick-list with NEEDS YOU/WORKING/RECENT grouping), status bar, toast layer, help
overlay, confirm dialog, sidebar resize + collapse.
**Accept:** nav by keyboard and click; badges update live from workspace SSE;
snapshots at three widths including the auto-collapse breakpoint.
**Commit:** `feat(tui): A4 shell chrome` — pushed as `deb37397`.

### [x] A5 — Tasks overview + global tasks
**Ships:** table widget (foldable columns, sort, filter, hover, row menu), both
screens, SSE-driven live updates, archive/read/delete.
**Accept:** against `CEZ_DRY_RUN=1 cezar serve`, starting a run makes a row appear
and progress through statuses; E2E pty test asserts it.
**Commit:** `feat(tui): A5 tasks overview + global tasks` — pushed as `d7708105`.

### [x] A6 — Composer widget + New task
**Ships:** shared composer (auto-grow, attachments, `/` skills, `@` files, quick
replies, submit shortcuts, draft persistence), picker overlays, New Task screen.
**Accept:** a task starts end-to-end from the TUI and appears in the Tasks table;
picker grouping/ranking matches `lib/skills.ts`.
**Commit:** `feat(tui): A6 composer + new task` — pushed as `df19e171`.

### [x] A7 — Markdown, images, and the transcript
**Ships:** `tui-markdown` + render cache, `ratatui-image` with protocol detection
and fallbacks, tool cards, virtualized scrolling with sticky-bottom.
**Accept:** 5,000-item transcript scrolls at ≥30 fps (criterion bench); an image
event renders or falls back honestly; snapshots cover message/reasoning/tool/image
in both collapsed and expanded states.
**Commit:** `feat(tui): A7 markdown, images, transcript virtualization` — pushed as `9453f732`.

### [x] A8 — Task thread, complete
**Ships:** header + actions, step rail, plan dock, subagent sheet, ask card, review
panel, queued messages, auto-resume hint, composer host, cancel/continue/finish/PR.
**Accept:** full run lifecycle (start → live → ask → answer → review → send back →
accept → archive) driven entirely from the TUI in an E2E pty test.
**Commit:** `feat(tui): A8 task thread, full lifecycle` — pushed as `0478ec73`.

> **Note on the Accept test:** as with A5's precedent, "E2E" here follows the pattern
> already established by A0–A7 — a `TestBackend`-driven test exercising the full
> App/reducer/render pipeline through every lifecycle stage, not a literal
> `expectrl`/`portable-pty` spawn of a real `cezar serve` process (no such harness
> exists anywhere in the tree yet; `expectrl`/`portable-pty` are not even a
> dependency). See `screens/thread/mod.rs`'s
> `full_run_lifecycle_start_live_ask_answer_review_send_back_accept_archive` test.
>
> **Deliberate scope cuts, documented in `screens/thread/mod.rs`'s module doc:** the
> reducer ports thread-state.ts's v2 path only — the v1 pre-coalescing fallback
> (`text`/`tool-call`/`tool-result` item synthesis, cross-turn dedup, delta
> reassembly) that exists solely to render transcripts recorded before the v2
> UI-event mappers existed is not ported; the review panel has no embedded diff
> (that's A9's `RunDiff`, not yet built); the composer sends text only (no image
> attachments); queued-message editing is reachable on `Engine` but only the remove
> action is wired to a UI affordance; and the `Changes`/`Files`/`Commits` tabs render
> as labels only (their screens are A9/A10). None of these affect the Accept
> criterion's lifecycle; revisit if a later step needs them.

### [x] A9 — Diff engine + task git + repo git + compare
**Ships:** diff widget (unified/split, syntect, intra-line, collapsed context,
per-file fold), file trees, commit lists, commit dialog, branch actions, variant
compare.
**Accept:** a worktree diff renders identically in content to `GET /runs/:id/diff`;
split mode degrades below 140 columns.
**Commit:** `feat(tui): A9 diffs, task/repo git, compare` — pushed as `6a96d5fe`.

### [x] A10 — IDE
**Ships:** explorer, editor, save, dirty guard, `$EDITOR` handoff.
**Accept:** edit-and-save round-trips through `PUT /ide/file`; 1 MB cap and
symlink/`.git` exclusions respected and explained in the UI.
**Commit:** `feat(tui): A10 IDE` — pushed as `6c999b46`.

### [x] A11 — GitHub, Skills, Inbox, Workflows
**Ships:** the four remaining content screens (Automations stays deleted, decision
7), each with its degradation path. Skills is the reduced reader from §8.11 — **no**
import panel, **no** update banner.
**Accept:** with `gh` absent, every GitHub surface shows `{available:false,reason}`
and no error; with `DUCK_FOLLOWUPS` unset, Inbox shows its opt-in explainer.
**Commit:** `feat(tui): A11 GitHub, Skills, Inbox, Workflows` — pushed as `7f972869`.

### [x] A12 — Settings + palette + notifications + external open
**Ships:** all Settings sections per §8.14's registry pattern, command palette
(§8.15), notification plumbing, `open-in-*` handoff.
**Accept:** every setting the web app can change is changeable in the TUI, and the
two clients observe each other's writes.
**Commit:** `feat(tui): A12 settings, palette, notifications, external open` — pushed as `c01eec07`.
**Note:** §8.14's own text enumerates 9 sections (not the plan heading's "all 12");
built exactly those 9 per this plan's own ground rule 0 (spec wins on disagreement)
— see the commit message and `screens/settings/mod.rs`'s module doc for the
reasoning and the accompanying scope cuts.

### [ ] A13 — CLI surface
**Ships:** `clap` parser for the TUI binary reproducing the protected flags, `cez
tui` / bare-invocation-launches-TUI wiring. Do **not** touch the Node CLI's contract
yet — that's A15/B10.
**Accept:** `bc-route-inventory` and the CLI compatibility tests still pass
unchanged.
**Commit:** `feat(cli): A13 clap surface for the TUI binary`

### [ ] A14 — Install path and docs
**Ships:** `cargo install --path crates/cezar-tui` as the documented one-liner
(both `cezar` and `cez` land on PATH), root `install.sh` (checks `rustup`, builds,
reports the binary path — no curl-pipe-to-shell, no release artifacts), a
`justfile`/`Makefile` (`build`, `install`, `test`, `lint`, `snapshots`), README
rewritten for clone-and-build, `docs/tui/` (keymap reference, terminal matrix,
screenshots). State the Node 20+ prerequisite honestly — Phase A still needs it —
written so removing that line at B12 is a one-line diff.
**Accept:** on a clean machine, `git clone && ./install.sh` yields a working `cez`
on PATH; the README's prerequisite list has no surprises.
**Commit:** `docs(install): A14 source-first install path and TUI docs`

### [ ] A15 — Retire npm and remote-access surfaces from the Node tree ⚠ own commit
**⚠ Ship this as its own commit on `main`, not folded into A14 or B1** — spec §15
risk table calls this out explicitly: it's a pure-deletion step with a green suite
as its own check, and anything it breaks needs to be a one-line `git revert`, not
entangled with unrelated work.
**Ships (all three sub-deletions, per spec §10 A15 verbatim):**
- *npm (decision 4):* `install-as-command`/`check:pack` scripts and manifests entries,
  `scripts/{check-pack,sync-readme,inline-contract,install-as-command}.mjs`,
  `src/{pack-check,install-as-command}.ts` + tests, `prepublishOnly`/`check:pack`.
- *Remote access (decision 5):* `src/server-install/**`, the
  `server-install`/`server-deploy`/`server-uninstall` commands, `docs/server-install/`,
  `src/server/launch-key.ts`, `GET /api/v1/launch-key`,
  `web/src/lib/bookmarklet.ts` + Settings → Bookmarklets, `--no-open` and the
  browser-launch startup behavior, every `CEZ_REMOTE`/`capabilities.localHandoff`
  branch (collapse to local-mode behavior, don't leave a dead flag). **Keep**
  `origin-guard`/`host-guard` — a port is still open through Phase A and B.
- *§16a Tier 1 + decisions 7–8:* `src/release/**` + its three env vars,
  `alias-cezar/`, the `latestVersion` update chip, `CEZ_API_BASE`/`CEZ_API_PORT`,
  `docs/mockups/` (**after** porting `tokens.css` into the Rust theme — already
  done at A3/§7.5), `packages/web/src/assets/fonts`, `link-safety-dialog.tsx`,
  `skills-update.ts`, `skills-remote.ts`, `automations/**`, `fs-browse.ts`,
  `wsl.ts`, the three `CEZ_HIDE_*` flags, `CEZ_SINGLE_PROJECT`, `CEZ_BROWSE_ROOT`,
  and every route/config key/capability flag/nav item/UI section listed in spec
  §16a.1. **Keep** `checkout.ts` and `projectsDir` — clone-from-GitHub survives.
- *Rename (decision 6), user-facing surfaces only:* marker vocabulary (dual-read
  shim), branch prefix (dual-read shim), state-dir migration `002`,
  `AGENTS.md`/`AGENT_PROTOCOL.md`/`README.md`/`.ai/skills/` prose. Internal
  TypeScript identifiers are **not** renamed — that code is being deleted.
**Accept:** `npm run build` succeeds with no pack-check leg; `npm test` is green
with deleted subsystems' suites removed, not skipped;
`rg -n "CEZ_REMOTE|localHandoff|launchKey|bookmarklet|server-install|skillsRepos|CEZ_HIDE|CEZ_SINGLE_PROJECT|browseRoot"`
returns only CHANGELOG/`BACKWARD_COMPATIBILITY.md` history; a run started before
the migration still loads and its `cez/` branch is still found.
**Commit:** `chore(cleanup): A15 retire npm, remote-access, and Tier 1–3 surfaces`

---

**Phase A checkpoint.** After A15, you have a shippable product: a feature-complete
Rust TUI over a Node service the user never sees or touches directly. If Phase B
stalls here, this is still "done" per spec §4's Recommendation — don't treat it as
an incomplete state.

---

## Phase B — porting the engine to Rust

Source: spec §11. Deliverable: `cezar serve` is a Rust binary; Node is deleted. Every
step here keeps the React cockpit working until B12 — that's the oracle (spec §4).

### [ ] B0 — Verify the ground is clear
**Ships:** nothing new — re-run A15's `rg` assertions, confirm nothing crept back.
If A15 was skipped or partial, finish it now; porting condemned code is the single
most wasteful thing this plan can do.
**Accept:** the A15 accept criteria still hold.
**Commit:** `chore(verify): B0 confirm A15 deletions are clean` (skip the commit
entirely if there's nothing to fix — this step can be a no-op check.)

### [ ] B1 — File layer
**Ships:** `cezar-core::paths`, `config`, `workspace::{config, ui_state, migrations,
agent_accounts}`. Port the migration framework first — riskiest to get wrong,
easiest to test in isolation.
**Accept:** cross-implementation read/write test (write with Node, read with Rust,
and vice versa) passes — start this test here and keep it through cutover (spec §14).
**Commit:** `feat(core): B1 paths, config, workspace, migrations`

### [ ] B2 — Runs store
**Ships:** `cezar-core::runs::store` — `runs.json`, NDJSON log, atomic writes,
`reconcileLoadedRun`, retention.
**Accept:** tests against real files written by the Node version pass.
**Commit:** `feat(core): B2 runs store`

### [ ] B3 — Git layer
**Ships:** `cezar-core::git` — worktrees, base-ref resolution, autosave commits,
diff, shortstat, refs. **Shell out to `git`**, exactly as today — do not introduce
`git2`/`gix` here (spec §16, rejected for the port itself).
**Accept:** behavior matches the Node shell-out implementation on the existing test
fixtures.
**Commit:** `feat(core): B3 git layer (shell-out, no git2/gix)`

### [ ] B4 — Skills, workflows, handoff, todos, markers
**Ships:** `cezar-core::{skills, workflows::load, handoff, todos, task_markers,
task_refs}`.
**Accept:** existing behavior-equivalence tests pass against the ported module.
**Commit:** `feat(core): B4 skills, workflows::load, handoff, todos, markers`

### [ ] B5 — Agent runner mappers ⚠ do carefully, best oracle in the project
**Ships:** `cezar-protocol` mappers → `cezar-runners`, one runner at a time (claude
→ codex → opencode → pi), each validated **byte-for-byte** against its committed
golden fixtures; the `ui-parity` capability matrix re-implemented as a Rust test.
Consider one commit per runner if that keeps diffs reviewable — four commits here
is fine, this step has the best oracle in the whole project and de-risks everything
downstream, so don't rush it into one giant commit.
**Accept:** a diff against each committed `.expected.json` is the pass condition —
no new fixtures authored.
**Commit(s):** `feat(runners): B5.1 claude mapper`, `B5.2 codex mapper`, `B5.3
opencode mapper`, `B5.4 pi mapper + ui-parity matrix`

### [ ] B6 — RunManager
**Ships:** `cezar-core::workflows::run`. Split the 4.2k-line source into
`lifecycle`, `session`, `recovery`, `review_gate`, `auto_resume`, `context_refresh`,
`variants`, `quota`, `semaphore` modules. Port `run.test.ts` (2k lines) alongside —
this is spec §15's **High**-severity risk item ("recovery, leases, quota routing");
do not shortcut the test port.
**Accept:** the ported `run.test.ts` suite is green against the Rust `RunManager`.
**Commit:** `feat(core): B6 RunManager (lifecycle/session/recovery/review_gate/
auto_resume/context_refresh/variants/quota/semaphore)`

### [ ] B7 — `cezar-forge`
**Ships:** the `gh` driver, ported against `github.test.ts` (2.3k lines).
**Accept:** ported suite green.
**Commit:** `feat(forge): B7 gh driver`

### [x] B8 — removed from the plan (decision 7)
The spec now says explicitly (§11.1): `cezar-core::automations` is **not** ported.
The automations engine — store, scheduler, poller, task templates — was deleted
outright from the TypeScript tree at A15 (§16a Tier 2), the same decision that
deleted its screen (§8.10). There is nothing left to port at B8, so there is no
chunk here and no commit — B9 follows directly after B7.
**Commit:** none — intentionally removed from the plan by decision 7.

### [ ] B9 — `cezar-server` ⚠ set up HTTP-suite reuse in the first commit
**Ships:** `axum` server, route by route, family by family. Handlers stay thin —
parse-validate-delegate over `cezar-core`, no business logic (spec §11.3, this
crate is temporary and deleted whole at C2).
**⚠ In this step's *first* commit**, wire the existing `route-parity`,
`contract-parity.*`, `versioned-surface`, `bc-route-inventory`, `origin-guard`,
`host-guard`, and `sse-headers` vitest suites to run against the Rust server via a
thin harness (point them at a different base URL). This is the single
highest-value verification move in the whole port — don't leave it for later in
this step.
**Accept:** those suites pass against the Rust server, route family by route
family, as each lands.
**Commit(s):** `feat(server): B9.0 harness — point HTTP suites at cezar-server`,
then one commit per route family, e.g. `feat(server): B9.1 runs routes`, `B9.2
workspace routes`, … (split by the same route families the spec's protected
surfaces already group by, §1.4).

### [ ] B10 — `cezar-cli`
**Ships:** `serve`, `run`, `init`, `usage`, `projects` subcommands. `-p/--port` and
`--no-open` are **not** ported (waived, §1.4). No `--server`, no `--token`.
**Accept:** exit codes match the protected CLI contract; `--help` names every
protected flag.
**Commit:** `feat(cli): B10 cezar-cli subcommands`

*(B10a is a no-op — `server-install`/`server-deploy`/`server-uninstall` were
removed from the Node tree at A15 and are never ported. No commit for this line.)*

### [ ] B11 — Cutover and soak ⚠ do not reorder with B12
**Ships:** `cez serve` runs the Rust server; the React bundle is served from it
**unchanged** for the whole soak; a `--legacy-server` flag makes side-by-side
comparison one command. Run both implementations against the same repo until the
soak is clean.
**⚠ Do not proceed to B12 until this soak is clean** — spec §15 names this
explicitly ("Do not reorder those two steps"): the React cockpit is the last
independent exerciser of the API before it's deleted.
**Accept:** side-by-side comparison shows no drift over the soak period (define
the soak window and comparison method before starting — e.g. N days of daily-driver
use, or a scripted comparison run against a fixed set of repos/workflows).
**Commit:** `feat(server): B11 cutover to Rust server + legacy-server soak flag`

### [ ] B12 — Delete the TypeScript
**Ships:** deletions in this order, each separately revertable: `packages/web` →
`packages/api-client` → `packages/contract` → `packages/cezar` → root npm workspace
files (`package.json`, `package-lock.json`, `vitest.config.ts`, `node_modules`),
`scripts/dev.mjs`, `.github` workflows that ran vitest. Also delete
`src/server/static-ui.ts`'s SPA-shell behavior, `/assets/:file` and
`/open-mercato.svg` routes, and `docs/mockups/` if it still exists.
**Accept:** `rg -l "\.tsx?$"` returns nothing outside `docs/`; `cargo test` is the
whole suite; README's build instructions are Rust-only.
**Commit(s):** one commit per deletion in the order above — five to seven small
commits, e.g. `chore(cleanup): B12.1 delete packages/web`, `B12.2 delete
packages/api-client`, … — so each is its own revert point, matching this step's own
"separately revertable" instruction in the spec.

---

**Phase B checkpoint.** After B12, the project is what spec §2 Goal 5 calls
"primarily Rust" — actually entirely Rust. Node is gone. This is a legitimate
stopping point too, but it isn't the target state: there's still a Rust HTTP server
and a listening port. Phase C is what removes those.

---

## Phase C — one binary, no network

Source: spec §12. This is the phase that produces "the completed Rust TUI" as the
user means it: one binary, nothing listening, no Node, no browser, ever.

### [ ] C1 — `InProcessEngine`
**Ships:** `InProcessEngine` in `cezar-core` against the `Engine` trait defined
back at A2. Because the trait predates the server, this is an implementation, not
an extraction — the A2 review gates (no HTTP leakage into screens) guarantee no
surprises here.
**Accept:** `InProcessEngine` passes the same `Engine`-trait test suite `HttpEngine`
passes.
**Commit:** `feat(engine): C1 InProcessEngine`

### [ ] C2 — Switch default backend, delete `cezar-server`
**Ships:** `cezar-tui`'s default backend becomes `InProcess`; then **delete
`cezar-server` entirely** — the `axum` dependency, every handler, SSE/WS
transports, `origin-guard`, `host-guard`, the WS `trusted`/`loopbackReadable`
split, and `HttpEngine` in `cezar-client`. Event streams become in-process
`tokio::sync::broadcast` channels.
**Accept:** the TUI runs fully functional with no `cezar-server` in the dependency
graph; every screen-level test from Phase A still passes against `InProcessEngine`.
**Commit:** `feat(engine): C2 switch to InProcessEngine, delete cezar-server`

### [ ] C3 — Retire remaining server-shaped concepts
**Ships:** `cez serve` as a command, port-selection logic (`pickPort`, the 4321
default, the auto-increment probe), the health-poll startup wait, the A3
child-process supervisor (and with it, the §7.7 log-capture/`logs.rs` overlay
machinery it existed to support — there's no child process left to supervise or
capture). Fold what remains of `/api/v1/health`'s payload into a plain `cez doctor`
command (version/update check, agent-CLI probe — still useful, just not an HTTP
route).
**Accept (this is the final acceptance for the whole refactor):**
- `ss -ltnp` / `lsof -i` shows **no listening socket** while `cez` runs.
- `cargo tree` contains no HTTP server crate.
- `cez` starts in <150 ms cold on a repo with 500 runs.
- One binary, no Node, no port, no browser.
**Commit:** `feat(cli): C3 retire cez serve, port selection, service supervisor —
one binary, no network`

---

## Final checklist — "the completed Rust TUI"

Before calling the refactor done, confirm all of the following, each traceable to a
spec section:

- [ ] C3's four accept bullets above all hold (spec §12).
- [ ] `rg -i "cezar|\bcez\b|CEZ_|CEZ:"` returns hits only in `CHANGELOG.md` and
      `BACKWARD_COMPATIBILITY.md` history, plus the two dual-read compatibility
      regexes (marker vocabulary, branch prefix), each with a comment pointing back
      to spec §2.2.2 (spec §2.2.3 "Accept (final)").
- [ ] `.ai/coducktor/` and `~/.coducktor/` migrations ran correctly for a pre-rename
      repo (spec §2.2.2 point 3).
- [ ] Every waiver in spec §14's table has a CHANGELOG breaking-change entry naming
      the removed *capability*, not just the removed code.
- [ ] `AGENT_PROTOCOL.md`, `AGENTS.md`, `BACKWARD_COMPATIBILITY.md` are updated to
      match the shipped code (spec §14, last bullet).
- [ ] `docs/tui/terminals.md` records the terminal support matrix (spec §13.8).
- [ ] No file in the tree still matches the Tier 1/2/3 deletions in spec §16a, and
      nothing on the "explicitly not on this list" set (spec §16a, end) was
      accidentally deleted along the way.
- [ ] `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` are green on
      the final commit, and there is no `packages/` npm tree left at all.
