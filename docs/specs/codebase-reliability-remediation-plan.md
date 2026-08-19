# Codebase reliability remediation — remaining-work implementation specification

Status: ready for one autonomous implementation session (rewritten 2026-08-19)

Audience: the next implementation agent. Work directly on `main`, preserve unrelated changes,
commit the completed work, and push `origin main` as required by `AGENTS.md`.

## Purpose and scope

This is the authoritative plan for the work that remains from the 2026-08-18 reliability audit.
It deliberately does not repeat completed implementation history except where a completed seam is
an integration dependency. It is a reliability and advertised-behavior remediation, not a product
redesign: retain the single in-process `Engine`, the terminal UI, local durable files, and the
existing runner seam. Do not add a service, socket, browser, database, network API, or new
environment-variable vocabulary.

The target outcome is straightforward: a blocked agent turn cannot make the cockpit unresponsive;
all retained Settings controls are either effective or honestly unavailable; durable data and
provider RPCs fail safely; and the remaining platform claims have recorded evidence.

## Audit inventory

The original audit had twelve findings. Six have their functional correction complete, while the
remaining six contain the work below. Several completed corrections still require focused
verification, so this is not a percentage-complete release claim.

| Finding | Current state | What remains |
| --- | --- | --- |
| R1 provider turn monopolizes a manager | core defect fixed | Concurrent same-project turns and non-blocking admission are in place (see evidence). `SessionFactory::open` still holds one process-wide `Mutex` for its whole call, so concurrent *opens* still serialize even though concurrent *turns* no longer do — narrowing the trait to `&self` would need every implementor updated across three crates; not attempted here. |
| R2 TUI awaits normal actions | required tests complete except true cancellation | All four required scaling/staleness tests now exist and pass (see evidence), including a real bug the A→B→A test caught and fixed: the IDE's file/directory loads had no generation guard at all. Remaining, deliberately not attempted: true mid-flight cancellation of a job already handed to a worker thread — `BackgroundWorkers` runs `handle.block_on(future)` to completion on a native thread once dispatched, with no cancellation seam; only pre-dispatch coalescing (closed this session) avoids redundant work. |
| R3 stream amplification | partial | Add the remaining coalescing/metrics/fault assertions; retain the implemented batched index and transcript paths. |
| R4 worktree execution | complete | Do not redesign; preserve its integration coverage. |
| R5 checks and review gate | complete | Do not redesign; preserve its integration coverage. |
| R6 selected account environment | complete | Do not redesign; preserve its integration coverage. |
| R7 resource settings | complete except memory limit | Workspace/repository leases and monitoring wake policy are both wired end-to-end now (see evidence); `memory_limit_mb` remains saved-but-honestly-unavailable, which is the intended final state, not a gap. |
| R8 runner protocol drift | partial, high | Complete the fixture/capability matrix and non-hanging response coverage. |
| R9 worker/process lifetime | partial, high | `TurnDispatch`'s per-run worker registry is now leak-checked (see evidence); still missing shutdown escalation — no bounded-wait-then-force-terminate path exists for a worker whose session ignores cancellation. |
| R10 durable run state | functionally complete | Complete the listed fault-injection coverage only. |
| R11 dead UI/duplicate tests | primary correction complete | Extract oversized orchestration code only when needed by R1/R2; no standalone refactor. |
| R12 cross-platform CLI handoff | code complete | Record real-terminal results where the platform is available; do not fabricate unavailable-platform evidence. |

Evidence checked during this rewrite and subsequent implementation:

- `RunManager::execute_job` (`crates/coducktor-core/src/workflows/run/mod.rs`) no longer opens a
  session or runs a turn itself. It stops the instant it would have called
  `SessionFactory::open`, records an `AdmittedTurn` (request plus enough resume state — workflow,
  step index, retry counts, plan checkpoint, failover context — to continue afterward), and
  returns; `RunManager::pump` keeps its exact signature and behavior for every existing caller,
  it just never blocks on provider I/O anymore. `RunManager::apply_open_failure`,
  `apply_admitted_turn`, and `apply_active_turn` are the three new entry points a caller uses to
  report back an open/turn/nudge result and receive either `TurnStep::Done` or
  `TurnStep::Nudge(RuntimeActive)` (send one more autonomous nudge and call `apply_active_turn`
  again). `RunManager::in_flight` counts an admitted-but-unresolved turn as busy so `pump`'s own
  capacity check and `RunManager::cancel` (which now defers to the worker's own eventual report
  instead of racing it) stay correct. `handle_active_outcome` (used by the synchronous
  `finish`/`deliver_message` resume path) is now `continue_active_turn`, shared by both that path
  and the new one; only a freshly admitted turn's `active.failover` is `Some`, so auto-failover
  stays exactly as failover-ineligible for a resumed session as it always was.
- `InProcessEngine`'s new `TurnDispatch` (`crates/coducktor-client/src/in_process.rs`) is the only
  place production code now actually opens a session or runs a turn: `activate_runs` and every
  mutating engine call that could admit new work (`cancel_run`, `finish_run`,
  `send_message`/`deliver_message`, `continue_run`) drains `RunManager::take_pending_turns` after
  its manager-lock section ends and hands each `AdmittedTurn` to its own OS thread. That thread
  opens the session and calls `turn`/`send_message` with no manager lock held at all; each
  streamed event and the final outcome are applied through a fresh, brief lock acquisition per
  call, never held across the child-process I/O. Two runs admitted from the same project now
  execute their turns genuinely concurrently, proved by
  `two_blocked_sessions_in_the_same_project_reach_their_first_tool_event_together` (a two-party
  barrier only satisfiable if both turns are in flight together); the parallelism ceiling itself
  is still enforced, proved by `a_single_slot_leaves_the_second_run_queued_behind_a_blocked_first`.
  `activate_runs`'s existing per-project background thread and worker-registry reaping are
  unchanged in shape — it just does admission (fast) and dispatch (spawns the real workers) instead
  of running a turn itself.
- Not fixed: `SharedSessionFactory::open` still locks the one process-wide
  `Arc<Mutex<Box<dyn SessionFactory>>>` for the entire `open` call (unchanged from before this
  session), because `SessionFactory::open` takes `&mut self`. Two runs opening concurrently on
  different providers still serialize on that mutex before their (now genuinely concurrent) turns
  begin. The real fix — narrowing `open`/`request_cancel` to `&self`, since no implementation
  actually needs exclusive access beyond an already-interior-mutable cancellation map — touches
  the trait and every implementor (`DefaultSessionFactory`, `SharedSessionFactory`, and every test
  fake across `coducktor-core`, `coducktor-client`, and `coducktor-runners`). Left for a dedicated
  follow-up rather than folded into this session's diff.
- Two blocking mock sessions on separate worktree paths in the same project both reach their first
  streamed event without waiting on each other (max_parallel 2); on max_parallel 1 the second run
  observably stays `Queued` and its session factory is never invoked while the first is blocked —
  both are now covered by dedicated tests in `crates/coducktor-client/src/in_process.rs`.
  Cancellation reaching a session blocked in `open` (not just `pump`'s old lock) remains covered by
  the existing `cancel_reaches_a_session_factory_blocked_during_open` test, still passing unchanged
  against the new worker path.
- `TurnDispatch::dispatch` initially left every finished `JoinHandle` in its per-run worker
  registry forever (nothing reaped it, unlike `activation_workers`, which
  `reap_finished_activation_workers` already swept). Fixed: `dispatch` now sweeps every finished
  handle before spawning new ones, so any later dispatch call — including an `activate_runs` with
  nothing new to admit — brings the registry back to empty. A worker cannot safely reap itself as
  its last action (a redispatch it triggers for its own run id can already have inserted a new
  handle under that key before the old thread returns), hence the sweep-on-every-dispatch design
  instead. `repeated_start_finish_and_cancel_cycles_return_turn_worker_counts_to_baseline` proves
  worker and cancellation-registry counts return to baseline across repeated start/finish and
  start/cancel cycles. Not covered: a worker whose session never observes cancellation at all has
  no bounded exit — there is still no shutdown-escalation path (graceful wait, then forced
  termination) for that case, which is the remaining R9 shutdown-escalation gap. "Reader" and
  "child" counts (pipe-reader threads, spawned child processes) are a `coducktor-runners`-layer
  concern with their own existing coverage (e.g. `child_process::tests::drop_kills_and_reaps_a_live_child_with_its_pipe_readers`);
  this session did not touch that layer.
- `crates/coducktor-tui/src/runtime.rs` now dispatches normal engine/host operations through its
  fixed four-thread bounded worker pool; route generations reject stale results, input drains
  before receivers, and a receiver that exhausts its item/time budget wakes the next frame
  immediately. Same-run workspace updates coalesce per frame, with a sanitized counter.
- Closed one concrete piece of "complete request-key coverage": `App::queue_pending`'s existing
  dedup only ever saw the still-queued tail of `pending`, so a coalescable refresh
  (`RefreshTasks`/`RefreshIndex`/`RefreshProjectRegistry`/`RefreshNewTask`/`RefreshModels`)
  resubmitted from a *later frame*, after the earlier identical one had already been drained and
  handed to a background worker, was never recognized as a duplicate — nor were the many
  production call sites that push onto `pending` directly rather than through `queue_pending`.
  `App` now tracks `in_flight_coalescable`; `execute_pending` checks
  `coalescable_in_flight`/`begin_coalescable_dispatch` immediately before it would otherwise spawn
  one (the single point every such action passes through regardless of how it reached the queue),
  and each of the five `BackgroundResult` arms calls `finish_coalescable_dispatch` once its result
  arrives. `a_thousand_identical_refresh_submissions_across_frames_stay_bounded` proves 1,000
  submissions across 1,000 separate simulated frames collapse to exactly one dispatched job, not
  one per frame — the required R2 scaling case for idempotent loads.
- The other three required R2 tests are now written and pass too, closing out the required list
  from the original section 2 spec except true cancellation:
  `a_ten_thousand_result_burst_drains_across_bounded_frames_without_losing_any` proves
  `drain_background_results` needs `10_000.div_ceil(RECEIVER_ITEMS_PER_FRAME)` calls (many bounded
  frames, not one unbounded pass) and every individual call stays under 50ms, with all 10,000
  items eventually delivered intact.
  `spawning_a_slow_background_job_never_delays_the_dispatching_frame` proves `spawn_background`
  returns before a 5-second future it queued has even started, since it hands the future to a
  worker thread and never awaits it in the caller — the property every archive/delete/settings/Git
  dispatch relies on to keep a slow one from delaying that frame's draw.
  `slow_a_to_b_to_a_ide_file_reopen_never_overwrites_the_later_load` is the literal A→B→A required
  case, and it caught a real, previously-unguarded bug: `BackgroundResult::LoadIdeDirectory`/
  `LoadIdeFile` staled-result rejection only compared the response's path against the screen's
  current path, with no generation counter — every other generation-guarded screen
  (thread/settings/github/compare/repo-git/task-git/scratchpad) already had one, but reopening the
  *same* IDE file or directory (A → B → A landing back on A) meant a still-outstanding first load
  and the fresh reopen's load shared an identical path, so a slow first answer arriving after the
  reopen would have silently overwritten newer content with stale bytes. Fixed by adding
  `IdeUi::directory_generation`/`file_generation` (`begin_directory_request`/`begin_file_request`),
  mirroring every other screen's existing pattern, and checking them alongside the path in both
  result arms.
- Not attempted: true mid-flight cancellation of a job already handed to a worker thread (native
  threads running `handle.block_on(future)` to completion have no cancellation seam today — only
  pre-dispatch coalescing was addressed this pass).
- Production manager wiring now installs one shared workspace admission instance and canonical
  repository-root leases. Monitoring wake deadlines now have an owned scheduler:
  `RunManager::due_monitoring_wakes` (a thin filter over the new, pure
  `workflows::run::monitoring::is_due`, mirroring `auto_resume`'s shape) is polled by
  `InProcessEngine`'s new `coducktor-monitor` background thread on a bounded 15-second interval —
  not a tight/busy loop, and immaterial slack against a user-configured interval denominated in
  minutes. A due session is detached from `self.active` via the new `begin_monitoring_wake`
  (marking it `in_flight` so `RunManager::cancel` cannot race the eventual report, exactly like an
  admitted turn) and driven through the existing `TurnDispatch`/`apply_active_turn` machinery from
  R1 — a monitoring check-in is a `send_message` turn like any other nudge, just dispatched fresh
  instead of continuing an in-flight worker's own loop. The scheduler is started only from
  `InProcessEngine::new` (the one production construction path), never from the
  `with_session_factory`/`with_session_factory_at` test seams, so the test suite does not
  accumulate one sleeping thread per constructed engine.
  `a_parked_monitoring_session_is_woken_once_its_deadline_passes` proves the whole path end to end
  (with the interval shortened for the test via `spawn_monitoring_scheduler_with_interval`). The
  Settings UI's "unavailable · no owned scheduler" label was removed along with its two test
  assertions — the control is genuinely wired now, matching the workspace-config value it already
  displayed.
- Durable run metrics now report event appends, index flushes/bytes, and thread projection
  rebuild count/work. A truncated index is covered end-to-end: boot degrades, writes quarantine,
  and explicit repair makes a backup before replacing the index.
- `fixtures/CAPABILITY_MATRIX.md` is checked in and its golden test requires every replayed
  normalized Codex, Claude, OpenCode, and pi fixture to remain represented in the matrix.
- Worktree admission, production checks/diff inspection, account-home propagation, durable index
  salvage/repair, `repair-runs`, reader teardown, and portable handoff argument construction are
  present with focused regressions. Do not duplicate them.

## Non-negotiable constraints

1. Never hold a `MutexGuard` across runner I/O, child-process control, Git, filesystem traversal,
   a channel send that can block, or `.await`.
2. Keep all screen dependencies behind `Engine`; runner wire types remain in
   `coducktor-runners`; persisted request/response shapes belong in `coducktor-contract` and must
   be backward-compatible with current JSON.
3. Preserve NDJSON event compatibility, unknown JSON keys, one-warning corrupt-state behavior,
   atomic owner-only writes, and the retained task-marker/branch reader compatibility shims.
4. A missing CLI, credentials, Git, optional state directory, platform capability, or telemetry
   source must reduce only that capability and never prevent normal startup.
5. Tests may use `unwrap`/`expect`; production paths follow `AGENTS.md`'s panic prohibition.
6. Keep behavior changes and pure file moves in separate commits when both are needed. Reference
   search before deletion. Review `insta` changes rather than accepting blindly.

## Required implementation sequence

Implement in this order. Each section specifies enough choices to proceed without a product or
architecture decision from a human.

### 1. Replace blocking activation with a coordinator and per-run workers (R1, R9 foundation)

Primary files:

- `crates/coducktor-client/src/in_process.rs`
- `crates/coducktor-core/src/workflows/run/mod.rs`
- `crates/coducktor-core/src/workflows/run/semaphore.rs`
- focused client/core integration tests beside those modules

Keep `RunManager` as the durable state-transition authority. Introduce a client-side project
coordinator that owns a bounded command channel, worker registry, cancellation tokens, and the
shared workspace admission state. The coordinator holds the manager lock only to select/admit a
job, apply one event/outcome, or persist a state transition. It moves an admitted live
`AgentSession`/turn to a named per-run worker before executing it. A worker sends bounded outcome
commands back to the coordinator; it never mutates `RunManager` directly while executing a turn.

Use these precise policy decisions:

- Commands are `Start`, `Send`, `Continue`, `Finish`, `Cancel`, worker event, worker terminal
  outcome, and `Shutdown`. Each caller gets a typed acknowledgement/result.
- The coordinator command queue has a fixed documented capacity. On saturation, idempotent
  refresh/admission nudges coalesce; ordered mutations return a typed `Unavailable` result rather
  than blocking the UI thread indefinitely.
- Worker count is bounded by the effective workspace/project limits. FIFO admission uses a
  monotonically increasing enqueue sequence. A waiting/monitoring session releases its active
  slot exactly as the current `RunManager` semantics require.
- Store cancellation outside the manager lock. `cancel_run` signals it immediately, then queues
  the durable state update. An unavailable/busy manager must never prevent the signal.
- Give every worker a shutdown token and join handle. Shutdown requests graceful cancellation,
  waits a small named bounded interval, escalates to child termination through the existing session
  seam, then reaps completed workers/readers. A confirmed TUI quit must not wait forever.
- Maintain the existing worktree/profile/check/review admission path. The authoritative working
  directory and chosen profile remain fixed for an already-admitted step.

If moving session ownership requires a narrow core API, add it there with a contract-preserving
test; do not expose provider-specific session types through `Engine`.

Required tests:

- Two deliberately blocked mock sessions in the same project both reach their first tool event
  when effective parallelism is two; with one, the second remains queued.
- A blocked turn leaves `get_run`, `list_runs`, `runs_index`, navigation-facing reads, and cancel
  able to complete inside 100 ms in a deterministic test.
- Project A cannot consume project B's project limit; workspace limit remains authoritative.
- Cancellation reaches a blocked child without waiting for a coordinator state lock.
- Repeated start/cancel/finish and shutdown cycles return worker, cancellation, reader, and child
  counts to baseline. Cover a worker that ignores graceful cancellation.

### 2. Make TUI commands fully non-blocking and bounded (R2, R9 completion)

Primary files:

- `crates/coducktor-tui/src/runtime.rs`
- `crates/coducktor-tui/src/app.rs`
- TUI runtime/app tests

Replace the action-specific use of `BackgroundWorkers` with one typed command executor. It may
reuse the existing fixed native-worker implementation, but it must have a bounded submission queue,
request key, generation, cancellation/supersession token, and typed completion. No branch of
`execute_pending` may await engine, filesystem, Git, subprocess, or terminal work in the frame
task after this change. Ordered mutations preserve FIFO execution; idempotent loads coalesce by
their full route/request key.

Use full route identity, not only project identity, for stale-result rejection: project, screen,
run/group/path/file selection, and request generation where applicable. Continue using the
existing route guards rather than adding a second state cache. When the queue is full, replace an
existing coalescible request with the newest one; report an ordered mutation failure visibly.

The event loop must drain keyboard/terminal input before background completions and live events.
Keep current per-frame item/time budgets for every receiver, coalesce mouse-move and repeated
run-record updates, and schedule an immediate wake whenever any receiver still has backlog.
Replace unconditional sleep pacing with event-or-tick selection: idle waits for input/completion
or a low-frequency tick; busy work does not sleep after consuming its budget.

Required tests:

- Slow A → B → A responses never overwrite the active route, including file/path/group selections.
- A 10,000-event burst drains across bounded frames while quit and cancel are processed promptly.
- 1,000 identical refresh submissions use bounded workers and bounded queued jobs; ordered
  mutations preserve order.
- A deliberately slow archive/delete/settings/Git operation does not delay an input-to-draw test
  beyond 100 ms.

### 3. Finish shared policy wiring without implementing the separate auto-router (R7)

Primary files:

- `crates/coducktor-client/src/in_process.rs`
- `crates/coducktor-core/src/workflows/run/{mod.rs,semaphore.rs}`
- `crates/coducktor-tui/src/screens/settings/mod.rs`

Install one process-wide shared workspace semaphore when managers are wired, so all lazily opened
projects participate in the same workspace capacity. Install one shared repository-root lease per
canonical root for in-place (non-worktree) runs; worktree-backed runs use the existing no-conflict
path. Reconfiguration changes limits only for future admission and never changes an active
session's cwd, profile, or established reservation.

Wire `monitoring_wake_interval_minutes` into the existing durable monitoring wake/reconciliation
path. A disabled value means no timer-driven wake; a configured value schedules only due monitor
work and must not spin or poll in a tight loop. Do not implement the advanced usage routing,
route reservations, provider probing, or automatic failover in
`intelligent-auto-routing.md`; its separate coordinator owns those features.

Do not pretend to enforce a portable child memory cap. Keep `memory_limit_mb` saved but render it
as unavailable with a concrete platform-neutral reason everywhere it is editable/viewable. The
existing unavailable marker test is the regression guard.

Add a table-driven conformance test covering every retained resource field: producer, effective
resolver, production consumer or unavailable marker, safe reconfiguration behavior, and test.

### 4. Close streaming and durable-state verification gaps (R3, R10)

Primary files:

- `crates/coducktor-core/src/workflows/run/mod.rs`
- `crates/coducktor-core/src/runs/` and persistence helpers
- `crates/coducktor-tui/src/screens/thread/`
- durability and projection tests

Retain durable semantic NDJSON append and the existing debounced index/notification batching.
Finish the missing measurements and fault tests rather than replacing the format. Coalesce only
safe fine-grained provider deltas into bounded semantic updates; preserve final text, tool output,
errors, normalized ordering, and exact final transcript. Lifecycle/terminal/error/shutdown
boundaries flush immediately.

Add local, sanitized counters/test seams for event append, index flush count/bytes, projection
rebuild count/time, command queue depth, coalesced updates, and worker count. They must not log
prompts, credentials, or raw provider payloads. Batch each received frame into one thread
projection update, preserving stable IDs and history prepend behavior.

Complete the remaining R10 fault matrix: unknown nested keys, one bad entry, truncated index,
permissions, concurrent writer conflict, disk-full, pre-rename crash, post-rename/directory-sync
failure, and repair-replacement failure. Each test must prove the original recoverable bytes are
not silently destroyed and the resulting behavior is typed/degraded as documented.

Required scaling assertions:

- 10,000 deltas retain the exact final transcript and cause a bounded number of index rewrites.
- N versus 2N accepted events is near-linear in projection work, not quadratic.
- A 300-run project plus several registered projects can refresh without waiting on a blocked
  provider worker.

### 5. Complete provider compatibility and terminal evidence (R8, R12)

Primary files:

- `crates/coducktor-runners/src/{codex_runner.rs,claude_runner.rs,opencode_runner.rs,pi_runner.rs}`
- committed sanitized fixtures under `fixtures/`
- `docs/tui/terminals.md`

Create a checked-in capability/fixture matrix for Codex, Claude, OpenCode, and pi. For each runner
cover: first/follow-up turn, built-in and custom/MCP tools, shell/PTY, delegation, question and
permission, image, plan, usage, cancellation, timeout, resume, and teardown. A missing capability
must produce a precise normalized degraded result; it may not leave a mock provider waiting or
cause an unrelated runner failure.

For every client-directed Codex request fixture, assert exactly one protocol answer, durable park,
or explicit JSON-RPC decline. Preserve the existing bounded permission and malformed-request
behavior. Keep Claude `--forward-subagent-text`; characterize native question/permission behavior
in headless fixtures and document it as unsupported if no durable answer seam exists. Never couple
runtime logic to one installed CLI version and never commit credentials, full prompts, or raw
provider captures.

Run real terminal handoff only on platforms actually available to the agent. Record command,
provider, terminal, pass/fail, date, and observed behavior in `docs/tui/terminals.md`. For an
unavailable macOS/Windows platform, leave its existing “not run” state and do not infer success
from Linux argument-construction tests.

## Explicitly out of scope

- New product features and any browser/server/hosted surface.
- Replacing the in-process engine or adding a second production engine.
- The broad automatic-routing implementation in `intelligent-auto-routing.md`.
- A standalone decomposition of large files. Extract only the coordinator/executor units needed to
  make ownership testable; make such moves behavior-preserving and separately committed.
- Deleting worktrees during retention or repair, or weakening existing compatibility readers.

## Completion and handoff checklist

Before committing, inspect the diff and run focused tests as each section lands. Then run:

```text
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo tree --workspace
```

Inspect changed `insta` snapshots manually. If terminal behavior was exercised, record the real
result in `docs/tui/terminals.md`; headless output is not terminal evidence. Commit coherent
changes on `main`, push `origin main`, and report the commit SHA, test commands/results, any
platform verification that could not be performed, and any deliberately deferred item from the
explicit out-of-scope list.

The remediation is complete when all required tests above exist and pass, no provider I/O occurs
under a state lock, no TUI frame awaits engine/host I/O, capacity is enforced across live projects,
durability and protocol failures preserve a usable degraded state, and the final repository gate
passes.
