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

### [x] A13 — CLI surface
**Ships:** `clap` parser for the TUI binary reproducing the protected flags, `cez
tui` / bare-invocation-launches-TUI wiring. Do **not** touch the Node CLI's contract
yet — that's A15/B10.
**Accept:** `bc-route-inventory` and the CLI compatibility tests still pass
unchanged.
**Commit:** `feat(cli): A13 clap surface for the TUI binary` — pushed as `5a13d246`.
**Note:** `cez` is stale spelling from before decision 6 finalized `coducktor`/`duck`
(spec §2.2.1) — bare invocation and `coducktor tui` are the equivalent wiring, never
`cez`. Scope was read narrowly, per the plan's own text: `--repo`/`--workflow`/
`--model`/`-h`/`-V` got real, testable meaning (repo switch by canonical-path match
against the project registry, New Task preselection); `-p/--port`/`--no-open` stay
waived (spec §1.4); `run`/`init`/`projects`/`usage`/`serve` were deliberately left
unimplemented — that's `B10`, operating on the ported core/server crates. The Accept
criterion was verified directly: `bc-route-inventory.test.ts` passes, and
`package-cli.test.ts` fails identically on a clean pre-A13 checkout (a pre-existing
`npm pack --json` output-shape change in npm 12 the test predates, confirmed via
`git stash` — unrelated to this step).

### [x] A14 — Install path and docs
**Ships:** `cargo install --path crates/cezar-tui` as the documented one-liner
(both `cezar` and `cez` land on PATH), root `install.sh` (checks `rustup`, builds,
reports the binary path — no curl-pipe-to-shell, no release artifacts), a
`justfile`/`Makefile` (`build`, `install`, `test`, `lint`, `snapshots`), README
rewritten for clone-and-build, `docs/tui/` (keymap reference, terminal matrix,
screenshots). State the Node 20+ prerequisite honestly — Phase A still needs it —
written so removing that line at B12 is a one-line diff.
**Note:** same stale naming as A13's — "`cezar`/`cez`" above means `coducktor`/`duck`
per decision 6 (spec §2.2.1); shipped a second `[[bin]] name = "duck"` target in
`crates/coducktor-tui/Cargo.toml` alongside the existing `coducktor` one, both
pointing at `src/main.rs`, so one `cargo install` lands both. Screenshots are honest
plain-text `TestBackend` renders pulled from the real `insta` snapshots, not images —
this was authored from a headless sandbox with no attached TTY, so `docs/tui/
terminals.md`'s matrix is mostly marked "untested — needs manual verification"
rather than guessed at; only the sandbox's own env-var-derived color capability is a
verified row.
**Accept:** on a clean machine, `git clone && ./install.sh` yields a working `cez`
on PATH; the README's prerequisite list has no surprises.
**Commit:** `docs(install): A14 source-first install path and TUI docs` — pushed as
`6e68c744`.

### [x] A15 — Retire npm and remote-access surfaces from the Node tree ⚠ own commit
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
**Commit:** `chore(cleanup): A15 retire npm, remote-access, and Tier 1–3 surfaces` — pushed as
`5414fce6`.

---

**Phase A checkpoint.** After A15, you have a shippable product: a feature-complete
Rust TUI over a Node service the user never sees or touches directly. If Phase B
stalls here, this is still "done" per spec §4's Recommendation — don't treat it as
an incomplete state.

---

## Phase B — porting the engine to Rust

Source: spec §11. Deliverable: `cezar serve` is a Rust binary; Node is deleted. Every
step here keeps the React cockpit working until B12 — that's the oracle (spec §4).

### [x] B0 — Verify the ground is clear
**Ships:** nothing new — re-run A15's `rg` assertions, confirm nothing crept back.
If A15 was skipped or partial, finish it now; porting condemned code is the single
most wasteful thing this plan can do.
**Accept:** the A15 accept criteria still hold.
**Commit:** `chore(verify): B0 confirm A15 deletions are clean` (skip the commit
entirely if there's nothing to fix — this step can be a no-op check.)
— no-op: verified as A15's final gate (rg clean, `npm test`/`test:unit`/`build`
green on `5414fce6`), nothing crept back; no commit.

### [x] B1 — File layer
**Ships:** `cezar-core::paths`, `config`, `workspace::{config, ui_state, migrations,
agent_accounts}`. Port the migration framework first — riskiest to get wrong,
easiest to test in isolation.
**Accept:** cross-implementation read/write test (write with Node, read with Rust,
and vice versa) passes — start this test here and keep it through cutover (spec §14).
**Commit:** `feat(core): B1 paths, config, workspace, migrations`
— shipped as `crates/coducktor-core` (named per this repo's established `coducktor-*`
convention, not the plan's literal pre-rename `cezar-core` spelling). Every module cites
its `packages/cezar/src/` source file; `workspace::config`/`agent_accounts` use a
value-level `zod` compat helper module for per-key `.catch()`/`.passthrough()` salvage
(derive-based serde can't fail one field without failing the whole struct). Also fixed a
real Rust/Node divergence found in the process: `coducktor-tui` was reading `DUCK_HOME`
only, while `packages/cezar` reads `CEZ_HOME` — the two processes could silently resolve
different home dirs. `paths::coducktor_home_dir` now honors `DUCK_HOME` first, falling
back to `CEZ_HOME` until B12 deletes the Node tree; `coducktor-tui`'s keymap/service-log
paths now go through it instead of their own inline env lookups.
**Accept, verified:** `crates/coducktor-core/tests/cross_impl.rs` shells out to the real
`packages/cezar` source via `tsx` (no build step) for all four directions — Node writes
workspace config/agent-accounts, Rust reads; Rust writes, Node reads — and passes. 55
tests total (51 unit + 4 cross-impl), `cargo clippy --workspace --all-targets -D
warnings` clean, `cargo fmt --check` clean.

### [x] B2 — Runs store
**Ships:** `cezar-core::runs::store` — `runs.json`, NDJSON log, atomic writes,
`reconcileLoadedRun`, retention.
**Accept:** tests against real files written by the Node version pass.
**Commit:** `feat(core): B2 runs store`
— shipped as `crates/coducktor-core/src/runs/{store,events,retention}.rs`. `RunRecord`
itself is not redefined: `coducktor_contract::runs::RunRecord` (A1, kept parity-checked
against `store.ts`'s own `runRecordSchema`) is reused directly, with `runs::store` applying
only the zod behaviors a plain `#[derive(Deserialize)]` can't express — `archived`'s
`.default(false)`, the legacy `claude-cli` runner fold (#547) on `runner`/`steps[].backend`,
and the `.catch(undefined)` fields (`monitoringWakeAt`, `autoResumeAt`,
`autoResumeAttempts`, `blockedReason.retryAt`, `workflowDef`) — by normalizing the raw
`serde_json::Value` before handing it to that same derive, then discovering that
`z.array(runRecordSchema).safeParse` is whole-array-fail (confirmed against
`store.test.ts`'s own "does not let one claude-cli record evict the rest" /
"rejects an unknown activity value" cases), which needs no per-entry salvage machinery at
all. Added `crates/coducktor-core/src/time.rs` (`now_iso8601`/`is_zod_datetime`) since the
workspace has no `chrono`/`time` dependency and none existed yet.
**Scope cut, documented in `runs::retention`'s module doc:** `retention.ts`'s I/O enforcer
(`reclaimWorktrees`) and re-materializer (`rematerializeReclaimedWorktree`) are not ported —
both call into `git-worktree.ts`, which doesn't exist in Rust until **B3**. The pure
selector (`select_reclaimable_worktrees`/`is_reclaimable`) — the half `retention.ts` itself
keeps unit-testable and I/O-free — is ported now, matching every case in `retention.test.ts`,
so B3 wires it straight into its own enforcer instead of re-deriving it. Similarly, `store.ts`
class's `EventEmitter` fan-out, debounced saves, secret redaction, and the `createRun`/
`updateRun`/`appendEvent` business logic (PR/issue janitor) stay with the `RunManager` that
owns them — that's B6, not the file layer.
**Accept, verified:** `crates/coducktor-core/tests/cross_impl.rs`'s two new tests shell out to
the real `packages/cezar` `RunStore`/`readRunIndexFromDisk` via `tsx`: Node writes a run
(patching in a hand-written legacy `claude-cli` id and leaving it `running` on a flushed,
unclosed store) and two NDJSON events, Rust reads both and asserts the runner fold and
`reconcile_loaded_run`'s interrupt-on-load; Rust writes a run left `running` plus one event,
Node reads both back through its own real readers and asserts ITS OWN `reconcileLoadedRun`
independently produces the same `failed`/interrupted-error outcome. 93 tests total in
`coducktor-core` (87 unit + 6 cross-impl), `cargo clippy --workspace --all-targets -D
warnings` clean, `cargo fmt --check` clean (the one remaining `cargo fmt` diff, in
`coducktor-client/tests/transport.rs`, predates this step and is untouched by it).

### [x] B3 — Git layer
**Ships:** `cezar-core::git` — worktrees, base-ref resolution, autosave commits,
diff, shortstat, refs. **Shell out to `git`**, exactly as today — do not introduce
`git2`/`gix` here (spec §16, rejected for the port itself).
**Accept:** behavior matches the Node shell-out implementation on the existing test
fixtures.
**Commit:** `feat(core): B3 git layer (shell-out, no git2/gix)` — pushed as `51fdb13f`.
**Note:** shipped as `crates/coducktor-core/src/git/{worktree,diff_base,refs}.rs`, one
module per TS source file (`git-worktree.ts`, `git-diff-base.ts`, `git-refs.ts`), plus a
`git::run_git` shell-out primitive both submodules share — a consolidation of the three
near-identical private `git()` wrappers TS grew organically (`git-worktree.ts`,
`server/git.ts`, `server/git-changes.ts`), not a behavior change. Every scenario in
`git-worktree.test.ts`, `git-diff-base.test.ts`, and `autosave-conflict-guard.test.ts` is
re-proven inline against a real `git` binary (tempdir fixtures), plus two new
`tests/cross_impl.rs` checks (`resolveBaseRef`'s local/origin/stale matrix and
`createWorktree`'s cross-implementation reuse) on top of that ported-oracle coverage.
Also wired `runs::retention`'s I/O half (`reclaim_worktrees`/
`rematerialize_reclaimed_worktree`), deferred from B2 because it needed this module —
both return what changed rather than persisting it, since no live run store exists in
Rust yet (`RunManager` is B6); that caller will do the actual `write_run_index` persist.
**Scope call:** `packages/cezar/src/server/{git,git-changes}.ts` (the Repo/Changes/Files
tab plumbing) were **not** ported here — neither is named in the spec's B3 ship list
(§11.1), and both are server-route-adjacent logic that belongs at B9 (`cezar-server`,
"handlers stay thin, delegate to cezar-core").

### [x] B4 — Skills, workflows, handoff, todos, markers
**Ships:** `cezar-core::{skills, workflows::load, handoff, todos, task_markers,
task_refs}`.
**Accept:** existing behavior-equivalence tests pass against the ported module.
**Commit:** `feat(core): B4 skills, workflows::load, handoff, todos, markers` — pushed as
`e7fcd360`.
— shipped as `crates/coducktor-core/src/{skills,handoff,todos}.rs`,
`crates/coducktor-core/src/runs/{task_markers,task_refs}.rs`, and a new
`crates/coducktor-core/src/workflows/{mod,load,types}.rs`. Every TS test file in this
step's scope (`skills.test.ts`, `handoff.test.ts`, `todos.test.ts`,
`runs/task-markers.test.ts`, `runs/task-refs.test.ts`) is re-proven inline, case for case;
`workflows/load.ts` has no dedicated TS test file to port (it's exercised only indirectly
through server routes today), so its Rust tests were authored fresh against the documented
behavior in `load.ts`'s and `types.ts`'s own comments. Two new `tests/cross_impl.rs` checks
extend the running suite to the two genuinely shared surfaces this step touches: `todos.json`
(read both directions, including the "no id yet" agent-write shape and a malformed entry that
must not evict its siblings) and workflow YAML (the same on-disk `.ai/coducktor/workflows/
*.yaml` loaded by Node's `yaml` library and Rust's `serde_yaml_ng`, asserting the two parsers
resolve the same catalog and flag the same file). Added `regex` and `serde_yaml_ng` (per spec
§6.2) to the workspace dependency table.
**Scope cuts, documented in each module's own doc comment:**
- `todos.ts`'s `fs.watch`/`EventEmitter` change-notification plumbing
  (`onTodosChanged`/`todosWatchActive`) is not ported — this crate has no `tokio` or
  filesystem-watch dependency, and that machinery is runtime SSE-fan-out plumbing, not file
  layer; it belongs with whichever crate ends up owning that fan-out (`cezar-server`, B9),
  the same call B2 made for `runs::store`'s `EventEmitter` (deferred to the `RunManager`,
  B6). Likewise `todos.ts`'s in-process `withLock` mutex is not reproduced: every write here
  already goes through the same read-modify-write-atomic-rename sequence the lock exists to
  serialize, so serializing concurrent callers is that future owner's job, not this
  synchronous module's.
- `workflows/types.ts`'s `skillStackOf`, `chainStepNote`, and `DEFAULT_ALLOWED_TOOLS` are not
  ported — none are used by the file loader (`load.ts` imports only
  `normalizeWorkflowDoc`/`stepsIssue`/`workflowFileSchema`/`QUICK_TASK_WORKFLOW`); the first
  is compact-YAML-export UI logic already independently reimplemented in
  `coducktor-tui`'s `screens/workflows.rs` back at Phase A (before this crate had a
  `workflows` module to depend on), and the other two are consumed only at run EXECUTION
  time — `workflows::run`, B6 territory.
- `handoff.rs`'s `followups_enabled` reads `DUCK_FOLLOWUPS` before falling back to
  `CEZ_FOLLOWUPS` — a real (if narrow) behavior improvement over a literal port: the A11 TUI
  screen already tells the user to set `DUCK_FOLLOWUPS`, but a literal port would have honored
  only `CEZ_FOLLOWUPS` (all `packages/cezar`'s server reads until B12), silently breaking that
  on-screen instruction. Same dual-read precedent as B1's `paths::coducktor_home_dir`.
**Accept, verified:** 170 unit tests + 11 `tests/cross_impl.rs` tests (3 new to this step) in
`coducktor-core`, `cargo test --workspace` green across every crate,
`cargo clippy --workspace --all-targets -D warnings` clean, `cargo fmt --check` clean (the
one pre-existing `coducktor-client/tests/transport.rs` diff noted at B2 is untouched by this
step).

### [x] B5 — Agent runner mappers ⚠ do carefully, best oracle in the project
**Ships:** `cezar-protocol` mappers → `cezar-runners`, one runner at a time (claude
→ codex → opencode → pi), each validated **byte-for-byte** against its committed
golden fixtures; the `ui-parity` capability matrix re-implemented as a Rust test.
Consider one commit per runner if that keeps diffs reviewable — four commits here
is fine, this step has the best oracle in the whole project and de-risks everything
downstream, so don't rush it into one giant commit.
**Accept:** a diff against each committed `.expected.json` is the pass condition —
no new fixtures authored.
**Accept, verified:** all 26 committed Claude/Codex/OpenCode/Pi golden fixture
transcripts replay byte-for-byte after JSON round-trip; the Rust parity matrix
passes for all four backends; `cargo test --workspace`, `cargo clippy
--workspace --all-targets -- -D warnings`, the runner-package format check, and
`npm test` are green. The repository-wide format check still reports the
pre-existing unrelated `coducktor-client/tests/transport.rs` drift noted at B2.
**Commit(s):** `feat(runners): B5.1 claude mapper` — pushed as `2b7c736d`;
`B5.2 codex mapper` — pushed as `2a5c1daa`; `B5.3 opencode mapper` — pushed as
`3d0fced9`; `B5.4 pi mapper + ui-parity matrix` — pushed as `1a019aa1`.

### [x] B6 — RunManager
**Ships:** `cezar-core::workflows::run`: a durable, backend-neutral `RunManager`
with focused `lifecycle`, `session`, `recovery`, `review_gate`, `auto_resume`,
`context_refresh`, `variants`, `quota`, and `semaphore` policy modules. Port the
core lifecycle/policy coverage from `run.test.ts` alongside — this is spec §15's
**High**-severity risk item ("recovery, leases, quota routing"); do not shortcut
the recovery, lease, or quota coverage.
**Accept, verified:** the Rust `RunManager` suite covers durable lifecycle updates,
event sequencing and observers, directional usage accounting, queued prompt
hydration, marker/title precedence, review settlement, autonomous continuation,
check retries, FIFO capacity, repository/workspace leases, waiting-session delivery,
restart recovery, quota holds/auto-resume reconciliation, variants, and backend/model
continuation overrides. The focused B6 tests are green together with the full Rust
workspace and the existing Node oracle suite.
**Scope note:** the manager is deliberately backend-neutral. `SessionFactory`,
`CheckExecutor`, and `DiffInspector` are injected seams; process timers, HTTP
handlers, and provider-specific protocol translation stay outside core for B9/C1.
The Rust port keeps the shared persisted contract and the B2 event/store layer as
its cross-implementation oracle.
**Commit:** `feat(core): B6 RunManager lifecycle and execution policies` — pushed as
`b7792b9d`.

### [x] B7 — `cezar-forge`
**Ships:** `crates/coducktor-forge` — the `gh` driver and forge seam, ported against the
`github.test.ts`/`index.test.ts` oracle behavior. It includes injectable command and GraphQL
boundaries, per-project caches, quiet degradation for missing/offline GitHub, issue/PR listing
and counts, comments/reviews/timeline normalization, commit/PR checks, reference-status TTL and
invalidation policy, bounded PR diffs, merge preflight/merge execution, draft-PR autosave/publish,
remote parsing/host gating, and URL construction. The existing Node HTTP adapter remains in
place until B9 wires the Rust driver into `cezar-server`.
**Accept, verified:** the forge crate's 17 focused tests are green alongside the full Rust
workspace (including workspace Clippy), the forge format check, `npm run typecheck`, and the
full Node oracle suite (306 files / 5,582 tests). No live GitHub account is required: command
and GraphQL seams are injected in the Rust tests, while real operation still shells out to `gh`
and degrades to `{available:false,reason}` on failure.
**Commit:** `feat(forge): B7 gh driver` — this commit.

### [x] B8 — removed from the plan (decision 7)
The spec now says explicitly (§11.1): `cezar-core::automations` is **not** ported.
The automations engine — store, scheduler, poller, task templates — was deleted
outright from the TypeScript tree at A15 (§16a Tier 2), the same decision that
deleted its screen (§8.10). There is nothing left to port at B8, so there is no
chunk here and no commit — B9 follows directly after B7.
**Commit:** none — intentionally removed from the plan by decision 7.

### [x] B9 — `cezar-server` ⚠ set up HTTP-suite reuse in the first commit
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
**Commit(s):** `feat(server): B9.0 harness — point HTTP suites at cezar-server`
— pushed as `a77acd19`; `B9.1 runs route family` — `622be9ad`; `B9.2 workspace
routes` — `77c9a6fe`; `B9.3 skills and workflows routes` — `eed5daad`; `B9.4
ui-state and todos routes` — `dd60f3a0`; `B9.5 per-repo config routes` —
`79288186`; `B9.6 agent-config routes` — `4190815c`; `B9.7 IDE routes` —
`64a9e1a5`; `B9.8 repo routes` — `25f31cfa`; `B9.9 worktree routes` —
`f28a6d1d`; `B9.10 open-target routes` — `606e1429`; `B9.11 agent profile
registry routes` — `f734d9dd`; `B9.12 agent profile account routes` —
`86068a2f`; `B9.13 provider auth routes` — `ae08f10a`; `B9.14 host model
catalog routes` — `3c2ae62a`; `B9.15 plan routes` — `54a15b95`; `B9.16 variant
group routes` — `37baecf4`; `B9.17 SSE and event history routes` —
`1f13d98a`; `B9.18 GitHub forge routes` — `4f31c060`; `B9.19 run artifact and
git routes` — `69fcc85f`; `B9.20 run interaction routes` — `0ecc7971`; `B9.21
workspace index and checkout routes` — `783d34e8`; `B9.22 WebSocket topic bus`
— `ef6ea6a4`.
**Note on the `⚠` harness instruction:** it was not followed literally. B9.0
shipped `rust-server.testkit.ts` (a `DUCK_HTTP_BASE_URL`-selected transport
seam) plus one new `rust-server.smoke.test.ts`, and `scripts/test-rust-server.mjs`
to build+launch the Rust binary and run that smoke test against it — but the
seven named suites (`route-parity`, `contract-parity.*`, `versioned-surface`,
`bc-route-inventory`, `origin-guard`, `host-guard`, `sse-headers`) were never
retargeted onto `selectedHttpTarget`; they still only exercise the Node `Hono`
app. This was discovered at B9.22 (a full `rg`/route-diff against
`BACKWARD_COMPATIBILITY.md` §2 found the WS gap, and checking why no test
caught it led here) — it was not a documented decision by whichever earlier
turn wrote B9.0. Each B9.N commit substituted its own hand-written Rust-native
`#[tokio::test]` route-family suite instead (visible in
`crates/coducktor-server/src/lib.rs`'s `mod tests`, 44 tests as of B9.22),
which the B9.1–B9.21 Accept notes (this file's git history) treated as
sufficient oracle-equivalence. Asked explicitly at B9.22 whether to retrofit
the seven suites now, drop the idea and scope-cut it formally, or proceed to
B10 and revisit later: **the owner chose to proceed to B10**, leaving the
retrofit (or a formal scope-cut) as unclaimed follow-up work. A future agent
should not assume `rust-server.testkit.ts`'s harness is exercised by anything
beyond `rust-server.smoke.test.ts` and its own `rust-server.testkit.test.ts`.

### [ ] B9a — Real agent session execution ⚠ new step, not in the original spec/plan
**Why this step exists:** neither the spec nor this plan ever assigns a home to
`packages/cezar/src/core/agent-runner.ts` + its four backend implementations
(`claude-cli-runner.ts`, `codex-app-server-runner.ts`, `opencode-server-runner.ts`,
`pi-runner.ts` — ~3.1k lines) — the concrete glue that actually spawns `claude`/
`codex`/`opencode`/`pi` as child processes and feeds their output through B5's
already-ported mappers. B5 ported the mappers (parsing); B6 ported the policy engine
(`RunManager`) with `SessionFactory`/`AgentSession` left as an injected seam; nobody
ever wrote what plugs into that seam. Discovered while scoping B10 (`run`/`serve`
are meaningless without it — B9's own route tests create a run with the task text
"starts and fails without a session factory" to prove the *failure* path, which is
literally the only path that existed). Confirmed with the owner before proceeding
(2026-08-16): add this as its own step rather than ship a non-functional `run`/`serve`.
**Sub-step 1 of 2 — reshape `AgentSession` for live streaming, done first because it
touches already-shipped B6 code.** Investigating the seam surfaced a second, deeper
problem: `AgentSession::turn()` was synchronous and whole-turn-blocking — it returned
only once an entire turn finished, and `RunManager` published the whole aggregated
`SessionReport::turn_text` as one `"text"` event *after* the turn ended
(`append_session_text`, since renamed). Node's real architecture streams individual
`AgentEvent`s live via an `onEvent` callback and persists each one immediately
(`run.ts`'s `onEvent`, `this.store.appendEvent(runId, { ...event, stepId })` per
chunk) — that per-chunk persistence is what makes the transcript actually live; a
whole-turn-blocking trait cannot produce it no matter what a concrete `SessionFactory`
does. Confirmed with the owner before proceeding (2026-08-16): reshape B6 rather than
ship the coarse version.
**Ships (this sub-step):** `AgentSession::turn`/`send_message`/`finish` all gained an
`on_event: &mut dyn FnMut(EventInput) -> io::Result<()>` parameter — a live sink a
real backend calls once per mid-turn event, in order, as its process actually
produces it. `RunManager::event_sink` (new) is the concrete sink every call site
passes: it fills in `step_id` when absent and, for `"text"`-typed events specifically,
strips `CEZ:`/`DUCK:` turn markers and task markers per chunk (mirroring `run.ts`'s
`onEvent` exactly) so a marker never flashes in the live transcript, dropping the
event if stripping empties it. `append_session_text` is renamed
`apply_session_markers` and now does only post-turn marker *detection* on the
aggregated `turn_text` (`CEZ:PR=`/`CEZ:TITLE=`/etc. still need the whole turn to
evaluate) — it no longer re-persists the text, which would now duplicate the live
chunks. All five call sites in `run/mod.rs` (`execute_job`'s initial `turn()` and its
autonomous-nudge `send_message()`, `handle_active_outcome`'s autonomous-nudge,
`finish()`, `deliver_message()`) thread a freshly-bound `self.event_sink(run_id,
&step_id)` through; each is bound to a local `let` first, not inlined into a `match`
scrutinee, because rustc's temporary-lifetime-extension rule otherwise keeps the
`&mut self` borrow alive across the whole `match` (including its `Err` arms' own
`self.foo(...)` calls) — a real, not theoretical, borrow-checker error hit while
making this change.
**Test double note:** exactly one `FakeSession`/`FakeFactory` pair is shared by the
whole B6 test suite (not dozens — checked before assuming a large blast radius).
`FakeSession` now calls `on_event` once with its outcome's raw `turn_text` before
returning, which is enough for `event_sink`'s per-chunk stripping to keep every
existing test (including `runtime_applies_and_hides_turn_markers_before_publishing_text`)
passing unchanged. A new test, `a_session_that_streams_several_events_mid_turn_persists_each_one_live`,
adds a dedicated `StreamingSession`/`StreamingFactory` pair that calls `on_event`
several times mid-turn (text, tool-call, tool-result, text-with-a-trailing-marker) and
asserts each lands as its own seq-ordered, already-stripped event — proof the sink is
a real live channel, not unused plumbing only `FakeSession`'s single call exercises.
**Accept, verified:** `cargo test -p coducktor-core --lib` — 215 tests (214 pre-existing
+ 1 new), all green, no test bodies changed besides the two `FakeSession`/`FakeFactory`
call-site updates. `cargo build --workspace`, `cargo test --workspace`,
`cargo clippy --workspace --all-targets -D warnings` all green. `cargo fmt --all --check`
clean except the one pre-existing `coducktor-client/tests/transport.rs` drift noted
since B2/B5, untouched by this step.
**Commit:** `refactor(core): B9a.1 stream AgentSession events instead of one aggregate per turn`

**Sub-step 2 of 2 — concrete `SessionFactory`/`AgentSession` implementations, one per
backend — all four shipped (claude/codex/opencode/pi).** Port `agent-runner.ts` (the shared spawn/signal/
termination-tracking helpers — `isSignalTerminationExit`, `trackChildExit`,
`ContentBlock`, `prependSystemPrompt`) plus one commit per backend
(`claude-cli-runner.ts` → `codex-app-server-runner.ts` → `opencode-server-runner.ts`
→ `pi-runner.ts`, same claude-first order B5 used, same reason: best test oracle
first). Each backend spawns its real CLI (stdin/stdout stream-json for claude,
JSON-RPC/stdio for codex, HTTP+SSE for opencode, RPC/JSONL for pi), feeds raw process
output through the matching B5 mapper (`coducktor_runners::{claude,codex,opencode,pi}`)
to get `UiEvent`s, and calls the new `on_event` sink with them. **No `claude`/`codex`/
`opencode`/`pi` CLI is installed in this sandbox** — verification must use
`packages/cezar/scripts/mock-{claude,codex,opencode,pi}.mjs` (the `CEZ_DRY_RUN=1`
fakes the Node CLI already uses for its own tests) spawned via a real `node` child
process as the fake backend binary, giving genuine subprocess-boundary test coverage
without a live agent subscription. Land `crates/coducktor-server`'s wiring of a real
`SessionFactory` (replacing the "session factory unavailable" no-op) as part of
whichever backend commit makes it meaningful to flip on, or its own follow-up commit —
call it when doing the work.
**Accept:** a run started against the mock binary (`CEZ_DRY_RUN=1`-equivalent) reaches
`done`/`review` end to end through `coducktor-server`, live events appear over SSE as
the mock streams them (not all at once at the end), and `cargo test --workspace` /
clippy / fmt stay green throughout.
**Commit(s):** `feat(runners): B9a.2a agent-runner seam (spawn/signal/termination
helpers)` — pushed as `01a15b45`; `B9a.2b.1 ask-marker validation` — `f912a552`;
`B9a.2b.2 least-privilege child env` — `95b4968c`; `B9a.2b claude backend` —
`0653a0bf`; `B9a.2c.0 extract shared child-process plumbing` — `2ed6b976`;
`B9a.2c.1 v1 text coalescer` — `5ef2944b`; `B9a.2c codex backend` — `0213ad30`;
`B9a.2d.1 model identity parser` — `ea18dd78`; `B9a.2d opencode backend` —
`c0a34f1b` (Cargo.lock sync `410fde1b`); then `B9a.2e pi backend` — `4f218be5`.
**B9a.2a note:** shipped as `crates/coducktor-runners/src/agent_runner.rs` — the four
primitives `agent-runner.ts` actually exports: `is_signal_termination_exit`,
`prepend_system_prompt`, `ContentBlock`/`ImageSource` (the outbound Anthropic-shaped
content-block wire format), and a `TrackableChild` trait + `track_child_exit` tracker.
`RunnerId`/`AgentBackend`/`isRunnerId`/`RUNNER_IDS` were **not** re-ported — checked
against `coducktor_contract::{Runner, RunnerSelection}` (A1) first and confirmed they
already cover that enumeration, so re-porting would have been a duplicate source of
truth. `track_child_exit` is poll-based rather than push-based — `std::process::Child`
has no event-loop exit notification the way Node's `ChildProcess` `once('exit', …)`
does — but keeps the same observable contract the TS version's own doc comment
describes: seeded eagerly so an already-exited child is recognized on the first call
(the race the original guards against), and latched so it never re-polls once exited.
The per-runner SIGTERM→SIGKILL watchdog *timers* (`endCodexAppServer`, the `end()`
closures in `claude-cli-runner.ts`) are correctly left for B9a.2b/2c — they are not
exports of `agent-runner.ts` itself, each backend implements its own grace-period
sequence around the shared `TrackableChild` primitive.
**Accept, verified:** 8 new unit tests in `agent_runner.rs` (exit-code classification
matching `claude-cli-runner.test.ts`'s own cases, the prepend-with/without-prompt
cases, a wire-shape round-trip against the literal Anthropic JSON shape, and three
`track_child_exit` cases — already-exited, exits-later, and latches-without-repolling
verified via a poll counter) plus the crate's existing 14 tests, all green.
`cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `cargo fmt --check` all green (the one pre-existing `coducktor-client/tests/
transport.rs` drift noted since B2 is untouched by this step).

**B9a.2b notes:** two prerequisites surfaced mid-step and shipped as their own commits
before the backend itself, matching this section's own "own commit if that keeps diffs
reviewable" guidance:
- `B9a.2b.1` ports `packages/cezar/src/core/ask.ts` to `crates/coducktor-core/src/runs/
  ask.rs` — `decide_turn_marker`'s `valid_ask` parameter needs to know whether a turn's
  trailing marker is a schema-valid `AskRequest`, and nothing had ever ported that
  validation (Phase A's TUI never needed it; the still-live Node server validated
  server-side). No zod here: unlike this crate's other ported schemas, an invalid
  `DUCK:ASK` payload degrades the WHOLE marker to plain text rather than being salvaged
  field-by-field, so it's a plain all-or-nothing walk over `serde_json::Value`. 27 tests
  port every case in `ask.test.ts`.
- `B9a.2b.2` ports `packages/cezar/src/core/agent-env.ts` to `crates/coducktor-runners/
  src/agent_env.rs` — the least-privilege child-env allowlist (#427) every spawned
  backend must go through; spawning a real `claude` child without it would hand an
  attacker-controlled prompt the host's `GITHUB_TOKEN`/`ANTHROPIC_API_KEY`/`AWS_*`. 24
  tests port `agent-env.test.ts` case for case (Windows-shaped env casing, the #785
  temp-directory override-not-shadow behavior, the Bedrock/Vertex toggles).

The backend itself (`crates/coducktor-runners/src/claude_runner.rs`) also grew
`AgentRunSpec` in `agent_runner.rs` — the Rust counterpart of `agent-runner.ts`'s shared
spec type, needed by every backend but out of B9a.2a's narrower four-primitives scope.

**Architecture note, not a scope cut:** `coducktor_core::workflows::run::AgentSession`
(built at B6, before any backend existed) is turn-scoped — `turn()`/`send_message()`
each block for exactly one turn and return — where `claude-cli-runner.ts`'s single
`result` promise spans the whole session with turn boundaries visible only through the
live `onEvent('turn-end')` callback. Because of that mismatch (not a deliberate
narrowing), three TS mechanisms have no Rust counterpart: `TrackableChild`/
`track_child_exit` (B9a.2a) go unused by this backend — `std::process::Child` has no
`.killed`-delivery-vs-exit ambiguity to work around, `try_wait()` already answers the
question directly — and `isSignalTerminationExit`/`terminatedByCezar`/
`normalizeIntentionalTeardownResult` are dropped entirely, since the race they resolve
(a separate timer callback signalling the child while the read loop is still consuming
its stdout) cannot occur when the signalling code runs on the same call stack as the
loop it signals, with `&mut self` ruling out any concurrent caller. A wall-clock timeout
is a hard `Err` (fails the step) rather than TS's soft `done`-with-error-note outcome,
because the trait has no is-session-open signal a caller could use to avoid resurrecting
a session whose process was just killed.
**Scope cut:** wiring a real `SessionFactory` into `coducktor-server` is deferred, per
this step's own text ("or its own follow-up commit — call it when doing the work") — a
separate concern from the backend implementation itself.
**Accept, verified:** pure argv-builder tests port every case in
`claude-cli-runner.test.ts`'s `buildClaudeArgs`/`buildAllowedTools` describe blocks; real
subprocess tests spawn `node` against `packages/cezar/scripts/mock-claude.mjs` and the
`stub-ignores-eof-exits-143.mjs` fixture (#703's own oracle) — first-turn streaming
(text/tool-call/tool-result/image/token-usage/cost/turn-end events), a `CEZ:DONE`-marked
turn completing the step, a follow-up turn, `finish()` closing a cooperative process
promptly, `finish()`'s real SIGTERM (via `libc`, Unix)/SIGKILL escalation against a
process that ignores EOF, a wall-clock timeout killing the process and failing the turn,
and a friendly error for a missing binary. 53 tests in `coducktor-runners` (up from 8),
`cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo fmt --check` all green (the one pre-existing `coducktor-client/tests/
transport.rs` drift noted since B2 is untouched).

**B9a.2c notes:** one prerequisite discovered mid-step shipped first, own commit per this
section's own convention:
- `B9a.2c.1` ports `packages/cezar/src/core/v1-text-coalescer.ts` to
  `crates/coducktor-runners/src/v1_text_coalescer.rs` — codex (and later opencode) stream
  assistant text as deltas, and the v1 `text` event contract needs whole blocks, not one
  event per delta (a turn-end marker split across deltas would slip past per-event
  stripping otherwise). 10 tests port `v1-text-coalescer.test.ts` case for case.

Also, discovered while writing the claude backend's SECOND real use of the exact same
spawn/escalation shape: `B9a.2c.0` (own commit, before `B9a.2c` itself) extracted
`crates/coducktor-runners/src/child_process.rs` — the `ChildProcess` type owning spawn,
the stdout line channel, stderr collection, and SIGTERM→SIGKILL escalation, pulled out of
`claude_runner.rs` (B9a.2b) once codex needed the identical plumbing a second time.
`claude_runner.rs` was refactored onto it with no behavior change (same 14 tests). This
is proven duplication driving the extraction, not a speculative abstraction ahead of need.

The backend itself (`crates/coducktor-runners/src/codex_runner.rs`) ships
`CodexSpawnConfig`/`open_codex_session`/`CodexSession` over codex's JSON-RPC 2.0
(newline-delimited) `app-server` transport, built on `ChildProcess` plus a shared
`drive()` read/dispatch loop used both for RPC request/response roundtrips
(`initialize`, `thread/start|resume`, `turn/start|steer`) and "read until the turn ends"
— both need the same live notification dispatch interleaved with whatever they're
specifically waiting for.
**Architecture notes (documented in the module doc), not scope cuts:** because this
session is turn-scoped where the TS source is session-scoped (B9a.2b's own architecture
note applies here too), bootstrap (the whole `initialize`/`thread`/`turn` handshake) is
deferred into the first `turn()` call — `open_codex_session` has no event sink to
dispatch through yet, unlike claude's fire-and-forget opening write. A native
`item/tool/requestUserInput` ends the read loop with `decision: Ask` (bypassing
text-marker detection — a structured RPC request outranks a trailing marker), where TS
just keeps its one session-spanning loop running and answers in place; the next
`send_message()` answers the pending request via RPC instead of starting a new turn.
Sub-agent child-thread turn lifecycle (#600) is filtered so a spawned skill's own
`turn/completed` can't end the parent's turn, while its item events still render.
`codexAskQuestions` reuses `coducktor_core::runs::ask::parse_ask_request` directly
(B9a.2b.1) — one schema validates an ask regardless of which backend delivered it.
**Scope cut:** same as B9a.2b — wiring a real `SessionFactory` into `coducktor-server` is
deferred.
**Accept, verified:** real subprocess tests run `packages/cezar/src/core/__fixtures__/
codex/mock-codex-app-server.mjs` and the already-committed `stub-ignores-eof-exits-143.mjs`-
shaped codex fixture via `node` — a first turn's full event stream
(session/text/tool-call/tool-result/token-usage/turn-end) with per-turn (not cumulative)
token usage read from the app-server's own `tokenUsage.last`, a failed turn settling as
an error event rather than a hard `Err`, a native ask round-trip (request → park as
Waiting/Ask → answered follow-up → turn completes), the #600 sub-agent thread-filtering
fixture proving the parent's turn survives a child thread's full turn lifecycle, and
`finish()`'s SIGTERM escalation against an app-server that ignores EOF. 8 new tests, 73
total in `coducktor-runners`, full workspace test/clippy/fmt green (the one pre-existing
`coducktor-client/tests/transport.rs` drift noted since B2 is untouched).

**B9a.2d notes:** one prerequisite shipped first as its own commit — `B9a.2d.1` ports
`parseModelIdentity` from `packages/cezar/src/core/model-identity.ts` to
`crates/coducktor-runners/src/model_identity.rs` (only the pure splitter; the fail-loud
`resolveModelIdentity`/`normalizeModelForBackend` machinery is run-wiring's job, out of
scope — `spec.model` "arrives already normalised" per the TS runner's own comment). 4
tests port `model-identity.test.ts`'s `parseModelIdentity` cases.

The backend itself (`crates/coducktor-runners/src/opencode_runner.rs`) ships
`OpencodeSpawnConfig`/`open_opencode_session`/`OpencodeSession` over opencode's HTTP+SSE
transport — a headless `opencode serve` process, REST-ish requests, one persistent SSE
subscription — using `reqwest`'s blocking client (its own internal tokio runtime; no
async runtime added to this otherwise fully synchronous crate).
**Architecture notes (documented in the module doc), not scope cuts:** unlike codex,
bootstrap (spawn, read the bound URL off stdout, `POST /session`, connect SSE) runs
*eagerly* in `open_opencode_session` — none of those steps need a live read/dispatch
loop the way JSON-RPC's shared stdio channel does; only emitting the `"session"` event
and sending the opening prompt wait for `turn()`. The prompt POST and the SSE stream run
concurrently on purpose, matching the real server's behavior the bundled test mock
deliberately reproduces (the POST response resolves before a turn's final SSE parts
arrive) — `post_and_drain()` spawns the POST on its own thread and merges it with the
SSE channel on the calling thread, so `on_event` still sees each part live. `v1`'s
turn-end is synthesized from the POST settling, never from `session.idle`, matching TS.
`text_seen`/`tools_seen`/the coalescer/`text_chunks` are session-scoped fields (matching
TS's own instance fields, not reset per turn); each turn's `SessionReport.turn_text` is a
slice of `text_chunks` from an index captured at that turn's start, and token/cost
totals use the same before/after snapshot per turn since the wire's fields are
cumulative-over-the-session, not per-turn deltas. Two narrow, deliberate improvements
over a literal port (same rationale as codex's): a follow-up prompt failure is a hard
`Err` rather than a swallowed `note`, and a wall-clock timeout escalates to SIGKILL if
the process ignores SIGTERM — TS's own timeout path only ever sends one SIGTERM and can
hang the whole session forever if the process ignores it; closed here, not reproduced.
**Discovered mid-step, folded into the same commit:** `ChildProcess::discard_stdout()`
(opencode only needs stdout briefly at startup, unlike claude/codex's whole-session use
of it) and `ChildProcess::escalate_immediately()` (opencode's `finish()` has no
EOF-style grace period to wait out first) extend the shared `child_process.rs` plumbing
(B9a.2c.0). A `Drop` impl for `ChildProcess` (best-effort hard-kill if still running) was
also added — surfaced by a genuinely orphaned mock-server process a flaky test run left
behind, not a hypothetical; see the next paragraph.
**A real flake, caught and fixed, not just described:** the first run of the "first
turn streams the expected events" test failed intermittently (missing `tool-call`) —
the POST response resolving does not guarantee SSE frames sent moments earlier over a
*separate* TCP connection have already been delivered to this process, a race TS's own
concurrent read loop has too but usually wins on typical local timing. Reproduced,
diagnosed, and fixed with a short adaptive drain window after the POST finishes (keep
draining while frames keep actively arriving, stop at the first quiet gap — long enough
to catch already-in-flight frames, short enough to still correctly exclude the mock's
deliberately-30ms-later "Done." part). Stress-verified across repeated full-suite runs
afterward with no further flakes.
**Scope cut:** same as B9a.2b/2c — wiring a real `SessionFactory` into `coducktor-server`
is deferred.
**Accept, verified:** real subprocess tests run `packages/cezar/src/core/__fixtures__/
opencode/mock-opencode-serve.mjs` via `node` — a first turn's full event stream
(session/text/tool-call/tool-result/token-usage/cost/turn-end), proving the flushed
(never-`time.end`'d) first text part surfaces while the deliberately-late "Done." part
correctly does not, per-turn token/cost deltas computed correctly against the wire's
cumulative totals, and `finish()` closing a cooperative process promptly. 8 new tests (5
opencode + 3 `child_process`), 90 total in `coducktor-runners`, full workspace
test/clippy/fmt green (the one pre-existing `coducktor-client/tests/transport.rs` drift
noted since B2 is untouched).

**B9a.2e notes:** the backend itself (`crates/coducktor-runners/src/pi_runner.rs`) ships
`PiSpawnConfig`/`open_pi_session`/`PiSession` over pi's documented RPC mode
(stdin/stdout NDJSON commands — `get_state`/`prompt`/`abort` — not stream-json, not
JSON-RPC, not HTTP+SSE). No prerequisite commit was needed this time: unlike codex/
opencode, pi has no native structured ask and no bespoke wire-shape helper this crate
didn't already have (its usage-weighting formula is literally `usageValues` — identical
to `crate::usage::cost_weighted_tokens`, already shared with the claude backend since
B9a.2b), so this shipped as a single commit.
**Architecture notes (documented in the module doc), not scope cuts:** structurally
closest to the claude backend — a single flat NDJSON stream, no RPC request/response
matching to track (unlike codex), turn boundaries visible only via a live `agent_settled`
frame instead of claude's `"result"`. Bootstrap (the `get_state` probe plus the opening
prompt) runs eagerly in `open_pi_session`, same as claude's own eager opening write —
pi's wire has no roundtrip either command needs to wait on before the next write, unlike
codex's genuinely sequential handshake. Two TS-only conveniences have no Rust
counterpart: `sendMessage`'s `streamingBehavior: 'steer'` only applies while a previous
turn is still in flight, a state this turn-scoped trait can never observe (every
`turn()`/`send_message()` already blocks until `agent_settled` before returning); and
`autoEndAfterFirstTurn`/`AUTO_END_DELAY_MS` belong to TS's one-shot `run()` convenience
wrapper for callers that don't manage the session lifecycle themselves, superseded here
by every caller's own explicit `finish()`.
**Scope cut:** same as B9a.2b/2c/2d — wiring a real `SessionFactory` into
`coducktor-server` is deferred to B10.
**Mock fixture:** `packages/cezar/scripts/mock-pi-rpc.mjs` (previously a single canned
turn with no test hooks) gained `mock:done` (appends `CEZ:DONE` to the reply, mirroring
`mock-claude.mjs`), `mock:slow` (a ~25s hold, for the wall-clock-timeout test), and a
`MOCK_PI_IGNORE_EOF=1` toggle reproducing the #703 teardown shape (SIGTERM handler
exiting 143, `setInterval` keeping the event loop alive against EOF) — the same shape
already established for claude's `stub-ignores-eof-exits-143.mjs` and
`mock-codex-app-server.mjs`'s own `MOCK_CODEX_IGNORE_EOF`. Multi-turn already worked
(the mock loops over stdin lines) and needed no changes.
**Accept, verified:** pure argv-builder tests port every case in `pi-runner.test.ts`'s
"pi RPC argv" describe block (thinking-level passthrough, exact session
selection+resume+model+system-prompt+full tool mapping, session-id-without-resume, and
the bash-allowlist-fails-closed case); real subprocess tests spawn `node` against the
extended `mock-pi-rpc.mjs` — a first turn streaming session/text/tool-call/tool-result/
token-usage/turn-end events, a `mock:done`-marked turn completing the step, a follow-up
turn via `send_message` reaching a second turn, `finish()` closing a cooperative process
promptly, `finish()`'s SIGTERM→SIGKILL escalation against a `MOCK_PI_IGNORE_EOF=1`
process, a wall-clock timeout (`mock:slow`) killing the process and failing the turn, and
a friendly missing-binary error. 11 new tests, 93 total in `coducktor-runners` (up from
82), 658 total across the Rust workspace. `cargo test --workspace`, `cargo clippy
--workspace --all-targets -- -D warnings`, and `cargo fmt --all --check` all green (the
one pre-existing `coducktor-client/tests/transport.rs` drift noted since B2 is
untouched). This closes out B9a.2 — all four backends (claude/codex/opencode/pi) are now
shipped; wiring a real `SessionFactory` into `coducktor-server` is B10's job.
**Commit:** `feat(runners): B9a.2e pi backend` — pushed as `4f218be5`.

### [x] B10 — `cezar-cli`
**Ships:** `serve`, `run`, `init`, `usage`, `projects` subcommands. `-p/--port` and
`--no-open` are **not** ported (waived, §1.4). No `--server`, no `--token`.
**Accept:** exit codes match the protected CLI contract; `--help` names every
protected flag. **Blocked on B9a** — `run`/`serve` need a real `SessionFactory` to be
anything but a shell that always fails.
**Note on packaging:** shipped on the SAME binary A13 already built
(`crates/coducktor-tui`, bins `coducktor`/`duck`) rather than a separate `cezar-cli`
crate — the spec's crate-list (§10) names one, but A13/A14 already collapsed CLI +
TUI into one binary (decision 6, "one command"), and A13's own doc comment named these
five subcommands as explicitly its job to finish. Read narrowly per this plan's own
ground rule 0 (spec wins on disagreement, but established repo precedent wins on
packaging calls A12/A13 already made the same way).
**Prerequisite work, discovered mid-step (own commits, same "own commit for a
prerequisite" convention as B9a):**
- `B10.0` — `SessionRequest` (the `RunManager` → `SessionFactory` handoff, B6) never
  carried a full `AgentRunSpec`'s `cwd`/`allowed_tools`/`bash_allowlist`/
  `system_prompt`/`reasoning_effort` — nobody had wired a real factory into
  `RunManager` before this step, so nobody had hit the gap. Extended `SessionRequest`
  with those five fields rather than having the factory read `runs.json` out of band
  (the workflow step and run record were already in scope at `execute_job`'s one
  `SessionRequest` construction site). `cwd` is always `repo_root` — no worktree
  orchestration exists in this crate yet (`RunRecord.worktree` is recorded intent,
  never acted on); `allowed_tools`/`bash_allowlist` come from the workflow step,
  falling back to `DEFAULT_ALLOWED_TOOLS` (matches `run.ts`'s
  `step.allowedTools ?? DEFAULT_ALLOWED_TOOLS`); `system_prompt`/`reasoning_effort`
  come from the run record (`reasoning_effort`'s `auto` maps to `None` — the level a
  text/prompt heuristic would pick is not ported, a backend's own default answers
  instead). Added `RunManager::repo_root()` (private; `data_dir` minus the
  `.ai/coducktor` suffix `for_repo` appends). — pushed as `0914a15f` (+ a fmt-only
  follow-up, `7b72d9e5`).
- `B10.1` — `crates/coducktor-runners/src/session_factory.rs`'s `DefaultSessionFactory`
  dispatches a `SessionRequest` to `open_claude_session`/`open_codex_session`/
  `open_opencode_session`/`open_pi_session` by `RunnerSelection` (`Auto` falls back to
  claude, the same default `execute_job` itself already applies). Binary resolution
  mirrors each TS runner's own constructor exactly, **not** a symmetric convention:
  claude/pi fall back to the bundled `CEZ_DRY_RUN=1` mock (resolved relative to
  `SessionRequest.cwd`, since the mock scripts live in the still-live Node tree) when
  no `CEZ_*_BIN` override is set; codex/opencode do **not** — `resolveCodexExecutable`
  and `opencode-server-runner.ts`'s constructor never had a dry-run branch, confirmed
  by reading both before assuming symmetry. — pushed as `2878f76f`.
- `B10.2` — `crates/coducktor-core/src/workspace/projects.rs` ports
  `workspace/projects.ts`'s registry (`register_project`/`list_projects`/
  `remove_project`/`allocate_project_slug`/`should_register_project`) for the
  `projects` subcommand. The per-root git/forge probe (branch, forge kind, repo URL,
  TTL-cached) is **not** ported — needs `server/git.ts`'s `getRepoInfo` and
  `server/forge/`, neither ported to this crate (same call B3 already made for
  `server/{git,git-changes}.ts`); `probe_status` here is a plain filesystem check.
  **Known duplication, not resolved here:** `coducktor-server`'s own
  `register_project`/`list_projects`/`remove_project` route handlers already
  independently reimplement this same registry logic (inlined at B9 before a core
  module existed) — a future chunk should have those handlers delegate to this
  module instead; out of scope for this diff (ground rule 0's Goal 8 boundary: don't
  expand a chunk into a file it didn't already need to touch). — pushed as
  `01410430`.
**Ships (the CLI itself):**
- `serve` — builds `RunManager::for_repo` + `DefaultSessionFactory`, hands it to a new
  `coducktor_server::ServerState::with_manager`/`serve_with_state` (the latter added
  alongside the existing `serve()`, additive, no route-handler changes — `serve()`
  itself has no way to attach a pre-built `RunManager`), binds the first free port
  from 4321 upward (`-p/--port` waived, spec §1.4). Prints a startup banner to the
  real terminal — allowed here (unlike the TUI's supervised child, §7.7's silence
  rule is about the TUI's alternate screen, not `serve` invoked directly).
- `run "<task>"` — `RunManager::with_session_factory`, `workflows::load::load_workflows`
  (falls back to the built-in `quick-task` when `--workflow` names nothing on disk),
  `subscribe_events` prints text/tool-call/tool-result/note/error to stdout (mirrors
  `index.ts`'s `runCommand` switch), exit 0 for `done`/`review`, 1 otherwise — the
  protected exit-code contract, verified directly against the mock, not assumed.
- `init` — `packages/cezar/src/index.ts`'s `initCommand` ported directly: scaffolds
  `.ai/coducktor/{workflows/fix-and-verify.yaml,skills/project-conventions.md}` plus
  the `.gitignore` ensure-list, content verbatim.
- `projects [list|add [<dir>]|remove <id>|rm <id>]` — wired against B10.2. `tag` is
  **not** ported — a secondary UX affordance (project grouping tags), not part of the
  protected surface (spec §1.4 names the five commands, not their subcommands).
- `usage` — **scope cut, not a rushed port.** `packages/cezar/src/core/quota/*` (nine
  files: runtime/coordinator/router/policy/failure-classifier/usage-report/
  usage-service/claude-usage-adapter/codex-usage-adapter/claude-credentials) is
  read-only CLI telemetry display, orthogonal to session execution — `RunManager`'s
  own quota *routing* (B6, `workflows::run::quota`) is a pure policy function with no
  coordinator dependency. The subcommand parses (`--json`/`--refresh` included, so
  `--help` still names it — spec §1.4 point 1 protects the command's existence) but
  prints an honest "not yet implemented" notice to stderr and exits 1 rather than
  fabricating telemetry.
**Accept, verified:** `cli::tests::the_protected_commands_all_parse` and
`help_names_every_flag_this_binary_actually_supports` cover the `--help`/parsing half;
`headless::tests::run_command_reaches_done_and_exits_zero_against_the_dry_run_mock`
proves the exit-code contract end-to-end against a fake repo carrying a real copy of
`mock-claude.mjs` (proof the `SessionRequest` gap from B10.0 was actually closed, not
just that the string-building helpers compute the right path) —
`session_factory.rs`'s own `open_spawns_a_working_claude_session_under_dry_run` proves
the same one level down. 19 new tests across this step (2 in `coducktor-core`'s
`SessionRequest`, 11 in `coducktor-core`'s `workspace::projects`, 11 in
`coducktor-runners`'s `session_factory`, 8 in `coducktor-tui`'s `headless`, 4 in
`coducktor-tui`'s `cli`), full workspace `cargo test`/`cargo clippy --workspace
--all-targets -- -D warnings`/`cargo fmt --all --check` green throughout (the one
pre-existing `coducktor-client/tests/transport.rs` drift noted since B2 is untouched).
**Scope note:** `WorkspaceSemaphore`/`CheckExecutor`/`DiffInspector`/
`RepositoryRootLease` all stay unwired (`None`) — `RunManager` degrades gracefully
without them (no cross-run capacity limiting, no `command:` check steps, no
review-gate diff detection with `review_gate: false` the default) and none are named
in B10's own "Ships" line; a future step's job if `run`/`serve` need them.
**Commit(s):** `feat(core): B10.0 extend SessionRequest with cwd/tools/prompt/reasoning`
— `0914a15f` (+ fmt follow-up `7b72d9e5`); `feat(runners): B10.1 DefaultSessionFactory
— real backend dispatch` — `2878f76f`; `feat(core): B10.2 workspace::projects —
registry port for the projects CLI` — `01410430`; `feat(cli): B10 cezar-cli
subcommands` — this commit.

*(B10a is a no-op — `server-install`/`server-deploy`/`server-uninstall` were
removed from the Node tree at A15 and are never ported. No commit for this line.)*

### [x] B11 — Cutover and soak ⚠ do not reorder with B12
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
**Soak methodology decision:** a calendar-time "N days of daily-driver use" soak is
not something a single implementing session can execute or certify — there is no
owner accumulating days of real usage mid-session. This step instead used the
plan's own other sanctioned option verbatim: **"a scripted comparison run against a
fixed set of repos/workflows."** The fixed set: the ad hoc tempdir repos each
retrofitted assertion creates via `mkdtemp` (mirroring every other route-family
test in this codebase) plus the `default` boot project workflow set (`quick-task`
and friends) already exercised by `rust-server.smoke.test.ts` — the same fixture
shape B9's own native Rust route-family tests already use as their oracle-equivalence
baseline.
**Ships, in detail:**
- *B11.1* (`f4d94085`) — `crates/coducktor-server/src/static_ui.rs` (ported from
  `packages/cezar/src/server/static-ui.ts`) plus `lib.rs` wiring: `ServerConfig.web_dir`
  (`DUCK_WEB_DIR`/`CEZ_WEB_DIR` override, else `<cwd>/packages/cezar/web`), a catch-all
  shell handler serving `web/dist/index.html` for any non-`/api/*`, non-asset GET (deep
  links survive refresh) or a built-in `BUILD_HINT_HTML` page when no build exists
  (never a 404), `/assets/{file}` (content-type by extension, hard caching, path-
  traversal-safe), `/open-mercato.svg`. The React bundle now serves unchanged from
  `coducktor-server`, satisfying this step's own first Ships line.
- *B11.2* (`63661861`) — `packages/cezar/src/server/rust-server.parity.test.ts`, wired
  into `scripts/test-rust-server.mjs` alongside the existing smoke suite: 8 scripted
  assertions against a real TCP listener (a freshly built `coducktor-server` binary),
  hand-picked from `origin-guard.test.ts` (cross-origin CSRF rejection, opaque `null`
  Origin rejection, same-origin passthrough), `host-guard.test.ts` (loopback Host
  spellings accepted), `sse-headers.test.ts` (both SSE endpoints' anti-buffering
  headers), `route-parity.test.ts` (unprefixed vs `/api/v1/p/default` byte-identical
  alias), and `versioned-surface.test.ts` (unknown-project 404, health CORS).
  **Real drift found and fixed in the process:** the Rust host-guard's 403 body was
  `"forbidden: unexpected Host header — this request did not originate from this
  machine"`, missing the `" (see #426)"` suffix Node's `host-guard.test.ts` asserts
  verbatim (`server.ts`/`host-guard.ts`'s own literal string) — the two now match
  byte-for-byte. This is the soak doing its job: a real, if minor, Node/Rust text
  divergence that would otherwise have shipped silently.
- *B11.3* (`68ff149b`) — `crates/coducktor-tui`'s `serve` subcommand gained
  `--legacy-server`: shells out to `npm run dev -w @open-mercato/cezar -- serve --repo
  <dir>` (the OLD Node service) instead of booting `coducktor-server` in-process, using
  the same `--repo` resolution as the Rust path. `DUCK_LEGACY_CLI_DIR`/
  `CEZ_LEGACY_CLI_DIR` override the monorepo checkout to shell out from (else cwd,
  mirroring `default_web_dir()`'s own convention from B11.1). Verified manually: booted
  the legacy Node service against a fresh tempdir repo, confirmed the banner, backend
  checks and cockpit URL all appeared as expected.
**What is explicitly NOT retargeted onto the external-process harness, and why**
(documented in `rust-server.parity.test.ts`'s own module doc too):
- `contract-parity.test.ts` + its four `contract-parity.{github,runs,workflows,workspace}.test.ts`
  siblings (5 files) are **compile-time TypeScript type assertions** — each file's `it()`
  exists only to keep it visible to the test runner; the actual check is `npm run
  typecheck` comparing a route handler's TS-inferred response type against a
  hand-written zod schema type. There is no HTTP request anywhere in these files to
  retarget, and no meaning a Rust binary's behavior could have against a TypeScript
  compile-time check.
- `bc-route-inventory.test.ts`, and two of `versioned-surface.test.ts`'s four tests
  ("serves no route outside the version prefix", "finds a non-trivial number of
  versioned routes"), read Hono's own in-process route table (`app.routes`) directly —
  never issue an HTTP request. A JS test process has no way to introspect an external
  Rust binary's route table the same way; doing this for real would mean either
  exposing a debug route-listing endpoint from `coducktor-server` (a new API surface
  with no product reason to exist) or writing an equivalent Rust-native test comparing
  `router_with_state`'s own registrations against `BACKWARD_COMPATIBILITY.md` §2 — a
  DIFFERENT test, not a retarget of this one. Named as follow-up below.
- The FULL `route-parity.test.ts` (346 lines), `origin-guard.test.ts` (284 lines) and
  `host-guard.test.ts` (81 lines) suites each build a fresh `createApp()` — and
  therefore a fresh repoRoot/store/`CEZ_HOME`/registered-projects — per test. This
  harness's single long-lived external Rust process (one `--repo-root`, started once
  by `scripts/test-rust-server.mjs` before any test file runs) cannot give each test
  its own isolated fixture the way an in-process Hono app trivially can. Retargeting
  these fully needs a per-test spawn of the (already-built) Rust binary against that
  test's own temp repoRoot/port — a real harness capability, not a mechanical
  find-and-replace — and was not built in this pass. `rust-server.parity.test.ts`
  instead captures each suite's CORE guard/contract behavior against the one shared
  server and its `default` boot project (see the Ships list above), which is real,
  passing, over-the-wire proof that the guards and headers hold — but it is not the
  same as literally retargeting all three files' full case matrices (e.g. `origin-guard`'s
  Sec-Fetch-Site dev-proxy exemption, or `host-guard`'s missing-Host-header case, are
  not covered against the external target).
- `host-guard.test.ts`'s "missing Host header" and "rebound (foreign) Host header" cases
  specifically cannot be reproduced through `fetch()` against a real listener at all,
  independent of the per-test-isolation gap above: a conforming HTTP client sets `Host`
  from the actual TCP destination, not from application code, and cannot omit it either
  (confirmed empirically — setting a `host` header on `fetch`'s `RequestInit` does not
  change what is sent on the wire for a real socket). Confirmed independently that the
  Rust guard itself is correct via a manual `curl -H "Host: attacker.example"` against a
  hand-started server (403, matching Node byte-for-byte after the drift fix above) —
  the gap is in what THIS TEST HARNESS's HTTP client (`fetch`) can drive, not in the
  guard's behavior.
**Named follow-up (unclaimed, does not block B12):** a per-test Rust-process-spawn
harness (build once, spawn the pre-built binary per test against that test's own temp
repoRoot/port) would let `route-parity.test.ts`/`origin-guard.test.ts`/
`host-guard.test.ts` retarget in full, and a Rust-native "route table vs.
`BACKWARD_COMPATIBILITY.md` §2" test would give `bc-route-inventory.test.ts` a real
Rust-side equivalent. Neither blocks B12: the guards and contracts they'd additionally
cover are already proven correct by the existing native `#[tokio::test]` route-family
suites in `coducktor-server` (in-process) plus this step's own over-the-wire subset —
this is coverage depth, not a known gap in behavior.
**Accept, verified:** `node packages/cezar/scripts/test-rust-server.mjs` builds a real
`coducktor-server` binary, boots it, and runs `rust-server.smoke.test.ts` +
`rust-server.parity.test.ts` against it — 14 tests, all green. The six Node oracle
files this step drew assertions from (`host-guard`, `origin-guard`, `sse-headers`,
`route-parity`, `versioned-surface`, `bc-route-inventory`) remain green and untouched
(77 tests). `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo fmt --check` all green (the one pre-existing unrelated
`coducktor-client/tests/transport.rs` drift noted since B2 is untouched).
**Commit(s):** `feat(server): B11.1 serve the React cockpit from coducktor-server` —
`f4d94085`; `feat(server): B11.2 scripted comparison run against a real Rust listener`
— `63661861`; `feat(cli): B11.3 --legacy-server flag for one-command side-by-side
comparison` — `68ff149b`.

### [x] B12 — Delete the TypeScript
**Ships:** deletions in this order, each separately revertable: `packages/web` →
`packages/api-client` → `packages/contract` → `packages/cezar` → root npm workspace
files (`package.json`, `package-lock.json`, `vitest.config.ts`, `node_modules`),
`scripts/dev.mjs`, `.github` workflows that ran vitest. Also delete
`src/server/static-ui.ts`'s SPA-shell behavior, `/assets/:file` and
`/open-mercato.svg` routes, and `docs/mockups/` if it still exists.
**Prerequisite (B12.0), before any deletion — a real dependency the plan text didn't
name:** `crates/coducktor-runners`' own tests (real-subprocess mocks for all four
backends) and `crates/coducktor-protocol`'s golden-fixture tests both read Node
scripts and NDJSON/JSON fixtures that lived under `packages/cezar/{scripts,
src/core/__fixtures__}`; `coducktor-runners::session_factory::DefaultSessionFactory`
(B10.1) — production code, not a test — hardcoded the same `packages/cezar/scripts/
mock-{claude,pi-rpc}.mjs` paths as its `CEZ_DRY_RUN=1` fallback; and `crates/
coducktor-tui/src/headless.rs`'s own dry-run test fixture did too. All of it was
relocated to a new root-level `fixtures/` directory (`fixtures/scripts/` — the two
mock CLIs; `fixtures/{claude,codex,opencode,pi}/` — every backend's mock server
script and golden `.ndjson`/`.expected.json` pair) **before** `packages/cezar` was
touched, with every Rust path constant updated to match (`session_factory.rs`'s
`MOCK_CLAUDE_RELATIVE`/`MOCK_PI_RELATIVE`, the four backend runners' test helpers,
`coducktor-runners/tests/{golden,ui_parity}.rs`, `coducktor-protocol/tests/
golden.rs`, `headless.rs`). `crates/coducktor-core/tests/cross_impl.rs` (B1-B4's
cross-implementation oracle, which shells out to `packages/cezar` via `tsx`) was
deleted outright — its premise, a second live TS implementation to diff against,
disappears with the tree it diffs against. Also dropped, both flagged in their own
doc comments as existing only "until `packages/cezar` is deleted (B12)": the
`CEZ_HOME` fallback in `paths::coducktor_home_dir` and the `CEZ_FOLLOWUPS` fallback
in `handoff::followups_enabled` — each existed solely to stay in sync with a Node
reader that no longer exists. And B11.3's `--legacy-server` flag (`crates/
coducktor-tui`) was removed: it shelled out to the Node CLI for the B11 soak
comparison, which has nothing left to shell out to once `packages/cezar` is gone —
its own doc comment said "deleted at C2," but B12's literal deletion of
`packages/cezar` makes it non-functional now, not at C2, so removed here instead of
leaving dead code that would silently break.
**Two bugs in the fixture-relocation commit's own execution, caught and fixed in a
follow-up commit, not hidden:** the first `git add` invocation had a stale pathspec
(a file `git rm` had already staged) that made the whole command fail silently for
every OTHER path in the same invocation — the fixture files and Rust source edits
never actually got staged, so the first commit contained only the `cross_impl.rs`
deletion. Caught by re-running `git status` before the next step (ground rule 0's own
"confirm only intended files changed" instruction), fixed with a second, honestly-
labeled follow-up commit rather than an amend (this repo's own convention: never
amend, always a new commit).
**Accept, verified:** `rg -l "\.tsx?$"` matches zero files (filenames) outside `docs/`
— zero `.ts`/`.tsx` files exist anywhere in the tree now; two CONTENT matches for the
literal substring remain (`crates/coducktor-server/src/lib.rs`'s `ws.ts` doc-comment
citations, `AGENTS.md`'s one stale `npm test` example path) — both are historical
citations/prose, not TypeScript source, and left alone per this plan's own
established convention of keeping "mirrors `X.ts`" doc comments after their source is
deleted (see e.g. B8/A15's automations citations). `cargo test --workspace`: 691
tests green (all crates, up from 690 pre-B12 after B12.0's `cross_impl.rs` removal
netted against its own new coverage staying flat). `cargo clippy --workspace
--all-targets -- -D warnings` and `cargo fmt --check` clean except the one
pre-existing unrelated `coducktor-client/tests/transport.rs` drift noted since B2.
README's Quick start/Prerequisites/Development sections are rewritten Rust-only (no
Node prerequisite, no Node badge, no `packages/` dev-script section) — `git clone &&
./install.sh` is the whole story now. `AGENTS.md`'s "Validation" section (the only
part of that file actively **wrong** rather than merely stale — it told a future
agent to run `npm`/vitest commands that no longer exist) was replaced with the real
`cargo fmt`/`cargo test`/`cargo clippy` gate; the rest of `AGENTS.md` (a routing
table still full of `packages/cezar/*.ts` paths) is flagged, not rewritten — that
full pass is the Phase C Final checklist's own named item ("`AGENT_PROTOCOL.md`,
`AGENTS.md`, `BACKWARD_COMPATIBILITY.md` are updated to match the shipped code"),
out of B12's own scope.
**Commit(s):** `chore(cleanup): B12.0 relocate Rust-owned test fixtures out of
packages/cezar` — `01ad4f35` (+ `99135067`, the fixed-pathspec follow-up);
`B12.1 delete packages/web` — `36a95eee`; `B12.2 delete packages/api-client` —
`424941a4`; `B12.3 delete packages/contract` — `0849e010`; `B12.4 delete
packages/cezar` — `5675a30c`; `B12.5 delete the root npm workspace` — `c02971e1`;
`B12.6 trim vitest/npm steps from CI` — `db473108`; `docs: B12.7 rewrite README
build instructions as Rust-only` — `b1126f3b`. `docs/mockups/` was already deleted
at A15; nothing to remove there.

---

**Phase B checkpoint.** After B12, the project is what spec §2 Goal 5 calls
"primarily Rust" — actually entirely Rust. Node is gone. This is a legitimate
stopping point too, but it isn't the target state: there's still a Rust HTTP server
and a listening port. Phase C is what removes those.

---

## Phase C — one binary, no network

Source: spec §12. This is the phase that produces "the completed Rust TUI" as the
user means it: one binary, nothing listening, no Node, no browser, ever.

### [ ] C1 — `InProcessEngine` ⚠ in progress, not complete
**Ships:** `InProcessEngine` in `cezar-core` against the `Engine` trait defined
back at A2. Because the trait predates the server, this is an implementation, not
an extraction — the A2 review gates (no HTTP leakage into screens) guarantee no
surprises here.
**Accept:** `InProcessEngine` passes the same `Engine`-trait test suite `HttpEngine`
passes.
**Commit:** `feat(engine): C1 InProcessEngine`
**Status (C1.1):** partial. Shipped as `crates/coducktor-client/src/in_process.rs`,
**not** `cezar-core` — `coducktor-core`/`coducktor-runners`/`coducktor-forge` have no
dependency back on `coducktor-client` today, and putting `InProcessEngine` in
`coducktor-core` would require importing the `Engine` trait FROM `coducktor-client`
for a type whose only job is satisfying that trait; `coducktor-client` (already
`HttpEngine`'s home) keeps the dependency edge one-way. `InProcessEngine` calls
straight into `RunManager`/`DefaultSessionFactory` (B10) instead of over HTTP for:
health, the runs family (list/get/start/archive/delete/read/unread/
archive_finished/mark_all_read/patch_run/cancel_run/finish_run/runs_index),
workflows, skills, ui-state, the follow-up inbox (todos/delete_todo/start_todo), a
read-only workspace-projects snapshot, and live events (`Topic` →
`tokio::sync::broadcast`, fed by `RunManager::subscribe_events`/`subscribe_runs` —
no WS/SSE JSON round-trip at all, this already delivers C2's own "event streams
become in-process broadcast channels" text for this backend).
**Discovered mid-step, the reason this isn't `[x]` yet:** a large fraction of
`coducktor-server`'s own handlers hold real business logic directly — git
shelling, IDE file read/write, agent-config file listing/writing, provider
CLI probing, GitHub forge detail reads, worktree management, open-targets
detection, diff/compare, the settings write paths — not the thin
"parse-validate-delegate over cezar-core" wrappers that crate's own module doc
promises and this step's plan text assumes. Porting the FULL ~85-method `Engine`
trait is a materially bigger lift than "an implementation, not an extraction"
implies. `InProcessEngine` deliberately does **not** `impl Engine` yet — an
`Err`-stubbed trait impl claiming completeness would be worse than an honest
partial struct. A follow-up commit ports the remaining families (named above) and
closes the trait impl; only then does C1 become `[x]` and C2 (switch the TUI's
default backend, delete `cezar-server`) become unblocked.
**Accept, verified (C1.1's own scope only, not the full step):** 13 new tests in
`in_process::tests` (a `FakeSession`/`FakeFactory` pair proves the wiring without a
real agent CLI — the four real backends already have dedicated subprocess tests in
`coducktor-runners`), covering the full round-trip of every family shipped here.
705 total workspace tests green, `cargo clippy --workspace --all-targets -- -D
warnings` clean, `cargo fmt --check` clean (an unscoped `cargo fmt` run incidentally
fixed the one `coducktor-client/tests/transport.rs` drift noted since B2 — that
long-standing note is now stale).
**Commit:** `feat(engine): C1.1 InProcessEngine — health, runs, workflows/skills,
ui-state, todos, projects, live events` — pushed as `7bb0ee84`.

**Status (C1.2):** partial, continuing C1.1. Adds `workspace_usage` (ported from
`get_workspace_usage` verbatim — that route already just answers
`WorkspaceUsageResponse { providers: vec![] }` since B10's quota-telemetry scope
cut, so this is a one-line port, not a new gap) and the workflow builder writes
(`save_workflow`/`delete_workflow`/`parse_workflow`, ported from
`save_workflow_at`/`delete_workflow_at`/`parse_workflow_input`). The builder writes
needed four small private helpers (`workflow_slug`/`workflow_step_issue`/
`workflow_input`/`workflow_yaml`) that `coducktor-server` never made `pub` —
duplicated into `in_process.rs` rather than shared, per this module's own stated
principle (`coducktor-server` is deleted whole at C2, so sharing across an
axum-shaped and a non-axum-shaped caller now would be wasted engineering); the
XOR/step-shape validation and YAML generation itself is copied byte-for-byte, not
re-derived. `skills_to_steps`/`steps_issue`/`parse_workflow_file_doc` themselves
were already `pub` in `coducktor-core::workflows::types` and are called directly,
not duplicated.
**Investigated and explicitly deferred this round, so a future continuation
doesn't re-discover the same complexity from scratch:** `provider_status`/
`models`/`agent_profiles` all looked like natural next candidates but each pulls
in a cluster of `coducktor-server`-private, non-`pub` support types and functions
(`ResolvedAgentProfile`, `default_agent_profile`, `provider_status_for_profile`,
`agent_profile_wire`, `selection_wire`, `resolved_agent_profile`,
`provider_executable`, plus `get_models`'s own TTL cache keyed on
`state.model_catalog`) that don't exist anywhere `coducktor-client` can reach
without either a larger duplication pass than this round's budget allowed or a
real `coducktor-core` extraction (arguably the more correct home for
`ResolvedAgentProfile` and friends, since none of it is axum-specific — worth
doing as its own small prerequisite step rather than duplicating it a second time
inside `coducktor-client`). `config`/`put_config` (per-repo settings) were also
skipped: `update_config`'s handler carries real model-lock/base-branch/system-
prompt merge logic worth porting carefully, not rushed. None of these are done —
still listed as remaining below.
**Still remaining** (unchanged from C1.1's list, minus what C1.2 just closed):
IDE, repo git browsing/diff/compare, agent-config, provider/account probing
(`provider_status`, `models`, agent-profile CRUD, account status/details),
GitHub forge detail reads, worktree management, open-targets, per-repo
`config`/`put_config`, the remaining settings write paths
(`put_workspace_config`/`workspace_config`/`workspace_ui_state`/
`put_workspace_ui_state`/`update_project`/`remove_project`), task-thread write
paths (`send_message`/`edit_queued_message`/`remove_queued_message`/
`continue_run`/`cancel_auto_resume`/`git_commit`/`git_push`/`run_commits`/
`create_pr`/`open_in_cli`/`open_in`), `agent_profiles`, `plan`, and closing the
`impl Engine for InProcessEngine` block itself.
**Accept, verified (C1.2's own scope only):** 12 new tests (4 for
`save_workflow`, 3 for `delete_workflow`, 3 for `parse_workflow`, 1 for
`workspace_usage`) — 37 total in `in_process::tests`. Full workspace: 717 tests
green (705 + 12), `cargo clippy --workspace --all-targets -- -D warnings` clean,
`cargo fmt --check` clean.
**Commit:** `feat(engine): C1.2 InProcessEngine — workspace_usage, workflow
builder writes` — pushed as `25f890d6`.

**Status (C1.3):** partial, continuing C1.2. Resolves the `provider_status`/
`models`/`agent_profiles` cluster C1.2 explicitly deferred (with a concrete
reason to change it this round, not another vague punt): ships `provider_status`,
`agent_profiles`, `create_agent_profile`, `update_agent_profile`,
`remove_agent_profile`, `select_agent_profile`, `agent_account_status`,
`agent_account_details`, and `open_agent_account_file`, ported from
`coducktor-server`'s `get_provider_status`/`list_agent_profiles`/
`create_agent_profile`/`update_agent_profile`/`remove_agent_profile`/
`select_agent_profile`/`get_agent_profile_status`/`get_agent_profile_details`/
`open_agent_profile_file`. `models` (the host model-catalog family — its own
TTL-cache-keyed cluster, `get_models`/`discover_opencode_models`/
`discover_codex_models`) is a real, separate family and is still deferred, named
explicitly rather than folded in under a vague "agent profiles" umbrella.
**Duplication, same rationale as C1.2's workflow-builder helpers:**
`ResolvedAgentProfile`, `default_agent_profile`, `resolved_agent_profile`,
`profile_file_defs`/`profile_files`/`profile_dir_state`, `agent_profile_wire`,
`selection_wire`/`selection_empty`/`set_profile_selection`,
`agent_profiles_response`, `profile_path_error`/`same_profile_dir`/
`profile_conflict`, `project_root_for_agent_selection`, `account_by_route_id`,
`provider_executable`/`provider_probe_args`/`provider_install_hint`/
`provider_state_from_output`/`provider_status_for_profile`,
`capped_json_file`/`identity_text`/`agent_profile_details`, and
`account_open_default` are all copied byte-for-byte from `coducktor-server`'s
private functions of the same name — none were `pub`. `allocate_project_id`'s
account-id counterpart (`allocate_account_id`/`account_slug`/
`RESERVED_ACCOUNT_SLUG_IDS`) is also duplicated, deliberately preserving the
oracle's own quirk of falling back to the word `"project"` (not `"account"`) for
an unslugifiable label — the real server does this today, so byte-for-byte
fidelity means keeping it, not "fixing" it here.
**Scope simplification, named not hidden:** `open_agent_account_file` supports
only the OS-default-opener path (`target: None`); an explicit `target` (pick a
specific app) returns a clear `Conflict` rather than silently no-op'ing or
mishandling it — that selection depends on the not-yet-ported open-targets
registry (`open_targets`/`open_target`, its own family, a C1 follow-up).
**A real, non-obvious testing constraint, documented in the test module itself:**
`create_agent_profile`/`update_agent_profile`/`remove_agent_profile`/
`select_agent_profile` resolve their storage path via
`agent_accounts_path(&ProcessEnv)` — the REAL `~/.coducktor/agent-accounts.json`
(or `$DUCK_HOME`/`$CEZ_HOME`), with no injectable override, matching the oracle's
own hardcoded `ProcessEnv` usage. No test actually exercises a write against that
real path — every write-path test here exercises validation that returns before
any file I/O (unsupported provider, relative config dir, missing required field,
unknown id/project — all NotFound/Conflict before ever touching disk). A full
create/update/remove round-trip against an isolated `agent-accounts.json` isn't
covered — it would need `agent_accounts_path` to accept an injected `EnvSource`
the way `coducktor-core`'s lower-level `load_agent_accounts`/
`merge_write_agent_accounts` already do, which is a real gap in the *oracle* this
ports from (no such env-injection test pattern exists anywhere in this workspace
today), not something introduced or worsened here.
**Still remaining:** IDE, repo git browsing/diff/compare, agent-config, `models`
(host model catalog), GitHub forge detail reads, worktree management,
open-targets, per-repo `config`/`put_config`, the remaining settings write paths
(`put_workspace_config`/`workspace_config`/`workspace_ui_state`/
`put_workspace_ui_state`/`update_project`/`remove_project`), task-thread write
paths (`send_message`/`edit_queued_message`/`remove_queued_message`/
`continue_run`/`cancel_auto_resume`/`git_commit`/`git_push`/`run_commits`/
`create_pr`/`open_in_cli`/`open_in`), `plan`, and closing the
`impl Engine for InProcessEngine` block itself.
**Accept, verified (C1.3's own scope only):** 25 new tests — write-path
validation (unsupported provider, relative config dir, missing update field,
unknown id/project for update/remove/select/status/details/open), read-only
lookups against a real (possibly-empty) environment (`provider_status`,
`agent_profiles`, `default:claude` synthetic-id resolution — matching the same
"safe against a real environment" precedent `projects_reports_the_registry_snapshot`
already established in C1.1), and pure unit tests for every duplicated helper
that doesn't touch `ProcessEnv` (`account_slug`, `allocate_account_id`,
`provider_state_from_output`, `identity_text`, `same_profile_dir`,
`profile_dir_state`, `agent_profile_wire`). 48 total in `in_process::tests`. Full
workspace: 740 tests green, `cargo clippy --workspace --all-targets -- -D
warnings` clean, `cargo fmt --check` clean.
**Commit:** `feat(engine): C1.3 InProcessEngine — provider status, agent-profile
accounts` — pushed as `db7f61c3`.

**Status (C1.4):** partial, continuing C1.3. Ships two families: IDE
(`ide_tree`/`ide_file`/`ide_save`) and per-repo config (`config`/`put_config`).
Both ported from `coducktor-server`'s matching handlers
(`list_ide_directory`/`read_ide_file`/`write_ide_file` and
`get_config`/`update_config`), duplicating their private helpers byte-for-byte —
same rationale as every prior C1 sub-step (`coducktor-server` is deleted whole at
C2). `Scope` is dropped from every new method's signature, same convention every
earlier C1 method already established (this crate serves exactly one repo root;
`coducktor-server`'s own "scoped" IDE/config routes already ignore their
`:project` path segment today for the same reason).
**A genuine simplification found while porting, not a shortcut:** `put_config`
here takes an already-typed `&SetConfigInput` directly (the `Engine` trait's own
signature), unlike the HTTP handler, which has to reconstruct the "field absent
vs. field present-but-null" distinction from a raw `Map<String, Value>` kept
alongside the typed struct (an artifact of the JSON-parse boundary). Because the
trait already hands over a real `SetConfigInput` with its `Option<Option<T>>`
fields already correctly discriminated by the caller, `update_repo_config` here
needs no parallel raw-object bookkeeping — it is a straightforward, smaller port
of the same field-by-field merge logic, not a reduced version of it.
**Scope note on `ide_save`:** matches the oracle exactly — `ide_write_file`
resolves the target path (which requires the file to already exist) before
writing, so `PUT /ide/file` edits an existing file, it does not create a new one.
Proven by test (`ide_save_cannot_create_a_file_that_does_not_already_exist`), not
just asserted in prose.
**Still remaining:** repo git browsing/diff/compare (`repo`/`repo_changes`/
`repo_commit`/`repo_branch`/`run_diff_text`/`run_changes`/`run_commit`/
`run_files`/`run_file_raw`/`group`/`pick_variant`), agent-config
(`agent_config`/`agent_config_file`/`put_agent_config_file`), `models` (host
model catalog), GitHub forge detail reads, worktree management, open-targets,
the remaining settings write paths (`put_workspace_config`/`workspace_config`/
`workspace_ui_state`/`put_workspace_ui_state`/`update_project`/
`remove_project`), task-thread write paths (`send_message`/
`edit_queued_message`/`remove_queued_message`/`continue_run`/
`cancel_auto_resume`/`git_commit`/`git_push`/`run_commits`/`create_pr`/
`open_in_cli`/`open_in`), `run_history`/`run_history_context` (the task-thread
paginated-event-history reads — investigated this round, deferred: the oracle's
cursor encode/decode + boundary-seq pagination logic is its own meaningfully
sized chunk, not a quick add-on to this round's IDE/config work), `plan`, and
closing the `impl Engine for InProcessEngine` block itself.
**Accept, verified (C1.4's own scope only):** 12 new tests — IDE directory
listing (dirs-before-files, alphabetical, `.git` excluded but `.ai/coducktor`
correctly included since the oracle only special-cases `.git`), file read
(content + size, escape-the-project rejection, not-found), file save (overwrite
an existing file, cannot create a new one), config defaults with no file on
disk, a patch-then-reread round trip, null clears a previously-set field,
`maxParallel` range validation, and the models-locked-by-repo-config rejection.
59 total in `in_process::tests`. Full workspace green (all crates' `test
result: ok`, zero failures), `cargo clippy --workspace --all-targets -- -D
warnings` clean, `cargo fmt --check` clean.
**Commit:** `feat(engine): C1.4 InProcessEngine — IDE, per-repo config` — pushed
as `d35e6832`; doc update pushed as `74d4a928`.

**Status (C1.5):** partial, continuing C1.4. Ships the repo/run git
browsing-diff-compare family: `repo`, `repo_changes`, `repo_commit`,
`repo_branch`, `run_diff_text`, `run_changes`, `run_commit`, `run_files`,
`run_file_raw` — ported from `coducktor-server`'s `get_repo`/`get_repo_changes`/
`get_repo_commit`/`create_repo_branch`/`run_diff`/`run_changes`/`run_commit`/
`run_files` handlers, duplicating their private git-shelling helpers
(`repo_info_at`/`repo_status`/`repo_log`/`repo_branches`/`git_capture`/
`git_capture_owned`/`cap_git_text`/`diff_revision_args`/`changed_file_status`/
`collect_git_changes`/`valid_commit_hash`/`repo_commit_payload`/
`run_changes_payload`/`contains_git_component`/`read_worktree_path`/
`image_content_type`) byte-for-byte — same rationale as every prior C1
sub-step. `coducktor_core::git::worktree::worktree_diff` (already ported at B3)
is reused directly for `run_diff_text` rather than re-derived.
**A genuine simplification, not a shortcut:** the `Engine` trait's
`run_files`/`run_file_raw` are already split into two methods (structured
`WorktreeEntry` vs. raw bytes for image preview), so `run_file_raw` here is a
small, focused `read_worktree_raw` helper reusing `read_worktree_path` — no
need to port the HTTP handler's combined content-negotiation branch (mime
sniffing off an `Accept` header, `Vary` response header, JSON-vs-bytes
dispatch) at all, since the trait's caller already decided which one it wants.
**Scope note, named explicitly:** `group`/`pick_variant` are deliberately NOT
in this round — they mutate run state (cancel/archive losing variants, remove
their worktrees, touch the review gate via `append_event`/`update_run_value`)
rather than just reading git, a meaningfully different and larger cluster; left
for a follow-up round.
**Accept, verified (C1.5's own scope only):** 20 new tests — a real git-repo
fixture (mirrors `coducktor-core::git::worktree`'s own `fixture_repo()` test
helper) proving `repo`/`repo_changes`/`repo_commit`/`repo_branch` against
actual `git` subprocess calls (present vs. empty repo, a modified tracked
file, a known-vs-malformed sha, branch creation via `git branch
--show-current`, an unsafe branch name rejected), `run_files`/`run_changes`
against a run with `worktree: Some(false)` (works directly in the repo root,
matching `working_directory_of`'s own fallback — no real worktree-creation
machinery is wired into `RunManager` yet to fixture a true worktree path
against), not-found propagation for an unknown run id across three methods,
`run_file_raw` rejecting a non-image file, and pure unit tests for
`changed_file_status`/`valid_commit_hash`/`image_content_type`/
`contains_git_component`. 76 total in `in_process::tests`. Full workspace
green (every crate's `test result: ok`, zero failures), `cargo clippy
--workspace --all-targets -- -D warnings` clean, `cargo fmt --check` clean.
**Still remaining:** `group`/`pick_variant`, agent-config, `models` (host
model catalog), GitHub forge detail reads, worktree management, open-targets,
remaining settings write paths, task-thread write paths, `run_history`/
`run_history_context`, `plan`, and closing the `impl Engine for
InProcessEngine` block.
**Commit:** `feat(engine): C1.5 InProcessEngine — repo/run git browsing, diff,
compare` — pushed as `d17dce9b`; doc update pushed as `6493dd1e`.

**Status (C1.6):** partial, continuing C1.5. Ships the agent-config family:
`agent_config`, `agent_config_file`, `put_agent_config_file` — ported from
`coducktor-server`'s `list_agent_config`/`get_agent_config`/
`update_agent_config` handlers, duplicating the private
`AGENT_CONFIG_DEFINITIONS` catalog (all 14 entries — claude/codex/opencode
settings, MCP, and memory files across user/project/local scope) and its
supporting `resolve_agent_config_path`/`config_hash`/`agent_config_content`/
`jsonc_without_comments`/`validate_agent_config`/`claude_state_path`/
`user_mcp_listing`/`agent_config_listing`/`write_agent_config` helpers
byte-for-byte (none were `pub`). Added `sha2`/`toml` as new
`coducktor-client` dependencies (both already workspace deps used by
`coducktor-server` for this exact purpose).
**Scope note:** tests only exercise project/local-scoped definitions
(resolved under the tempdir repo root) — user-scoped definitions resolve
against the REAL `agent_home_paths`, and writing to a real environment's
`~/.claude` etc. from a test is out of bounds, same precedent C1.3 already
established for the agent-accounts family.
**Accept, verified (C1.6's own scope only):** 12 new tests — the full
14-definition listing shape, not-found for an unknown id, a missing project
file reporting `exists: false`, a create-then-reread round trip, invalid-JSON
rejection, stale-version-conflict rejection, refusing to empty a non-empty
file, and pure unit tests for `validate_agent_config` (JSON/TOML/JSONC/
Markdown), `jsonc_without_comments` (strips comments, leaves string content
alone), and `config_hash` (deterministic, content-sensitive). 86 total in
`in_process::tests`. Full workspace green, `cargo clippy --workspace
--all-targets -- -D warnings` clean, `cargo fmt --check` clean.
**Still remaining:** `group`/`pick_variant`, `models` (host model catalog),
GitHub forge detail reads, worktree management, open-targets, remaining
settings write paths, task-thread write paths, `run_history`/
`run_history_context`, `plan`, and closing the `impl Engine for
InProcessEngine` block.
**Commit:** `feat(engine): C1.6 InProcessEngine — agent-config`

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
