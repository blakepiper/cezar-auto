# Codebase reliability and core-functionality remediation

Status: implementation plan based on a repository audit on 2026-08-18

Scope: defects, incomplete production wiring, dead code, durability, and measured performance.
This plan does not propose new product features. It makes already-advertised behavior work, keeps
the cockpit responsive under its existing workload, and removes implementation that no longer has
a caller.

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
- Refreshes use detached OS threads that call `Handle::block_on`; they have no concurrency bound,
  cancellation, request registry, or shutdown ownership.
- terminal input, workspace events, background results, and thread events are drained with
  unrestricted `while try_recv()` loops in one frame.

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

- Every `RunManager::append_event` appends NDJSON, stamps `updated_at`, pretty-serializes the entire
  project `runs.json`, atomically renames it, publishes a full cloned run record, and then publishes
  the event.
- Some runner streams can emit fine-grained deltas (pi emits text deltas directly; protocol mappers
  also expose tool/reasoning/output updates).
- Each live `ThreadUi::push_event` calls `rebuild`, which reduces the complete event vector,
  re-projects all turns, rebuilds all transcript items, and reconciles them. Repeating this once per
  event is quadratic over a long stream even though final painting is virtualized.
- `runs_index` serially opens/locks every project and sorts up to 200 records from each on every
  refresh.

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

### R4 — Worktree mode is persisted but never executed (critical)

Evidence:

- README and New Task advertise optional isolated worktrees and mandatory isolation for competing
  variants.
- `CreateRunInput.worktree` reaches the run record, but production construction never calls
  `git::worktree::create_worktree` or assigns `worktree_path`, `branch`, and `base_branch`.
- `SessionRequest.cwd` is always `RunManager::repo_root()`. The worktree APIs in the client only
  inspect or remove paths that some other path would already have created.

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

### R5 — Workflow checks and the review gate have no production execution seams (critical)

Evidence:

- README advertises shell `command` steps with bounded retry.
- `RunManager` fails a command step with `check executor unavailable` unless a `CheckExecutor` is
  injected; only tests inject one.
- Review settlement requires an injected `DiffInspector`; only tests inject one. Production's
  manager also keeps default `RuntimeOptions { review_gate: false }`. The client contains a special
  review workaround only while picking a variant, not normal completion.

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

### R6 — Selected agent accounts do not reach task processes (critical)

Evidence:

- account/profile CRUD, provider probes, project selections, and child-env tests exist.
- a run stores `agent_profile`, but `SessionRequest` has no resolved profile environment and
  `to_agent_run_spec` always gets an empty `env` map.
- profile-specific `CLAUDE_CONFIG_DIR`/`CODEX_HOME` is only applied by status/model probes, not by
  the task session factory. The persisted choice is therefore cosmetic during execution.

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
- production manager construction calls `set_project_id` but not `set_runtime_options`,
  `set_workspace_semaphore`, `set_repository_lease`, `set_intelligent_context_refresh`,
  `set_check_executor`, or `set_diff_inspector`.
- `busy_slots(..., max_monitoring_sessions)` and stale-index selection exist but have no production
  caller. Memory limits and monitoring wake configuration are parsed and rendered with no runtime
  enforcement/scheduler in this codebase.

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

### R8 — Runner protocol coverage has drifted behind installed CLIs (high)

Evidence:

- the Codex 0.147.0 generated schema includes server requests for
  `item/permissions/requestApproval` and `mcpServer/elicitation/request` in addition to the two
  approval methods and `item/tool/requestUserInput` currently handled. Unknown requests currently
  receive no general response; unsupported approval methods return a protocol error and keep the
  provider turn moving without a recoverable user decision.
- the schema also exposes dynamic tool, MCP progress, terminal-interaction, and richer item shapes.
  The generic tool fallback retains some JSON, but parity fixtures do not prove these current
  shapes remain usable.
- Claude 2.1.233 supports `--forward-subagent-text` for stream-json sessions. Coducktor does not
  request it, so delegated child work cannot expose the provider's nested text/thinking stream.
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

### R9 — Process and worker lifetime bookkeeping leaks or is unowned (high)

Evidence:

- `DefaultSessionFactory.cancellations` inserts one token per opened run and never removes it.
- activation workers and TUI background threads are detached. There is no central count, join,
  cancellation, or error reporting path.
- child stdout/discard threads are detached; `ChildProcess::Drop` kills without waiting/reaping,
  and the stdout reader handle is not owned. These choices can accumulate threads/zombies under
  repeated start/cancel cycles even when the child itself is killed.

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

### R10 — Run-state durability violates repository invariants (high)

Evidence:

- malformed `runs.json` or one invalid entry loads as an indistinguishable empty list. The next
  mutation can overwrite the corrupt file; there is no one-time warning or write quarantine.
- `RunRecord` has no flattened unknown-field map, so a compatible read followed by any write drops
  future top-level and step keys.
- the index uses a fixed `.tmp` file and does not set `0600`, despite per-user project state and the
  repository's atomic/private read-modify-write requirement.
- `select_stale_run_ids` and run/event retention limits are tested but never invoked, compounding
  index-write and load costs.

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

### R11 — Dead thread UI and duplicated build targets should be removed (medium)

Evidence:

- `render_plan_dock`, `render_agents_dock`, and `collect_subagents` are hidden behind
  `#[allow(dead_code)]`, but `ThreadAction::TogglePlanDock`/`ToggleAgentsDock` and associated state
  remain reachable code with no rendered dock caller. The current task spec intentionally uses one
  timeline, so this is superseded implementation, not a missing new view.
- `coducktor` and `duck` point at the same `src/main.rs`, causing Cargo to compile and run the same
  binary tests twice under `--all-targets`.
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

### R12 — Interactive CLI handoff is Linux-only (medium)

Evidence:

- general project open-target support has macOS/Windows/Linux branches, but
  `open_terminal_for_command`, used by task `open_in_cli`, immediately returns false off Linux.
- this means the existing “open this task in its native agent CLI” action cannot work on two
  supported desktop families even when a terminal is installed.

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
