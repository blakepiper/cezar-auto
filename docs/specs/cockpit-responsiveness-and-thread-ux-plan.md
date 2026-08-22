# Cockpit responsiveness and thread UX — implementation specification

Status: ready for implementation (written 2026-08-22, decisions closed 2026-08-22)

Audience: the next implementation agent. Work directly on `main`, preserve unrelated changes,
commit the completed work, and push `origin main` as required by `AGENTS.md`.

## Purpose and scope

The cockpit is unusable for its primary loop. Sending a follow-up freezes the whole application
for the length of the agent's reply; the transcript degrades continuously as a task runs;
`waiting for your answer` is reported for turns nobody needs to answer; and agent prose is
rendered with the same treatment as `Sending…`.

This is a responsiveness and honesty remediation, not a product redesign. Retain the single
in-process `Engine`, the terminal UI, local durable files, and the existing runner seam. Add no
service, socket, browser, database, network API, or new environment-variable vocabulary. Every
change named here is local to code that already exists and is already tested.

Two findings below correct entries the 2026-08-18 reliability audit
(`codebase-reliability-remediation-plan.md`) records as complete. Read
[Corrections to the prior audit](#corrections-to-the-prior-audit) before trusting R1 or R3.

## Finding inventory

| ID | Finding | Priority | Evidence |
| --- | --- | --- | --- |
| F1 | `send_message` holds the manager mutex across a live provider turn | P0 freeze | measured |
| F2 | The thread re-projects from sequence zero and drops its height cache every frame | P0 freeze | measured |
| F3 | Four blocking worker threads amplify one slow call into a total stall | P0 freeze | inspection |
| F4 | `RunStatus::Waiting` conflates "asked you a question" with "turn ended" | P0 wrong status | inspection |
| F5 | `derive_status` reports `Completed` mid-turn | P1 wrong status | inspection |
| F6 | Live events are dropped on lag with no gap detection or resync | P0 data loss | inspection |
| F7 | Agent prose is demoted to a dim note and reordered to the turn tail | P1 legibility | inspection |
| F8 | Tool cards and prose share one foreground; `ToolKind` is computed and discarded | P1 legibility | inspection |
| F9 | A thread opens in a navigator where `a` archives and `f` finishes | P1 interaction | inspection |
| F10 | No key is bound to `ThreadAction::Cancel` | P1 interaction | inspection |
| F11 | "Enter sends guidance to the active turn" is not implemented | P1 interaction | inspection |
| F12 | A panic under the manager lock poisons it for the process lifetime | P2 resilience | inspection |

## Measured evidence

Both measurements came from throwaway tests written against `539eef85` and removed afterwards.
Phase 0 lands them as permanent tests; the numbers below are the baseline they must beat.

### F1 — an unrelated engine read blocked for the remaining turn duration

An integration test built `InProcessEngine::with_session_factory_at` over a session that parks on
`turn()` (`SessionOutcome::Waiting`, no marker) and sleeps 1.5s in `send_message()`. A follow-up
was sent from a background thread; 300ms later the main thread timed an unrelated read:

```text
run_history blocked for 1.203047594s
archive_run blocked for 690.241µs
```

`run_history` blocked for exactly the remaining turn. `archive_run` returned quickly only because
by then the turn had ended. With a real agent this is 30 seconds to several minutes of a dead
application — including `cancel_run` for any run other than the one holding the lock.

### F2 — one frame costs 25ms at 12,000 events

A test drove `ThreadUi::push_events` in 8-event batches (matching the runtime loop) with
`Transcript::render` after each, using realistic v2 item shapes: tool calls with 80-line output,
streamed markdown prose. Release build. Columns are the cost of the single *next* frame at that
thread size:

| events | items | projection | render | frame | rebuilt_events |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 200 | 7 | 0.10ms | 1.02ms | 1.2ms | 2,600 |
| 1,000 | 32 | 0.30ms | 1.81ms | 2.1ms | 63,000 |
| 3,000 | 92 | 0.98ms | 4.82ms | 6.2ms | 564,000 |
| 6,000 | 183 | 2.55ms | 9.50ms | 12.1ms | 2,253,000 |
| 12,000 | 365 | 6.53ms | 18.51ms | 24.6ms | 9,006,000 |

`FRAME_BUDGET` is 33ms (`crates/coducktor-tui/src/runtime.rs:30`). At 12,000 events — only 365
visible items — a frame costs 24.6ms of CPU in an optimized build. Around 16,000 events the budget
is gone. Total CPU spent re-projecting that one thread across its life: 18.5 seconds.

Rendering a second frame with no intervening `reconcile` costs **0.34ms** instead of 18.51ms.
That 54x penalty is paid on every frame during live streaming, purely because `reconcile` resets
the height cache.

## Corrections to the prior audit

### R1 "provider turn monopolizes a manager — complete" is incomplete

`TurnDispatch` correctly runs *admitted* turns with no manager lock held, and
`InProcessEngine::send_message` does drain `take_pending_turns` after its lock section. But
`RunManager::deliver_message` (`crates/coducktor-core/src/workflows/run/mod.rs:3755-3783`) still
calls `active.session.send_message(...)` inline on `&mut self` — that is, under the caller's lock
— and `InProcessEngine::send_message`
(`crates/coducktor-client/src/in_process.rs:3807-3814`) calls it while holding
`self.manager.lock()`. The follow-up path never went through the dispatcher. `RunManager::finish`
(`mod.rs:3648-3690`) has the same shape, blocking in `process.wait_for_exit()`.

`a_three_hundred_run_project_refreshes_without_waiting_on_a_blocked_sibling` passes because
`runs_index` reads `run_snapshot` and never takes the manager lock. It proves that one path is
safe; it does not constrain the rest.

### R3 "doubling accepted events does not quadruple rebuild time — complete" cannot detect the defect

`doubling_accepted_events_does_not_quadruple_rebuild_time`
(`crates/coducktor-tui/src/screens/thread/mod.rs:1513`) pushes all N events in a *single*
`push_events` call, producing exactly one rebuild folding N events. That is linear by
construction regardless of the underlying algorithm.

The runtime never does this. `runtime.rs:3202-3237` drains up to `RECEIVER_ITEMS_PER_FRAME`
events per frame and calls `push_events` once per frame, so a stream of N events produces
O(N/batch) rebuilds each folding a growing prefix — quadratic. The measured
`rebuilt_events = 9,006,000` for 12,000 accepted events is that quadratic sum. Any replacement
test must feed events in frame-sized batches.

## Findings

### F1 — the manager mutex is held across a live provider turn (P0)

`InProcessEngine::send_message` (`in_process.rs:3807-3814`) takes `self.manager.lock()` and holds
it across `RunManager::deliver_message`, which blocks in
`ClaudeSession::send_message` → `read_until_turn_end`
(`crates/coducktor-runners/src/claude_runner.rs:515-526`) for the whole turn. Every engine method
that needs the manager waits behind it.

Second site: `finish_run` (`in_process.rs:1805-1815`) holds the lock across `RunManager::finish`,
which calls `session.finish()` → `process.wait_for_exit()`. Third: `run_history`
(`in_process.rs:4138-4152`) takes the blocking lock purely to check a run exists, so *opening a
thread* stalls. `archive_run`/`delete_run` (`in_process.rs:1669-1690`) take it unconditionally.

The correct pattern already exists and documents itself: `TurnDispatch::run`
(`in_process.rs:427-470`) — "No branch of this function ever holds the manager's lock across the
session calls themselves, only around the brief, individual state mutations each one produces."

### F2 — the thread re-projects from zero and drops its caches every frame (P0)

Three compounding layers:

1. `ThreadUi::push_events` (`thread/mod.rs:267-280`) calls `rebuild()` on any frame carrying a new
   event. `rebuild()` (`thread/mod.rs:310-363`) runs `reduce_thread`
   (`thread/reducer.rs:203`) over the entire event vector from sequence zero.
2. `build_transcript_items` (`thread/mod.rs:395-570`) constructs a fresh
   `Vec<TranscriptItem>`, cloning every message and tool output and giving each new `MessageItem`
   an empty `RenderCache` — so the markdown cache never survives a frame.
3. `Transcript::reconcile` (`widgets/transcript.rs:551-583`) matches old to new with a linear scan
   per item (O(n²) string compares), then does `self.height_cache = HeightCache::default()`.
   The next `total_height()` (`transcript.rs:693-699`) re-measures every item cold, re-parsing
   every message's markdown.

`benches/transcript.rs` renders a static 5,000-item transcript with a warm height cache and calls
it "steady state". During live streaming the cache is never warm. `ThreadProjectionMetrics`
already records `rebuild_time` and `rebuilt_events`; nothing reads them outside tests.

### F3 — the worker pool amplifies any slow call (P0)

`BACKGROUND_WORKER_COUNT = 4` (`runtime.rs:34`) native threads each run
`handle.block_on(future)` (`runtime.rs:474-505`). Four concurrent slow calls consume the pool;
the next `BACKGROUND_QUEUE_CAPACITY = 128` queue silently; beyond that the user gets
"background command queue is full; please retry". Combined with F1, one follow-up parks four
workers on the same mutex and the data layer goes dark.

### F4 — `Waiting` means two different things (P0)

`RunManager::park_session` (`workflows/run/mod.rs:3030-3085`) sets `RunStatus::Waiting` whenever a
turn ends and the run neither holds a slot nor is monitoring — the ordinary end of a turn. The
marker layer already distinguishes the cases (`TurnMarkerDecision` in
`workflows/run/session.rs:10-17` has `Ask`, `Waiting`, `Done`, `Monitoring`, `Closed`), but the
call site (`mod.rs:3228-3240`) branches only on `Monitoring` and collapses the rest.

That one status drives the whole attention model. Every ordinary turn end therefore labels the
task `needs you` with a pulse (`screens/runs_util.rs:69-113`), fires a desktop notification
titled "Needs your answer" and plays a sound (`app.rs:584-598`), increments the terminal-title
counter (`app.rs:1967-1972`, `runtime.rs:3264-3268`), sorts into `NEEDS YOU`
(`app.rs:576-582`), and titles the composer `ANSWER` (`thread/mod.rs:874-885`).

### F5 — `derive_status` reports `Completed` mid-turn (P1)

`derive_status` (`screens/thread/projection.rs:584-612`) checks a running activity node, then a
pending question, then `if turn.response.text is non-empty → "Completed"`. That third branch never
consults `status`. Mid-turn, after the agent has streamed final-answer text but between tool
calls, the header reports `Completed` on a `Running` run.

### F6 — live events are dropped on lag with no resync (P0)

One `broadcast::channel(512)` (`in_process.rs:901`) carries every event of every topic — health,
workspace, and the delta firehose of every running task. `subscribe()`
(`in_process.rs:2519-2531`) filters *after* the receiver, so the workspace listener must drain the
run-event stream and vice versa. On lag, `BroadcastStream` yields `Lagged(n)` and the code
discards it with `.filter_map(|item| item.ok())`.

`ThreadUi::push_events` skips `seq <= as_of_seq` but never notices a gap. A dropped batch means
text that never appears, tool cards stuck on `running` forever, and task rows frozen on a stale
status, with no error and no resync until the thread is reopened. The TUI drains at most
`RECEIVER_ITEMS_PER_FRAME = 256` events per receiver per frame within a
`RECEIVER_TIME_BUDGET = 4ms` (`runtime.rs:31-32`), so falling behind is expected under load.

### F7 — agent prose is styled as system chatter and reordered (P1)

In `build_transcript_items` (`thread/mod.rs:455-462`), an assistant message that is not the turn's
designated final response becomes a `NoteItem` with `NoteTone::Dim` — rendered as `· text` in
`soft_fg` with no markdown (`transcript.rs:375-385`). That is identical treatment to
`Plan 3/5 complete`, `image: …`, `Sending…` and lifecycle notes.

All agent prose in a turn is then appended at the tail after every tool call
(`thread/mod.rs:544-552`), flattening the narrate → act → narrate rhythm into a wall of tools
followed by a wall of text. The justifying comment cites a real cause: runners open the
final-answer item before dispatching the turn's tools.

### F8 — tool cards and prose share one foreground (P1)

`paint_message` (`transcript.rs:324-341`) renders assistant text in `palette.fg` with no prefix,
gutter or role marker. `paint_tool_card` (`transcript.rs:387-455`) renders its title in
`palette.fg` + `BOLD`. Markdown headings and bold spans in agent prose are also `fg` + `BOLD`.
Items are painted back to back with zero vertical separation (`transcript.rs:719-800`).

`tool_display()` (`crates/coducktor-protocol/src/tool_display.rs`) already computes a `ToolKind`
(`Read`, `Edit`, `Search`, `Execute`, `Task`, `Plan`, `Fetch`, `Delete`, `Move`) and the renderer
uses it for exactly one thing: whether to print an exit code. `ThemePalette`
(`theme.rs:80-95`) already carries `accent`, `running`, `done`, `failed`, `review`, `add`, `del`.

### F9 — the thread opens in a destructive navigator (P1)

`ThreadUi::load` (`thread/mod.rs:183`) sets `focus = ThreadFocus::Transcript` on every entry, so
opening a task — or navigating back — lands in a vim-style navigator, not the composer. In that
mode (`thread/mod.rs:927-1000`) `a` = Archive, `f` = Finish, `R` = reload history,
`G` = jump to bottom. Typing a reply without pressing `i` first triggers destructive actions.

### F10 — no keyboard interrupt (P1)

`ThreadAction::Cancel` exists and works (`thread/mod.rs:1317-1322`) but nothing in `handle_key`
is bound to it. In transcript focus `Esc` does nothing; in the composer `Esc` only blurs
(`thread/mod.rs:1117`); `Ctrl+C` falls through to the app shell (`thread/mod.rs:995`).
`default-keymap.toml` has no thread-level bindings. Cancel is reachable only by clicking the
header or opening a row menu.

### F11 — mid-turn sending is advertised, not implemented (P1)

While a run is `Running` the hint reads "Agent is working · Enter sends guidance to the active
turn" (`thread/mod.rs:862`). But the turn worker takes ownership of the session at dispatch
(`workflows/run/mod.rs:2917`, `Ok(self.active.remove(run_id))`), so `deliver_message`'s
`self.active.get(run_id)` (`mod.rs:3762`) finds nothing and returns `Ok(false)`, which the engine
surfaces as `Conflict { reason: "session closed" }` (`in_process.rs:3817-3819`). The composer
flips to `SEND FAILED · RETRY` (`thread/mod.rs:230`). `queued_messages` only fold into the prompt
at dequeue time (`mod.rs:348-360`), before the run starts.

### F12 — a panic under the manager lock is permanent (P2)

The manager is a `std::sync::Mutex`. Any panic while it is held poisons it and every subsequent
`lock()` returns `Err` for the rest of the process — every engine call fails. The crate correctly
bans `unwrap`/`expect` in production paths, but direct slice indexing such as
`active.workflow.steps[active.step_index]` appears on lock-held paths
(`workflows/run/mod.rs:3614`, `:3675`, `:3765`) and can panic on a malformed or migrated record.

## Implementation phases

Each phase is independently shippable and ends with a gate that must pass before the next begins.
Run the `SDLC.md` validation gate before each commit.

### Phase 0 — instrumentation and guardrails

No behavior changes. This phase makes the next five provable.

1. **Benchmark the real path.** Add `crates/coducktor-tui/benches/thread_frame.rs` driving
   `ThreadUi::push_events` in frame-sized batches with a render after each, at 1k/6k/12k/30k
   events, using realistic v2 item shapes. Report projection and render time separately. Keep the
   existing `transcript` bench; relabel its "steady state" case as the static-scroll case it is.
2. **Make the frame budget a test.** Assert one live frame at 12,000 events stays under 8ms. It is
   25ms today, so land it `#[ignore]`d with the measured number in the message; un-ignore at the
   end of Phase 1.
3. **Replace the R3 scaling test.** `doubling_accepted_events_does_not_quadruple_rebuild_time`
   must feed events in frame-sized batches, not one batch, and assert on `rebuilt_events` as well
   as `rebuild_time`. It will fail until Phase 1.4.
4. **Write down the lock invariant and enforce it.** Add to `AGENTS.md` under "Safety and quality
   rules": *no engine method may hold the `RunManager` mutex across an `AgentSession` call or any
   child-process wait.* Add an integration test in `crates/coducktor-client/tests/` using a
   `SessionFactory` whose `send_message` and `finish` sleep 1s, asserting a concurrent
   `run_history`, `list_runs`, `get_run`, `archive_run` and `cancel_run` each return under 100ms.
5. **Surface the metrics that exist.** Add a `DUCK_DEBUG_HUD=1` status-bar readout showing frame
   ms, projection ms, events reduced, and dropped-event count (from Phase 2). Document it in
   `.env.example`. Diagnosing "it froze" must not require a rebuild.

**Gate.** New benches run. Tests 0.2, 0.3 and 0.4 fail with the measured numbers recorded in the
commit message. Correct the R1 and R3 rows in `codebase-reliability-remediation-plan.md` to point
here.

### Phase 1 — unfreeze (F1, F3, F2)

1. **Move follow-up delivery off the lock.** Restructure `RunManager::deliver_message` the way
   `TurnDispatch::run` already works: a lock-held *detach* that appends the user message, removes
   the session from `self.active` and marks the run in flight; the unlocked
   `session.send_message(...)` with per-event `apply_turn_event` re-acquiring the lock briefly,
   exactly as `TurnDispatch::apply_event` does; a lock-held *reattach* applying the outcome. Then
   have `InProcessEngine::send_message` return as soon as the message is durably appended and the
   worker dispatched — the admission-then-dispatch shape `start_run` already uses. The UI's
   optimistic `pending_prompt` path already handles a delivered-but-unanswered message.
2. **Same for finish; stop blocking reads.** Apply the identical split to `RunManager::finish`.
   Change `run_history`'s existence check to read `run_snapshot` instead of the manager lock.
   Audit every remaining `self.manager.lock()` in `in_process.rs` and convert read-only uses to
   the snapshot.
3. **Split the worker pool.** With 1.1 and 1.2 done, no engine method blocks for a turn, but the
   `spawn_blocking`-backed filesystem and Git work in `in_process.rs` still can. Keep
   `BackgroundWorkers` rather than moving to `tokio::spawn` — the crate comment at
   `runtime.rs:426-432` records why the native pool exists, and that reasoning still holds for
   shutdown. Split it in two: a read pool sized for the refresh fan-out (list/get/history/index/
   diff) and a smaller mutate pool (start/send/finish/cancel/git). A saturated read pool must never
   delay a mutation, and a full queue must surface as a visible error, never a silent stall.
4. **Incremental thread projection.** Give `reduce_thread` an incremental form: keep the
   `ThreadState` and its `items_by_key` index on `ThreadUi` and fold only newly arrived events.
   Full re-fold stays for `load` and `merge_earlier` (which prepends). Target: `rebuilt_events`
   grows linearly with events received. Today a 12,000-event thread reduces 9,006,000 events.

   **Prerequisite — move marker stripping out of the fold.** `reduce_thread` ends with a
   destructive post-pass over every turn (`reducer.rs:702-723`) that rewrites
   `message.text = strip_done_marker(&message.text, strip_ask)`, where
   `strip_ask = options.active_turn && index == last_index`. Both inputs change over the life of a
   thread: a new turn demotes the previously-last turn, and `active_turn` flips when the run leaves
   `Running`. Because stripping is destructive, an incremental fold cannot re-derive the correct
   text once it has been applied — the marker is already gone. Before 1.4, keep the raw text in
   `ThreadState` and apply `strip_done_marker` as a presentation step in `build_transcript_items`
   (or in `projection`), where it can be recomputed per render from unmodified state. Existing
   reducer tests that assert on stripped text must move with it.
5. **Stop `reconcile` nuking the height cache.** The single biggest win, 18.5ms → 0.34ms per
   frame. Three changes to `Transcript::reconcile`: build a `HashMap<&str, usize>` of old ids once
   instead of scanning per item; key the height cache on `(item id, revision, width)` rather than
   positional index so an unchanged item keeps its measured height (the existing `revision()`
   fingerprint is already the right invalidation key); and carry each item's `RenderCache` across,
   either by having `build_transcript_items` mutate items in place or by keeping a
   `HashMap<String, RenderCache>` on `Transcript` that survives rebuilds.
6. **Bound the on-disk event stream.** `events::append_event`
   (`crates/coducktor-core/src/runs/events.rs:32-40`) opens, appends and closes the NDJSON file
   per event, under the manager lock. Hold one buffered append handle per open run and flush on a
   short interval or at turn end. Preserve append-only durability and the existing recovery
   behavior.

**Gate.** Tests 0.2, 0.3 and 0.4 pass un-ignored. Manual check on a real repository: start a task,
send a follow-up, and while the agent answers switch projects, open another thread, archive a
task, and cancel the run — all must respond immediately. Record the result in
`docs/tui/terminals.md`.

### Phase 2 — stop lying about state (F4, F5, F6)

1. **Split `Waiting`.** Add `RunStatus::Idle` for "the turn ended and the session is parked" and
   reserve `Waiting` for a real `DUCK:ASK` or an explicit `TurnMarkerDecision::Waiting`.
   `BACKWARD_COMPATIBILITY.md` promises that existing run records stay readable, not that a newer
   record stays readable by an older binary, so a new variant is within policy: legacy `waiting`
   records still parse, and are presented as `Idle` unless a pending ask exists in the event log.
   Call the change out in `CHANGELOG.md` with that degradation path. (A `needs_input: bool`
   alongside the existing status is the smaller change, but it leaves two sources of truth for one
   question and every future reader has to know which wins.) Then update in one pass: `park_session`,
   `attention()`, `QuickTask::group()`, `notification_for_transition`, `needs_you_count`, the
   composer title, the hint line, and `derive_status`. Notifications and the title counter fire
   only for genuine needs-input and review.
2. **Make `derive_status` respect the run status.** Gate the `Completed` branch on a terminal
   `RunStatus`. While `Running`, describe the actual phase — Thinking, Writing response, or the
   running tool's title — and carry elapsed time and live token count alongside the throbber
   (`tokens_used` and `cost_usd` are already on the record and already rendered in the header meta
   row, `thread/widgets.rs:78-86`).
3. **Stop silently dropping events.** Three parts. Replace the single 512-slot broadcast with a
   per-topic registry, or at minimum a much larger buffer plus separate channels for `workspace`
   and each open run, so the workspace listener no longer drains the delta firehose. Stop
   discarding `Lagged` with `.ok()` — surface it as an explicit `EngineEvent::Lagged { count }`.
   In `ThreadUi::push_events`, compare incoming `seq` against `as_of_seq`; a jump beyond the
   expected increment is a hole. On a hole or a `Lagged`, queue a thread refresh that re-reads
   durable history from the last good sequence. The durable NDJSON is the source of truth; the
   live stream is an optimization and must be allowed to fail loudly.

**Gate.** A test that floods the bus past capacity while a thread is open and asserts the
transcript still converges to the durable event log. A test that a plain turn end produces no
`needs you` and no notification while a `DUCK:ASK` turn end produces both.

### Phase 3 — make the transcript readable (F7, F8)

1. **Agent prose is a message wherever it appears.** Delete the non-final-message → dim `NoteItem`
   branch. Every assistant message becomes a `MessageItem` with full markdown. Reserve `NoteItem`
   for system lifecycle lines.
2. **Restore chronological order without regressing `1c68aa1e`.** The tail-append is not an
   oversight — it is the fix in `1c68aa1e` ("keep agent text at thread bottom"), guarded by
   `waiting_session_keeps_agent_text_after_later_tool_activity` (`thread/mod.rs:2031`). It solves a
   real problem: an agent that asks a question in prose and *then* runs more tools has that
   question stranded mid-list, where sticky-bottom never shows it. Reordering by first delta does
   not help — in that test the message's first delta precedes the tools, so it would land in the
   middle again and the test would correctly fail.

   Separate the two concerns instead. The transcript returns to reducer order (delete the
   `agent_messages` tail-append and the `final_response` hold-back). Reachability moves to the
   dock, which already hosts the structured ask card: when a run is parked in a needs-input state
   and the agent's last substantive message is not already visible, render it above the composer as
   a "latest message" panel — the prose sibling of `render_ask_card`. This is also what the
   original complaint asks for: one predictable place where text meant for the user to read and
   reply to lives, separate from tool activity.

   Rewrite `waiting_session_keeps_agent_text_after_later_tool_activity` to assert the agent text is
   *reachable* (present in the dock panel) rather than that it is the last transcript item. Do not
   delete it — it guards a real regression.

   Cheaper fallback if the dock panel proves too large for this phase: keep chronological order and
   anchor the transcript scroll to the agent's last message on entering a parked state, rather than
   to the absolute bottom. Same test rewrite applies.
3. **Give every item a gutter.** Reserve two columns on every transcript item for a role marker so
   the item type reads from the left edge before any text is parsed — `●` accent for assistant,
   `▌` in `border` for user (keep the existing left rule), `▸`/`▾` in `soft_fg` for tools and
   reasoning, `·` for notes. Add one blank row between top-level items; `height()` and `paint()`
   both already own their geometry.
4. **Colour tool cards by what they do.** Use the `ToolKind` `tool_display` already computes: give
   the verb (`Ran`/`Edit`/`Read`/`Search`) a per-kind colour from the existing palette and leave
   the argument in `fg`. Drop `BOLD` from the tool title so it stops colliding with markdown bold.
   Tool output stays `soft_fg`.
5. **Cover it with snapshots.** `insta` is already a dev-dependency. Add snapshot tests for a mixed
   transcript in both themes.

**Gate.** Open a real finished task. Without reading a word it is clear which rows are prose,
which are tools and which are system notes, and the turn reads in the order it happened.

### Phase 4 — fix the interaction model (F9, F10, F11)

1. **Open in the composer.** Focus the composer in `ThreadUi::load` and preserve focus across
   navigation instead of resetting it. Put transcript navigation behind an explicit
   `Esc`-to-transcript / `i`-to-composer pair. Remove bare `a` and `f` from the transcript keymap;
   archive and finish belong on a modifier or in the row menu, both already behind a confirm.
   Update `docs/tui/keymap.md`.
2. **`Esc` interrupts.** Bind `Esc` to `ThreadAction::Cancel` while the run is `Running` or
   `Queued`, from both focuses. First press interrupts; second press, or when idle, drops focus to
   the transcript. Show the existing `cancel_pending` "Stopping the agent…" state immediately —
   `cancel_run`'s `try_lock` path already makes this fast.
3. **Stop promising mid-turn steering; queue honestly instead.** Change the hint to "Agent is
   working · Enter queues a follow-up for the next turn" and the composer title to `FOLLOW UP`.
   Persist the text through the existing `queued_messages` plumbing
   (`edit_queued_message`/`remove_queued_message` are already on the `Engine` trait), show it in
   the transcript as a pending user message marked queued, and deliver it when the turn parks. The
   composer must never lose typed text on a rejected send.

   True mid-turn delivery — the worker handing the message to the live session at the next tool
   boundary — is deliberately **out of scope** for this plan. It is the largest single item here
   and it needs a new delivery path through `TurnDispatch`; it belongs in its own spec once
   Phase 1 has settled the worker seam.
4. **Say what is happening while it happens.** With F5 fixed, put a live one-line activity
   indicator directly above the composer: current phase, elapsed time, token count, running tool.

**Gate.** Open a task, type a reply without pressing anything first, send it, and interrupt the
agent with `Esc`. Nothing is archived, nothing is finished, no typed text is lost.

### Phase 5 — harden and shrink (F12)

1. **Survive a poisoned lock.** Either recover explicitly (`PoisonError::into_inner` with a loud
   one-time warning, matching how a corrupt state file is already handled) or move to a
   non-poisoning mutex. Separately, replace direct slice indexing on lock-held paths with checked
   access that fails the run instead of the process.
2. **Split the two files nothing can be reviewed inside.** `in_process.rs` is 13,880 lines and
   `workflows/run/mod.rs` is 6,361 — which is why one lock rule could be followed in three places
   and broken in a fourth. Split along seams the code already has: runs / git / github / ide /
   config / usage for the engine; admission / dispatch / lifecycle / persistence for the manager.
   Pure moves, no behavior change, one commit per seam.
3. **Keep the numbers honest.** Wire the Phase 0 benches into the `AGENTS.md` check list alongside
   test/clippy/fmt, with the frame-budget and lock-discipline tests as hard failures. F1 and F2
   survived this long because nothing measured them.

**Gate.** The full `SDLC.md` validation gate is clean with the new performance and
lock-discipline tests included.

## Known test and snapshot churn

These exist today and will move. None should be deleted; each guards behavior that is still
wanted, just expressed differently after the phase that touches it.

| Asset | Phase | What must happen |
| --- | --- | --- |
| `doubling_accepted_events_does_not_quadruple_rebuild_time` (`thread/mod.rs:1513`) | 0.3 | Rewrite to feed frame-sized batches; assert `rebuilt_events` too. Fails until 1.4. |
| `batched_projection_work_scales_linearly_with_accepted_events` (`thread/mod.rs:1483`) | 1.4 | Its `rebuilt_events` expectations change once folding is incremental. |
| `waiting_session_keeps_agent_text_after_later_tool_activity` (`thread/mod.rs:2031`) | 3.2 | Rewrite to assert reachability in the dock, not last-item position. See 3.2. |
| Reducer tests asserting stripped `DUCK:*` text (`thread/reducer.rs`) | 1.4 | Move with `strip_done_marker` to the presentation layer. |
| `coducktor_tui__screens__thread__tests__thread_running_{80x24,120x40,200x60}.snap` | 3 | Regenerate; review the diff rather than accepting blindly, per `AGENTS.md`. |
| `coducktor_tui__screens__thread__tests__thread_review_{80x24,120x40,200x60}.snap` | 2, 3 | Regenerate after the status split and the gutter change. |
| `transcript.rs` unit tests for tool-card default-open and note tones | 3.3, 3.4 | Heights shift by the inter-item blank row and the gutter columns. |
| `crates/coducktor-runners/tests/ui_parity.rs`, `golden.rs` | — | Must **not** change. If a phase needs them to, the change has leaked past the runner seam. |

## Out of scope

No rewrite, no new dependency, no new screen, and nothing removed from the shipped surface.
Phases 1 and 2 address the freezing and the wrong statuses; Phases 3 and 4 close the task-view
gap against the tools this is compared to.

Two adjacent items, deliberately not part of this plan: `crates/coducktor-server` is an empty
directory still listed in `Cargo.toml`'s workspace members — delete it or fill it; and
`notes.md` (26KB, git-ignored) holds design thinking beside shipped docs — fold anything still
true into `AGENTS.md` or `docs/` and drop the rest.
