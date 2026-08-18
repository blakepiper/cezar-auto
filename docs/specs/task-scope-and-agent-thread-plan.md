# Implementation plan: project task scope and polished agent threads (superseded)

> Superseded by the implemented [Task experience](task-experience.md). Retained only as a record
> of the earlier table and Conversation/Activity proposal.

Companion specification: [Project-scoped tasks and agent thread UX](task-scope-and-agent-thread-ux.md)

This plan is written for GPT-5.6 Luna. Execute phases in order. Keep each phase independently
reviewable, run its focused tests before proceeding, and do not begin visual polish until Phase 0's
two-project isolation tests pass. Work directly on `main` and preserve unrelated changes as required
by `AGENTS.md`.

## Operating instructions

Before each phase:

1. Read the companion specification and the files named in the phase.
2. Search all call sites before changing a public type or deleting compatibility code.
3. Write or update the phase's regression tests first where practical.
4. Keep screens behind the `Engine` trait; do not read project files from TUI code.
5. Keep runner-specific wire types in `coducktor-runners` and render only normalized protocol data.
6. Do not use `unwrap()` or `expect()` in production paths.
7. Do not accept terminal snapshots blindly.

After each phase, run focused tests and `cargo fmt --all --check`. At the end, run the full repository
gate from `AGENTS.md`.

## Phase 0 — Make task scope real

### Objective

Make every task operation honor the project selected by the caller. Prove there is no fallback to
the boot repository or collision on run ID.

### Primary files

- `crates/coducktor-client/src/engine.rs`
- `crates/coducktor-client/src/in_process.rs`
- `crates/coducktor-client/src/events.rs`
- `crates/coducktor-core/src/runs/` and workspace registry modules as discovered
- `crates/coducktor-contract/src/` only if a stronger scope/key type is needed
- client/core integration tests

### Work

1. Inventory every `Engine` method accepting `&Scope`. Make a test checklist for run lifecycle,
   history, files/diff/commits, usage, worktrees, config, workflows, skills, repo info, and UI state.
2. Add a resolver from project ID to registered project metadata and canonical root. Unknown or
   unavailable projects must return a scoped error; never substitute `repo_root`.
3. Replace the single-manager routing assumption with a lazy project manager registry. Reuse live
   managers in `runs_index` instead of opening throwaway duplicates.
4. Route start/get/list/archive/delete/read/unread/archive-finished/mark-all-read/cancel/send/
   continue/finish/history/files/diff/commits/group/worktree operations through the resolved
   project context.
5. Ensure runner cwd, project config, skills/workflows, state directory, branch, and worktree all
   come from the same resolved project.
6. Subscribe to or multiplex run/usage events from every live project manager and attach the
   registry project ID. Define behavior for a project registered after startup.
7. Make global indexing consume the registry seam and deduplicate by `TaskKey`.
8. Preserve zero-configuration startup: missing registry entries, roots, Git, CLIs, credentials,
   or writable optional state may reduce capability but cannot crash unrelated projects.

### Required tests

- Two temporary repositories A/B with distinct runs.
- Same run ID in A/B.
- Start in B while booted in A, asserting B state path and cwd.
- Every mutation/read method targets B and leaves A untouched.
- Global index includes both once.
- Live event project attribution for both managers.
- Unknown and unavailable project errors do not fall back.

### Completion gate

All engine conformance tests pass and no production `Engine for InProcessEngine` method discards a
project scope with `_scope` unless the method is documented and tested as workspace-wide.

Suggested commit: `Honor project scope across task operations`

## Phase 1 — Project-qualified TUI state

### Objective

Prevent stale responses and cross-project events from corrupting visible state. Make Project Tasks
and All Tasks explicit, independent views.

### Primary files

- `crates/coducktor-tui/src/app.rs`
- `crates/coducktor-tui/src/main.rs`
- `crates/coducktor-tui/src/screens/tasks.rs`
- `crates/coducktor-tui/src/screens/global_tasks.rs`
- `crates/coducktor-tui/src/screens/new_task.rs`
- relevant input/hitmap and snapshots

### Work

1. Introduce a TUI `TaskKey { project_id, run_id }` or use an equivalent contract type where rows
   cross the workspace boundary.
2. Replace `App.tasks` plus shared task filter/selection/usage with project-keyed task state. Keep
   the global index state separate.
3. Add monotonically increasing list-request generations per project. Return `{ project,
   generation, result }` from async loads and reject stale generations.
4. Fix workspace event routing. In particular, `RunDeleted` must remove from the visible project
   list only when project IDs match, while still updating the qualified global index and quick list.
5. Audit row menus, pending actions, confirmations, notifications, thread routes, and background
   refreshes so they carry `TaskKey` through execution.
6. Give Project Tasks and All Tasks the headers, columns, empty states, load errors, and independent
   filters specified in the companion document.
7. Add project ID and root context above the New Task composer. Capture the project in the pending
   start action; preserve the prompt/draft on error; route success using the returned qualified key.

### Required tests

- A → B → A with deliberately out-of-order list responses.
- A B-run deletion event does not remove an A row with the same run ID.
- Project/global filters and selections survive route switching independently.
- Every menu action keeps the row's project if sidebar selection changes before completion.
- New Task submit cannot be retargeted by navigation while pending.
- 80×24 and 120×40 project/global list snapshots.

### Completion gate

All scope acceptance criteria in the specification pass through the TUI boundary, including stale
responses and colliding run IDs.

Suggested commit: `Isolate project task state in the TUI`

## Phase 2 — Build the turn projection

### Objective

Convert the compatible reduced event stream into a stable, testable turn view model without
changing the durable source of truth.

### Primary files

- `crates/coducktor-tui/src/screens/thread/reducer.rs`
- new `crates/coducktor-tui/src/screens/thread/projection.rs`
- `crates/coducktor-tui/src/screens/thread/mod.rs`
- `crates/coducktor-protocol/src/ui_events.rs` only for proven semantic gaps
- thread fixtures/tests

### Work

1. Add `ThreadViewModel`, `TurnViewModel`, prompt, response, activity tree, current status, and
   outcome types as pure data structures.
2. Map the initial `run.record.task` and later durable user messages into one prompt per turn.
   Preserve text exactly and define reconciliation for an optimistic temporary prompt ID.
3. Classify assistant messages by `MessagePhase`. Use a documented backwards-compatible heuristic
   only when legacy events omit it.
4. Assemble nested activity using `parent_item_id`. Retain orphaned nodes in an explicit group.
5. Derive active status deterministically from running tools, questions/approvals, plans, retry
   state, then commentary.
6. Derive turn outcomes from observed events: completion reason, timing, usage, changed files, and
   verification evidence. Represent unknown/not-run separately from success.
7. Keep existing v1 reduction and unknown event salvage. Do not rewrite persisted histories.

### Required tests

- Initial and multi-turn exact prompts.
- Commentary vs final messages.
- Streaming lifecycle and duplicate/out-of-order updates.
- Running/failed/interrupted turns without a final message.
- Nested and orphaned children.
- v1 fixtures with missing phases/IDs.
- Verification pass/fail/unknown classification.

### Completion gate

The projection can represent every existing thread fixture with stable IDs, and its tests require no
Ratatui frame or filesystem.

Suggested commit: `Project run events into conversational turns`

## Phase 3 — Semantic activity presentation

### Objective

Replace generic noisy tool cards with compact semantic activity that remains fully inspectable.

### Primary files

- new `crates/coducktor-tui/src/screens/thread/presenters.rs`
- `crates/coducktor-tui/src/screens/thread/widgets.rs`
- `crates/coducktor-tui/src/widgets/transcript.rs`
- protocol normalization in runner adapters only where required data is currently lost

### Work

1. Define a pure presenter interface returning title, subject, status, preview, expanded detail,
   locations, and change counts.
2. Implement presenters for read/list/search, edit/write/patch, execute, fetch, task/subagent,
   approval/question, and unknown tool kinds.
3. Make paths project-relative and preserve full paths in expanded detail. Bound and sanitize
   output; retain errors and exit status in collapsed rows.
4. Group adjacent successful read/search items in Conversation. Do not group active or failed
   items. Keep individual items in Activity.
5. Render subagents recursively in Activity with depth/height bounds and an explicit expand action.
6. Render reasoning collapsed by default, using only normalized provider-visible summary/content.
7. Preserve expansion state by stable ID through every streaming reconciliation.

### Required tests

- Presenter table tests for all kinds and statuses.
- Unknown tool fallback and malformed/missing fields.
- Long/Unicode paths, ANSI output, very large output, diffs, and nonzero exit codes.
- Grouping boundaries and nested depth limits.
- Snapshots for running, success, failure, cancellation, and interruption.

### Completion gate

A reader can understand the active operation from compact rows, and every normalized detail remains
reachable without raw event JSON dominating Conversation.

Suggested commit: `Render semantic agent activity`

## Phase 4 — Conversation and Activity thread UI

### Objective

Ship the visual hierarchy: exact prompts, calm progress, clear final responses, and detailed activity
on demand.

### Primary files

- `crates/coducktor-tui/src/screens/thread/mod.rs`
- `crates/coducktor-tui/src/screens/thread/widgets.rs`
- `crates/coducktor-tui/src/widgets/transcript.rs`
- theme, keymap, input, and hitmap modules

### Work

1. Add the Conversation/Activity selector under existing task tabs. Both consume the same view
   model and retain selected turn/item anchors when switching.
2. Render each turn as Prompt, Activity, Response, Outcome. Keep completed Prompt/Response expanded
   and collapse completed Activity to a useful count summary by default.
3. Add a sticky current-status row with elapsed time and stop affordance.
4. Implement stable anchored scrolling, follow-mode disengagement, unseen event count, and `G` to
   resume. Coalesce redraws without delaying input.
5. Add focus targets and keyboard behavior from the specification, plus matching mouse hit targets.
6. Define responsive priorities for 80×24, 100×30, and 120×40. Remove metadata before hiding prompt,
   status, response, or composer.
7. Cache markdown/wrapping by content and width; do not rebuild expensive render data on every
   paint.

### Required tests

- Golden snapshots at all three target sizes for running, settled, failed, multi-turn, and nested
  activity states.
- Focus traversal and expansion behavior.
- Bottom-follow, manual-scroll anchor stability, unseen count, and `G`.
- Streaming growth above/below the viewport.
- Resize while scrolled and while composing.

### Completion gate

The prompt, current action, final response, and next input are identifiable in one glance at 80×24,
and full activity remains inspectable at larger sizes.

Suggested commit: `Redesign task threads around conversational turns`

## Phase 5 — Composer, intervention, and iteration

### Objective

Make send → observe → intervene → review → follow-up a reliable continuous loop.

### Primary files

- `crates/coducktor-tui/src/screens/thread/mod.rs`
- `crates/coducktor-tui/src/widgets/composer.rs`
- `crates/coducktor-client/src/engine.rs`
- relevant contract capability/response types
- runner adapters for honest capability reporting

### Work

1. Add explicit backend/session capability data for steer, safe queue, cancel, resume, approvals,
   and questions when the current protocol cannot infer it reliably.
2. Change composer copy and submit action by settled/running/failed state and capability. Never label
   an action `steer` if it actually starts a later turn.
3. Add optimistic user prompts with a pending ID, single reconciliation, double-submit protection,
   and draft restoration on failure.
4. Preserve composer drafts across Thread/Changes/Files/Commits and transient overlays.
5. Mirror blocking questions/approvals into the dock with complete keyboard operation while keeping
   the event in history.
6. Add explicit stop/cancel interaction that preserves draft text and records the resulting terminal
   status.
7. When a task settles, reveal the final response and outcome actions without stealing focus from a
   composer the user is already editing.

### Required tests

- Settled follow-up, supported steer, unsupported steer with safe queue, and disabled unsafe queue.
- Delivery success/failure, duplicate acknowledgement, and rapid double submit.
- Stop while composer contains a draft.
- Question single/multi-select, custom response, skip/reject, and resolution.
- Approval allow-once/allow-persistent/reject where supported.
- Navigation across task tabs with an unsent draft.

### Completion gate

Every composer label predicts the actual engine action, no prompt is lost or duplicated, and a user
can complete two turns plus one intervention using only the keyboard.

Suggested commit: `Polish task intervention and follow-up flow`

## Phase 6 — Review surfaces and terminal hardening

### Objective

Make completion trustworthy and validate the experience in real terminals.

### Primary files

- thread outcome/action widgets
- existing `screens/task_git.rs` and related task tab routing
- `docs/tui/keymap.md`
- `docs/tui/terminals.md`
- snapshots and benchmarks

### Work

1. Connect observed changed-file/test/usage summaries to Changes, Files, and Commits without
   inventing evidence from agent prose.
2. Add copy-response and direct review actions. Preserve project-qualified routing in every action.
3. Verify degraded states: no Git, missing agent CLI, missing credentials, unwritable optional state,
   unavailable project, corrupt per-project state, and interrupted child process.
4. Profile a long streaming thread and a history with thousands of events. Fix avoidable full-list
   rebuilds, output expansion, or markdown/wrap churn.
5. Test in the real supported terminal set. Record terminal name, dimensions, input path, focus,
   scroll, resize, mouse, Unicode, and observed results in `docs/tui/terminals.md`.
6. Update `docs/tui/keymap.md` and screenshots only after behavior is stable.

### Required checks

```text
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo tree --workspace
```

Also search remaining documentation for claims that contradict the shipped terminal-only product.

### Completion gate

All specification acceptance criteria pass, the full repository gate is clean, affected snapshots
have been manually reviewed, and real interactive terminal results are documented.

Suggested commit: `Harden agent thread review workflow`

## Final implementation audit

Before declaring the work complete, answer each item with a code/test link:

- Which type guarantees `(project_id, run_id)` identity?
- Which resolver guarantees a selected project cannot fall back to the boot root?
- Which two-project test proves start/mutate/read isolation?
- Which generation check rejects stale list responses?
- Which normalized field distinguishes commentary from final output?
- Which presenter handles unknown tools?
- Which state preserves scroll anchor and expansion during streaming?
- Which capability decides steer versus queue versus follow-up?
- Which evidence distinguishes tests passed, failed, and not observed?
- Which real terminals were exercised at 80×24 and 120×40?

If any answer relies only on visual inspection, agent prose, or the currently selected sidebar
project, the corresponding requirement is not complete.
