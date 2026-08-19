# Codebase reliability and core-functionality remediation

Status: implementation in progress; audit and first remediation tranche completed on 2026-08-18

Scope: defects, incomplete production wiring, dead code, durability, and measured performance.
This plan does not propose new product features. It makes already-advertised behavior work, keeps
the cockpit responsive under its existing workload, and removes implementation that no longer has
a caller.

## Implementation progress

The first remediation tranche establishes the production seams and bounded hot paths needed by
the larger coordinator rewrite:

- run admission now materializes and persists worktrees before activation, and runners execute in
  the admitted worktree;
- selected agent profiles are resolved at admission and their provider-specific environment is
  passed to the child process;
- production checks and diff inspection use the run worktree, preserve real exit status, bound
  captured output, and enforce a timeout;
- workspace/project parallel limits, the monitoring-session cap, and the review-gate setting now
  configure every run manager, including lazily opened projects;
- reads use an observer-maintained run snapshot, cancellation bypasses a busy manager, activation
  threads are coalesced and owned, and session registrations are removed when sessions close;
- TUI background commands now run on a fixed four-worker native pool, so refresh storms cannot
  create one OS thread per request;
- streamed events remain durable while index rewrites and TUI transcript rebuilds are batched;
- run-index reads salvage valid entries, quarantine corrupt or partially corrupt state from
  overwrite, preserve unknown run and step keys, and replace indexes with collision-safe `0600`
  writes plus file synchronization;
- startup retention prunes only terminal stale records after committing the replacement index,
  removes matching NDJSON/handoff sidecars, and never removes recoverable worktrees;
- the usage-limit auto-resume setting now reaches runtime policy, so disabled workspaces keep due
  quota-limited runs parked rather than silently requeueing them;
- child process teardown owns and reaps stdout/stderr reader threads after bounded termination;
- Codex server requests receive explicit responses, Claude forwards subagent text, terminal CLI
  handoff uses structured arguments on Linux, macOS, and Windows, and Codex fixture coverage now
  proves unsupported MCP elicitation and unknown client requests cannot leave the provider
  waiting; dead thread docks plus the duplicated binary test target were removed; and
- forge cache-lock poisoning now resets only ephemeral cached state, while a poisoned merge guard
  fails closed; optional GitHub refreshes cannot terminate the terminal process.
- workspace UI-state serialization now returns a typed write error instead of panicking at the
  persistence boundary.
- agent-process setup now turns an unexpectedly absent stdio pipe into a typed spawn error and
  reaps the just-started child before returning.
- OpenCode's reasoning-variant fallback now validates its optional setting at the recovery point
  instead of relying on a panic-only invariant.
- project registration now reports an explicit I/O error if its durable mutation cannot produce a
  registry entry, rather than panicking on the internal result path.
- repository defaults now use a direct contract-value mapping and a safe all-default fallback,
  removing serialization and validation panics from config startup.
- workspace config serialization now writes its closed enum vocabulary directly, removing the
  remaining serialization panic paths while retaining the established wire spellings.
- agent-account serialization now uses the same stable provider spelling directly, removing its
  last persistence-boundary panic path.
- child-environment credential filtering now fails closed if its fixed regex cannot initialize,
  preserving the no-host-secret-leak guarantee without a startup panic.
- task-branch compatibility detection now uses its two exact retained prefixes directly, removing
  an unnecessary lazy regex initialization from the Git reader path.
- AskUser marker parsing and stripping now treat an unavailable compatibility regex as no marker,
  preserving the raw transcript instead of panicking on provider output.
- task-reference marker parsing and display stripping now likewise degrade to untouched raw text
  if a compatibility regex is unavailable.
- OpenCode's server-URL and reasoning-variant matchers now degrade to their existing fallback
  behavior when unavailable, instead of panicking during a provider turn.
- Codex resume setup now binds the already-validated session ID directly, preserving the fresh
  session fallback without a second invariant-based unwrap.
- Codex permission-profile approvals now park behind the existing durable permission flow and can
  grant only the exact bounded permission subset requested by the app server.
- Unsupported Codex dynamic-tool calls now receive the protocol's explicit failed result shape,
  so experimental provider tools cannot wait indefinitely for a nonexistent host.

The critical coordinator work is not complete. Provider turns still need to move out of the
project-manager critical section into bounded per-run workers before same-project parallelism and
all mutation latency requirements in R1 are satisfied. R2 still needs a general TUI command
executor instead of action-specific background work. The remaining R7 policy controls, full R8
protocol fixture matrix, bounded shutdown escalation in R9, and real-terminal R12 verification
also remain open. R10's core persistence protections and the `repair-runs` recovery command are
implemented; its exhaustive fault-injection matrix remains verification work. The findings and
acceptance criteria below remain authoritative until each item is closed by its named tests and
measurements.

## Outcome

Coducktor must remain interactive while several tasks are starting, streaming, waiting, or being
navigated; execute the worktree, workflow-check, account, review, and resource policies already
shown in the product; preserve durable state across compatible schema changes and corruption; and
track the installed agent CLIs closely enough that wrapping them does not silently disable their
core tools or strand a provider request.

The most likely explanation for the reported freezes is not one isolated slow render. A running
turn holds a project-wide `std::sync::Mutex<RunManager>` for the entire blocking provider call,
many TUI actions still await engine work in the input/render task, live events cause whole-index
disk rewrites and full transcript reconstruction, and three queues are drained without per-frame
budgets. Fix the ownership and scheduling model before attempting cosmetic render tuning.

## Audit baseline

The review covered the workspace crates, current specs, README promises, persisted-state code,
runner adapters, and the TUI event loop. The checked-out worktree was clean and all targets built.
The locally installed adapters available for protocol characterization were:

- Codex CLI 0.147.0;
- Claude Code 2.1.233;
- OpenCode 1.18.18; and
- pi 0.83.0.

Codex's generated app-server schema was compared with the adapter. Claude's installed CLI help was
compared with its constructed arguments. These versions are audit evidence, not versions to pin in
production. Compatibility tests must use committed fixtures and tolerate older supported shapes.

## Findings and required corrections

### R1 — A provider turn monopolizes the project manager (critical)

Evidence:

- `InProcessEngine::activate_runs` spawns a thread, locks the project manager, and calls
  `RunManager::pump` while retaining the guard.
- `pump` calls `execute_job`, which calls blocking `session.turn()` and may make further blocking
  `session.send_message()` calls before returning.
- Reads and mutations such as `list_runs`, `get_run`, history, archive, finish, and send all need
  the same mutex. Starting another activation thread only creates another waiter on that mutex.
- `RuntimeOptions::max_parallel` counts slots, but a single locked synchronous pump cannot execute
  two initial turns in one project concurrently. The setting therefore does not deliver the
  parallelism its name and Settings UI promise.

Required correction:

1. Split durable run state/coordinator commands from owned live sessions. A project coordinator
   may serialize short state transitions, but each live provider session must be owned by a
   cancellable per-run worker and must never run while holding the state-store lock.
2. Communicate worker events and terminal outcomes back through bounded commands. Apply each state
   transition and durable append in a short critical section.
3. Make `start`, `send`, `continue`, `finish`, and `cancel` explicit commands with acknowledgements.
   Cancellation must reach the process immediately without waiting for the state lock.
4. Make parallel limits real across projects and within one project. Use one workspace scheduler,
   project overrides, account/monitoring leases where already specified, and deterministic FIFO
   admission. Do not create one OS thread per UI refresh or activation.
5. Own worker handles and shutdown tokens. Confirmed quit may have a bounded graceful interval,
   then must terminate children without waiting indefinitely.

Acceptance:

- Two deliberately blocked sessions in one project both reach their first tool event when the
  effective limit is two; with a limit of one, the second stays queued.
- `get_run`, list refresh, navigation, and cancellation complete within 100 ms while a mock turn is
  blocked indefinitely.
- A project-A turn cannot consume project B's configured slot incorrectly; workspace and account
  limits remain authoritative.
- No `MutexGuard` is live across runner I/O, Git commands, filesystem traversal, or an `.await`.

### R2 — The TUI can block on ordinary navigation and mutations (critical)

Evidence:

- `execute_pending` performs many engine calls directly in the frame task: archive/read/delete,
  scratchpad and UI-state I/O, task Git, IDE, workflow, settings, project, and worktree operations.
- Several of those operations take the manager lock from R1 or perform synchronous filesystem work
  behind an `async fn`. Rapid navigation can queue several actions, which are then processed
  serially before the next draw.
- Refreshes use the fixed native worker pool to isolate blocking engine seams from the frame task.
  The pool has no request cancellation or bounded queue yet, and ordered mutations still await the
  general command executor.
- workspace events, background results, and thread events are each bounded by
  `RECEIVER_ITEMS_PER_FRAME` and a four-millisecond receiver time budget; thread events are
  collected into one batch before projection. The terminal loop still lacks backlog wake
  accounting and input-priority scheduling.
- Pending actions now drain FIFO in batches of `PENDING_ACTIONS_PER_FRAME`, retaining the tail for
  later frames. Individual actions still await engine and host work in the frame task, but their
  post-mutation global-index refresh is now queued into the backgrounded, generation-checked
  refresh path rather than awaited inline. Settings now collects its route-scoped snapshot on a
  background worker, fetches its independent sources concurrently, and applies it only while the
  matching settings route remains active.
  Post-action thread refreshes likewise queue the existing background thread loader instead of
  fetching a run inline. The task-Git changes load now fetches its run and diff concurrently on a
  background worker and discards results after navigation away from that task; task-Git file-tree
  reads and commit-list/detail reads now use the same route-guarded background path.
- IDE explorer and file reads now use that same route- and path-guarded background path, so
  out-of-order navigation completions cannot overwrite the selected directory or open draft.
- Repository Git aggregate, changes, and commit-detail loads now likewise require their active
  repository-Git route before they can update state or report an error; revisits also supersede
  earlier aggregate requests with a generation.
- Scratchpad hydration also runs in the background and applies only to its still-active project
  and request generation.
- Compare-group metadata and variant diffs likewise load in the background and reject stale
  project/group completions; aggregate metadata also carries a request generation for group
  revisits.
- GitHub handoff workflow and skill pickers now fetch concurrently in the background and discard
  a completion after leaving that project.
- Skills, workflow catalog, and workflow-palette skill reads now follow the same background,
  project-guarded completion model.
- Settings agent-config file reads now carry the selected file ID through the background result,
  preventing a slower earlier selection from opening the wrong editor.
- Post-registration project-registry refreshes are now best-effort background reads, rather than a
  second inline wait after the completed durable mutation.
- Compare variant selection now runs on the worker pool and queues the existing coalesced task
  refresh only after that mutation completes.
- A `--repo` launch switch queues its task and New Task snapshots through those background paths
  instead of awaiting either read while applying launch arguments.
- New Task snapshots now carry a per-project generation, so an A → B → A navigation cycle cannot
  hydrate A from the first request after the second request supersedes it.
- Settings snapshots and provider-usage loads now likewise apply only from the most recently
  queued settings request.
- GitHub aggregate refreshes now also carry a generation, so reopening the same project's screen
  cannot let an earlier request replace a newer snapshot.
- Draft PR creation now runs on the worker pool and preserves the follow-up thread refresh after
  its completion.
- CLI handoff terminal probing and launch now run on the worker pool; the existing failure notice
  is applied from its completion.
- Run activation now likewise leaves the frame task before taking the manager lock and starting its
  native runner worker.
- IDE editor handoff only resolves an absent project-registry root in the background and rejects
  that fallback after navigation away from the project.
- Exact queued duplicates of the safe task/index/new-task/model refreshes coalesce before a frame;
  mutations retain their original FIFO order and are never collapsed.

Required correction:

1. Give the TUI one bounded async command executor and typed completions. No engine or host I/O may
   be awaited from the render/input task.
2. Add a request key and generation to every route-derived load, not only task lists. Cancel or
   supersede stale project/thread/Git/IDE/GitHub/settings loads and reject completions whose full
   route key no longer matches.
3. Coalesce idempotent refreshes by key. Preserve ordered mutations, but do not enqueue duplicate
   refreshes behind each mutation.
4. Put per-frame item/time budgets on every receiver. Prioritize keyboard and terminal input,
   coalesce mouse-move and run-record updates, and request another wake when backlog remains.
5. Replace fixed `thread::sleep` frame pacing with event/tick selection so idle mode does not redraw
   continuously and busy mode does not sleep after overrunning its budget.

Acceptance:

- A scripted A -> B -> A navigation storm with slow out-of-order engine responses never applies a
  stale screen result and keeps p99 input-to-draw latency below 100 ms.
- A 10,000-event burst is consumed over bounded frames; quit and cancel remain responsive during
  the burst.
- The number of background workers remains bounded during 1,000 repeated refresh requests.

### R3 — Streaming causes write and rebuild amplification (critical)

Evidence:

- Every `RunManager::append_event` appends durable NDJSON and stamps `updated_at`; streaming index
  writes and run-record notifications debounce to a 250 ms boundary. Its 10,000-delta regression
  asserts fewer than 100 index writes and notifications after the explicit final flush.
- Some runner streams can emit fine-grained deltas (pi emits text deltas directly; protocol mappers
  also expose tool/reasoning/output updates).
- `ThreadUi::push_events` receives the frame's live batch and rebuilds once after accepting it,
  rather than rebuilding per event. A 1,000-event regression proves the batch performs one rebuild
  while retaining every event and its final sequence watermark.
- `runs_index` lazily opens a project only to seed its observer-maintained snapshot; routine
  refreshes sort that snapshot without taking a manager lock. A blocked-session regression proves
  the global index returns within 100 ms while the project's provider worker owns its manager.

Required correction:

1. Keep NDJSON append durable per semantic event, but debounce/coalesce `updated_at` index writes
   and run-record notifications. A lifecycle boundary must flush immediately; streaming activity
   may use a short bounded interval and must flush on shutdown/error.
2. Coalesce provider deltas into bounded semantic updates before persistence. Preserve final text,
   tool output, errors, and normalized event compatibility; do not persist one full-index rewrite
   per token.
3. Make thread reduction/projection incremental or batch all events received in a frame and rebuild
   once. Maintain stable item IDs and history-page prepend behavior.
4. Maintain a lightweight project index snapshot updated by run notifications instead of locking
   and re-sorting every project for routine global refreshes.
5. Add release benchmarks and counters for event append, index flushes, projection updates, queue
   depth, dropped/coalesced updates, frame time, and command latency. Logs must stay local and must
   not contain prompts, credentials, or raw provider payloads.

Acceptance:

- A 10,000-delta fixture performs a bounded number of `runs.json` rewrites, retains the exact final
  transcript, and survives an injected crash at every flush boundary.
- Appending N then 2N events demonstrates near-linear, not quadratic, projection cost.
- A 300-run project plus several registered projects can refresh without blocking input.

### R4 — Worktree mode is persisted but never executed (critical; implemented)

Evidence:

- README and New Task advertise optional isolated worktrees and mandatory isolation for competing
  variants.
- admitted worktree runs now persist `worktree_path`, branch, and base branch before execution;
  the runner, checks, diff inspection, handoff, and repository actions use that directory.
- temporary-Git integration coverage proves original-checkout isolation, runner cwd, variants,
  continuation affinity, and cleanup/retention behavior.

The correction and acceptance criteria below are retained as the completed audit trace.

Required correction:

1. Materialize the requested worktree before admitting the provider worker, using the existing Git
   helper and current branch vocabulary. Persist path/branch/base atomically before spawn.
2. Pass that directory consistently to runner cwd, check steps, diff inspection, handoff, open-in,
   commit/push, and continuation. A continued session cannot silently change its filesystem root.
3. Treat variants as isolated and fail their creation loudly if isolation cannot be created. A
   normal opted-in run may follow the documented non-Git degradation policy.
4. Roll back a partially-created worktree when pre-spawn setup fails, but never delete a worktree
   that may contain agent edits after execution begins.

Acceptance:

- A real temporary Git repository proves original-checkout isolation, branch spelling, runner cwd,
  parallel variants, continuation affinity, diff, commit, push seam, and cleanup/retention.

### R5 — Workflow checks and the review gate have no production execution seams (critical; implemented)

Evidence:

- README advertises shell `command` steps with bounded retry.
- production configuration injects bounded worktree-aware check execution and diff inspection.
- repository/environment review policy reaches `RuntimeOptions`, and normal run settlement uses
  the shared review path.

The correction and acceptance criteria below are retained as the completed audit trace.

Required correction:

1. Inject a production check executor using argument-array shell policy, bounded stdout/stderr,
   timeout, cancellation, and the run working directory. Persist the real exit status; do not
   reduce every failure to exit code one.
2. Inject a production diff inspector over the same run directory/base branch used by Git views.
3. Resolve repo/workspace/environment review policy at run admission and pass it into the runtime.
   Remove the variant-only duplicate settlement once the shared path is authoritative.

Acceptance:

- The README's `implement-and-verify` workflow passes, fails, retries, and cancels against real
  temporary commands.
- A changed successful run enters Review only when the effective gate requires it; unchanged and
  autonomous runs follow the documented policy.

### R6 — Selected agent accounts do not reach task processes (critical; implemented)

Evidence:

- account/profile CRUD, provider probes, project selections, and child-env tests exist.
- admission resolves the selected or project-default profile and passes only its provider-specific
  home override to the task child environment.
- focused concurrent-profile coverage proves task child processes observe their selected profile
  directory without copying credentials into durable run state.

The correction and acceptance criteria below are retained as the completed audit trace.

Required correction:

1. Resolve the explicit or project-default profile before provider spawn. Validate provider match
   and availability without reading or copying credentials.
2. Put the minimal profile home override in the session request and curated child environment.
3. Persist concrete runner/profile affinity per step. Resume only with the same provider/profile;
   fail clearly or start the already-specified fresh-session recovery path when affinity changed.
4. Keep missing profiles a localized run error and preserve zero-configuration default profiles.

Acceptance:

- Two mock Claude/Codex profiles used concurrently observe only their own config directory.
- project defaults, explicit task choice, continuation, and provider-status probes agree on the
  resolved profile.

### R7 — Several visible resource settings are read/write-only (high)

Evidence:

- workspace/project Settings expose `maxParallel`, `maxMonitoringSessions`, monitoring wake
  interval, auto-resume, intelligent context refresh, and memory limits.
- one tested effective-runtime-options resolver combines workspace resource limits, project
  overrides, repository configuration, and the retained `DUCK_REVIEW_GATE` compatibility override
  before production-manager construction. It also applies intelligent-context refresh, the check
  executor, and diff inspector; the cross-project workspace scheduler and repository lease remain
  unwired.
- monitoring-session admission is enforced at runtime. Memory limits and monitoring wake
  configuration have no runtime enforcement/scheduler in this build and are rendered as explicitly
  unavailable with their reason rather than as active policy.
- Saving repository or workspace policy now reconfigures every already-open manager for future
  admissions while preserving active sessions and their established cwd/profile/reservations.
- a table-driven Settings regression names every retained resource control and asserts the two
  unsupported controls visibly carry the unavailable marker, preventing a silent UI regression.

Required correction:

1. Make one documented effective-runtime-policy resolver combine workspace defaults, project
   overrides, repository config, and `DUCK_*` compatibility overrides.
2. Wire every retained Settings control into admission, worker setup, scheduling, or retention.
   If a control cannot be honored on a supported platform, render it unavailable with a reason;
   never accept and silently ignore it.
3. Reconfigure safe future admissions after Settings changes. Do not mutate the cwd, profile, or
   hard limit of an already-running session invisibly.
4. Coordinate the quota/auto-resume portion with
   [Intelligent automatic routing](intelligent-auto-routing.md); do not create a second router or
   contradictory scheduling vocabulary.

Acceptance:

- A table-driven conformance suite proves each contract/Settings field has a production consumer
  or an explicit unavailable state.

### R8 — Runner protocol coverage has drifted behind installed CLIs (high; partially implemented)

Evidence:

- Codex handles command, file-change, and permission-profile approvals through the durable
  permission flow. Permission-profile grants are limited to the exact requested object and a
  16 KiB bound; malformed forms are declined. Unsupported MCP elicitations and dynamic-tool calls
  receive their required explicit decline shapes; every other client-directed request receives a
  non-hanging JSON-RPC error.
- A mock malformed native `requestUserInput` request now proves the adapter returns `-32602`, emits
  a normalized non-fatal error, and allows the provider turn to settle rather than waiting forever.
- A mock unknown approval request likewise proves the generic approval fallback returns `-32601`
  and cannot leave the provider waiting for an unimplemented approval surface.
- the schema also exposes dynamic tool, MCP progress, terminal-interaction, and richer item shapes.
  The generic tool fallback retains some JSON, but parity fixtures do not prove these current
  shapes remain usable.
- Claude stream-json sessions request `--forward-subagent-text`, preserving nested activity.
- the current Claude approval opt-in changes `dontAsk` to `acceptEdits`; it does not implement the
  durable request/answer contract described for recoverable approvals.

Required correction:

1. Add a generated-schema/fixture inventory for every provider request that requires a client
   response. Every such request must be answered, durably parked for user input, or explicitly
   declined with a non-hanging normalized error.
2. Normalize current Codex permission and MCP elicitation requests through the existing durable
   ask/permission UI. Bound and validate forms; secrets and unsupported schemas fail closed.
3. Enable and map Claude's forwarded subagent stream while retaining parent IDs and bounded
   output. Characterize provider-native questions/permissions in headless mode before claiming
   recoverable support; otherwise document the precise degraded behavior.
4. Build the same capability matrix for OpenCode and pi: first/follow-up turn, built-in tools,
   shell/PTY operations, MCP/custom tools, delegation, question/permission, image, plan, usage,
   cancellation, timeout, resume, and teardown. A missing provider wire may degrade one cell, not
   the whole runner, but the product must not claim support it does not have.
5. Keep provider wire types at the runner seam and commit golden fixtures. Never couple runtime
   logic to one locally installed version.

Acceptance:

- No committed provider-request fixture can leave its mock process waiting for an unanswered RPC.
- Nested Codex and Claude subagent activity has stable parentage, bounded depth/output, and a final
  state even when a child fails.

### R9 — Process and worker lifetime bookkeeping leaks or is unowned (high; partially implemented)

Evidence:

- `DefaultSessionFactory` now wraps opened sessions in an RAII registration that deactivates and
  removes the matching cancellation token on close, open failure, or drop.
- a 1,000-cycle registration regression test proves dropped sessions return the cancellation
  registry to its baseline without retaining inactive tokens.
- activation workers coalesce per project and now globally reap completed handles; TUI background
  work is dispatched through a fixed four-thread pool rather than creating per-request threads.
  Both paths still lack a cancellation registry and bounded shutdown escalation.
- child stdout, stderr, and discard readers are owned by `ChildProcess`; termination reaps the
  child and joins readers after bounded escalation.

Required correction:

1. Register cancellation with an RAII session lease and remove/deactivate it on every terminal,
   open-failure, and drop path.
2. Own all long-lived worker and reader handles under the coordinator/session. Reap children after
   bounded escalation and join readers when pipes close.
3. Expose bounded worker/process counts to internal diagnostics and add a repeated lifecycle soak
   test.

Acceptance:

- 1,000 mocked open/cancel/finish cycles return cancellation, worker, reader, and child counts to
  baseline.

### R10 — Run-state durability violates repository invariants (high; implementation complete, fault coverage ongoing)

Evidence:

- `RunIndexLoad` distinguishes missing, valid, per-entry-salvaged, and corrupt `runs.json` state.
  Salvaged/corrupt state warns once at manager startup and quarantines mutations, preserving the
  original bytes until an explicit repair.
- `RunRecord` and nested step values preserve unknown JSON fields through a read-modify-write;
  regression coverage proves both levels survive.
- index writes use a collision-safe staging name, owner-only permissions, data sync before rename,
  and a best-effort directory sync afterwards. Explicit repair copies the original bytes to an
  owner-only backup before replacing the index; a Unix regression verifies the backup mode is
  `0600`.
- production manager construction invokes terminal-record retention. It atomically removes only
  inactive terminal records, then best-effort cleans matching NDJSON and handoff sidecars while
  leaving worktrees recoverable.
- the terminal-native `repair-runs` command opens the affected project's manager, runs the explicit
  repair, and prints the backup path. Its headless regression test proves corrupt bytes are backed
  up before the repaired index is accepted.

The implementation is complete. The remaining verification work is the fault-injection matrix in
the acceptance criteria, especially concurrent-writer, permission, disk-full, and
crash-between-write/rename cases. The pre-rename crash boundary is now covered deterministically:
the old index remains readable and the staging file is removed when the injected boundary fails.
An injected repair-replacement failure likewise leaves the corrupt original and its owner-only
backup intact.

Required correction:

1. Return a typed load outcome: missing, valid-with-salvage, or corrupt. Preserve valid entries
   individually, warn once, leave corrupt bytes in place, and quarantine writes until an explicit
   recoverable repair/backup path is complete.
2. Preserve unknown JSON keys at every nested persisted shape through mutations.
3. Use collision-safe atomic temporary files in the target directory, fsync where required for the
   stated durability guarantee, and enforce owner-only permissions for per-user state.
4. Invoke retention transactionally, including the matching NDJSON/image/handoff files, without
   deleting active or unrecoverable worktrees.
5. Replace production `unwrap`/`expect` sites, including persistence serialization and forge cache
   locks, with typed degradation. Fixed compile-time regex construction may use an explicitly
   documented infallible helper if the repository rule is amended; otherwise remove those panics
   too.

Acceptance:

- unknown-key, one-bad-entry, truncated-file, permission, concurrent-writer, disk-full, and
  crash-between-write/rename fault tests pass without destroying the original state.

### R11 — Dead thread UI and duplicated build targets should be removed (medium; implemented)

Evidence:

- obsolete thread docks, toggles, state, and snapshots were reference-searched and removed; the
  shipped thread view is a single timeline.
- `coducktor` and `duck` are trivial wrappers around the shared library entry point, with the
  short alias excluded from Cargo binary tests so all-target testing runs the suite once.

The correction and acceptance criteria below are retained as the completed audit trace.
- `in_process.rs` is about 10,900 lines, `app.rs` about 4,500, and `main.rs` about 2,460. This does
  not itself prove a runtime defect, but it obscures ownership boundaries exposed by R1/R2/R7 and
  makes dead wiring harder to detect.

Required correction:

1. Reference-search and delete the obsolete docks, toggles, state, and snapshots. Do not revive the
   superseded dual-view design.
2. Build one real binary entry point and produce the `duck` alias at install/package time, or move
   shared main logic into one library entry with only trivial wrappers and disable duplicate bin
   tests. Preserve both shipped command names.
3. As the runtime is changed, split `in_process` by capability adapters and split TUI orchestration
   into command scheduling, completion reduction, and loop modules. Do this as behavior-preserving
   moves with reference searches, not a standalone rewrite.

Acceptance:

- no production `allow(dead_code)` remains without a written compatibility reason;
- workspace all-target tests execute the binary test suite once; and
- both installed command names report the same version and behavior.

### R12 — Interactive CLI handoff is Linux-only (medium; implementation complete, terminal verification ongoing)

Evidence:

- resume command construction uses the validated open-target abstraction on Linux, macOS, and
  Windows, retaining command and session IDs as separate argument values.
- fixture coverage characterizes those platform commands; real interactive results are recorded
  as they are exercised in `docs/tui/terminals.md`.

The code correction and argument-construction acceptance criterion are complete; terminal
verification remains ongoing on available platforms.

Required correction:

1. Reuse the validated open-target abstraction for task resume commands on all supported
   platforms. Keep command/session IDs argument-safe and never interpolate untrusted text into a
   shell command.
2. Characterize each provider's current resume command with integration fixtures and degrade with
   a precise unsupported-target reason.

Acceptance:

- argument-construction tests cover Linux, macOS, and Windows without launching a real GUI; real
  interactive terminal results are then recorded in `docs/tui/terminals.md` on available systems.

## Delivery order

Implement this as small, reviewable commits on `main`. Do not mix behavior changes with file moves.

### Phase 0 — Reproduce and instrument

1. Add deterministic blocked-session, navigation-storm, event-burst, and repeated-lifecycle test
   harnesses.
2. Record baseline command latency, frame latency, index writes/bytes, projection time, worker
   count, and child count in release mode.
3. Add a production-wiring conformance test that fails for R4-R7 before changing behavior.

Exit: the reported unresponsiveness and false controls are represented by failing tests or a
repeatable benchmark, not manual timing alone.

### Phase 1 — Fix ownership and UI scheduling

Implement R1, then R2 and R9. Keep the existing `Engine` trait and in-process production engine;
do not add a service, socket, browser, or database.

Exit: blocked sessions cannot block reads/input/cancel, parallel admission is real, and worker
counts are bounded.

### Phase 2 — Make advertised execution real

Implement R4, R5, R6, and the runtime-policy resolver in R7. Land worktree setup first so every
subsequent executor receives the authoritative cwd.

Exit: end-to-end tests prove worktrees, variants, checks/retries, review settlement, account homes,
and effective resource settings through the `Engine` boundary.

### Phase 3 — Remove streaming amplification

Implement R3 after the coordinator owns flush/shutdown boundaries. Preserve NDJSON compatibility
and exact transcript output.

Exit: event and projection benchmarks meet their scaling assertions and the TUI storm test stays
responsive with several live tasks.

### Phase 4 — Harden state and protocol compatibility

Implement R10, then R8 and R12. Generate fixtures from current schemas/help but hand-review and
commit only bounded sanitized inputs. Coordinate unfinished automatic-routing work with its
existing spec.

Exit: corruption/fault tests pass, every client-directed provider request is answered, and the
capability matrix accurately describes all four runners.

### Phase 5 — Delete superseded code and reduce build waste

Implement R11 and behavior-preserving module extraction. Re-run reference searches immediately
before every deletion.

Exit: no superseded dock state remains, command aliases still ship, and compile/test duplication is
removed.

## Verification gate

Run focused crate tests and release benchmarks during each phase. At the end run:

```text
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo tree --workspace
```

Also run the new blocked-session/navigation/event/lifecycle soak suite under a timeout and inspect
affected `insta` snapshots rather than accepting them blindly. Interactive terminal behavior must
be tested in a real terminal and recorded in `docs/tui/terminals.md`; headless output is not
evidence for it.

## Completion definition

This plan is complete only when:

1. a running provider cannot make navigation, list/history reads, cancel, or quit unresponsive;
2. configured parallelism works within one project and across projects;
3. worktree, check, review, account, and retained resource settings have end-to-end production
   tests;
4. streaming work is bounded in disk writes, CPU, memory, and per-frame processing;
5. corrupt or future-compatible state is not destroyed by a read-modify-write;
6. all provider requests requiring a client answer terminate through an answer, explicit decline,
   cancellation, or typed error; and
7. dead/superseded code and duplicate test compilation are removed without changing the shipped
   terminal product.
