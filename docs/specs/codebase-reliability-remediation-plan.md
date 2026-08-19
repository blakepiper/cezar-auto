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
| R1 provider turn monopolizes a manager | open, critical | Replace project-wide blocking `pump` ownership with cancellable per-run workers and real shared admission. |
| R2 TUI awaits normal actions | partial, critical | Route every action through one bounded command executor; finish input-priority and backlog scheduling. |
| R3 stream amplification | partial | Add the remaining coalescing/metrics/fault assertions; retain the implemented batched index and transcript paths. |
| R4 worktree execution | complete | Do not redesign; preserve its integration coverage. |
| R5 checks and review gate | complete | Do not redesign; preserve its integration coverage. |
| R6 selected account environment | complete | Do not redesign; preserve its integration coverage. |
| R7 resource settings | partial, high | Wire process-wide workspace/repository leases and monitoring wake policy; keep memory limit visibly unavailable. |
| R8 runner protocol drift | partial, high | Complete the fixture/capability matrix and non-hanging response coverage. |
| R9 worker/process lifetime | partial, high | Give all workers cancellation, bounded queues, shutdown escalation, and leak checks. |
| R10 durable run state | functionally complete | Complete the listed fault-injection coverage only. |
| R11 dead UI/duplicate tests | primary correction complete | Extract oversized orchestration code only when needed by R1/R2; no standalone refactor. |
| R12 cross-platform CLI handoff | code complete | Record real-terminal results where the platform is available; do not fabricate unavailable-platform evidence. |

Evidence checked during this rewrite:

- `InProcessEngine::activate_runs` in `crates/coducktor-client/src/in_process.rs` still spawns one
  thread per project and holds `Arc<Mutex<RunManager>>` while calling `RunManager::pump`. This
  remains the blocking ownership defect; its background thread does not create same-project
  parallelism.
- `crates/coducktor-tui/src/runtime.rs` has a fixed four-thread background pool, generation guards
  for many loads, and per-frame receiver/action budgets. `execute_pending` still awaits ordered
  engine and host operations directly in the render/input task, and the pool queue is unbounded
  and non-cancellable.
- `RunManager` already has injectable `WorkspaceSemaphore` and `RepositoryRootLease` seams in
  `crates/coducktor-core/src/workflows/run/{mod.rs,semaphore.rs}`, but production manager wiring
  does not install shared process-wide implementations.
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
