# Project-scoped tasks and agent thread UX (superseded)

> Superseded by [Task experience](task-experience.md). This document remains as historical
> context for project scoping and event normalization; its task tables and separate
> Conversation/Activity presentation are no longer the product contract.

Status: Proposed

Audience: product, TUI, client, core, runner, and protocol implementers

Last updated: 2026-08-17

## Decision summary

Coducktor will expose two explicit task scopes:

- **Project tasks** contain only runs owned by the selected project.
- **All tasks** is a read-oriented workspace index whose rows always identify their project.

Every task mutation, history request, live event, cache entry, route, and selection will use the
compound identity `(project_id, run_id)`. Selecting another project changes the project task
dataset as well as the repository context in which new tasks run.

The task thread will become a turn-based conversation. Each turn makes the user's exact prompt
and the agent's final response visually primary. Agent commentary, reasoning summaries, tool
calls, plans, questions, and approvals remain observable, but are presented as structured
activity rather than an undifferentiated transcript. A persistent composer completes the loop:
prompt, observe, review, follow up.

Scope correctness is a prerequisite for the visual redesign. A polished task screen that reads
or mutates the wrong repository is not shippable.

## Problem statement

### Scope is currently cosmetic

The TUI already has separate `Route::Tasks { project }` and `Route::GlobalTasks` routes and sends
`Scope::Project(project)` in several requests. The production engine adapter does not honor that
contract for task operations:

- `crates/coducktor-client/src/engine.rs` accepts a scope for `list_runs`, `start_run`, `get_run`,
  archive/read/delete/cancel/send/continue/finish, history, diff, files, commits, and worktree
  operations, but delegates most of them to the boot engine after discarding `_scope`.
- `crates/coducktor-client/src/in_process.rs` owns one `RunManager` rooted at the boot repository.
  `runs_index` opens other repositories only for the global index.
- `App.tasks` is one mutable vector, not a project-keyed cache. One synchronous refresh path
  applies results without checking that the project is still active.
- a `RunDeleted` event removes an item from `App.tasks` by run ID alone, even when the event belongs
  to another project.

The visible symptom is cross-project rows. The more serious consequence is that a task started
while a non-boot project is selected can use the boot project's manager and repository context.

### The task thread obscures the work

The event protocol already has useful structure: stable item IDs, message phases, tool kinds,
item lifecycle status, parent item IDs, plans, locations, diffs, usage, and turn completion. The
thread projection flattens much of that into one sequence of messages, reasoning blocks, and
generic tool cards. As a result:

- the submitted prompt is easy to lose;
- progress and raw detail compete with the answer;
- known tools lack concise, domain-specific summaries;
- completed work has no strong final-answer or review boundary;
- streaming updates can disrupt reading;
- the follow-up composer does not make send, steer, queue, stop, or resume semantics obvious.

## Goals

1. Make project task isolation correct at the engine, persistence, event, and TUI layers.
2. Make the active scope unmistakable before a user creates, opens, or mutates a task.
3. Preserve the exact user prompt and make it the opening anchor of every turn.
4. Show a legible, low-noise account of what the agent is doing while it runs.
5. Make the final response, changed files, verification, and unresolved issues easy to review.
6. Keep the composer continuously available so a follow-up naturally begins the next turn.
7. Preserve backend neutrality and the durable v1/v2 event compatibility contract.
8. Work well in a keyboard-first terminal at both wide and narrow sizes.

## Non-goals

- No browser UI, JavaScript runtime, database, service, socket, or hosted component.
- No backend-specific wire types or rendering outside the runner normalization seam.
- No requirement that providers expose private chain-of-thought. Coducktor displays normalized
  reasoning summaries or provider-visible reasoning only.
- No replacement of durable run history with ephemeral UI state.
- No deletion or rewriting of compatible v1 event readers.
- No global task mutation without an explicitly project-qualified row target.
- No redesign of the Git, GitHub, Skills, Workflows, Settings, or embedded terminal screens except
  where they link to the same project-qualified task.

## Product vocabulary

- **Project**: a registered repository with a stable project ID and canonical root.
- **Project tasks**: runs whose durable state directory belongs to one project.
- **All tasks**: the workspace-wide index of project-qualified run summaries.
- **Thread**: one task/run and its ordered sequence of turns.
- **Turn**: one user prompt, zero or more agent activity items, and one terminal outcome.
- **Conversation**: prompt, important commentary, agent response, and turn outcome.
- **Activity**: the complete normalized item stream, including tools, reasoning summaries,
  subagents, plans, retries, and diagnostics.
- **Steer**: input delivered to the currently running turn when the backend supports it.
- **Follow-up**: input that starts a subsequent turn after the current turn settles.

## Research basis

This design borrows behavior, not implementation technology, from current coding harnesses:

- [OpenCode's normalized message parts](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/message-v2.ts)
  model text, reasoning, tools, files, patches, subtasks, and step boundaries as distinct durable
  parts. Its [terminal tool renderer](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/cli/cmd/run/tool.ts)
  gives known tools semantic one-line summaries and keeps a fallback for unknown tools. Its
  [turn summary](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/cli/cmd/run/turn-summary.ts)
  makes duration and model context secondary but available.
- [DeepSeek Harness architecture](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/architecture.md)
  treats the append-only session event log as the source of truth and derives both a Chat view and
  a detailed Trajectory view from it. Its official UI snapshots demonstrate distinct user prompts,
  collapsible thinking and tools, a persistent composer, stop affordance, questions in the input
  dock, completed-message actions, and a return-to-bottom control: [running turn](https://github.com/deepseek-ai/deepseek-harness/blob/master/apps/web/tests/snapshots/turn-tail-actions/running.expected.md),
  [settled turn](https://github.com/deepseek-ai/deepseek-harness/blob/master/apps/web/tests/snapshots/turn-tail-actions/settled.expected.md),
  and [question composer](https://github.com/deepseek-ai/deepseek-harness/blob/master/apps/web/tests/snapshots/question-composer/ui.expected.md).
- [Codex app-server protocol](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)
  reinforces turn/item lifecycle events, separate reasoning summary and content, command status and
  output aggregation, explicit steer/interrupt operations, and a final agent message.

The adaptation for Coducktor is intentionally terminal-native: compact rows, predictable keyboard
focus, bounded output, no mouse dependency, and no assumptions about DOM layout.

## 1. Task scope contract

### 1.1 Scope semantics

`Scope::Project(project_id)` MUST resolve through the workspace registry to one canonical project
root. Every project-specific engine operation MUST use resources rooted at that project:

- run manager and durable state;
- repository and worktree operations;
- project config and UI state;
- workflows and skills;
- runner working directory and environment;
- history, files, diff, commits, usage, and live session control.

An unknown project ID returns `EngineError::NotFound`. An unavailable root returns a scoped,
actionable error and must not fall back to the boot root.

Workspace scope MUST be accepted only by APIs that are truly workspace-wide, such as the project
registry and the all-tasks index. Prefer a distinct `ProjectScope` type or project-qualified
methods over runtime rejection where that does not break the public compatibility surface.

### 1.2 Manager registry

Replace the single boot-project assumption with a manager registry owned by `InProcessEngine`.
The registry is keyed by stable project ID and records the canonical root used to construct each
manager. It lazily opens a project manager on first use and reuses it for subsequent operations so
active child sessions, event streams, and manager state are not split across throwaway instances.

Registry properties:

- project lookup always begins with the workspace registry;
- canonical roots are compared without treating canonicalization failure as another project;
- a registry refresh can add, update, or mark unavailable projects without crashing startup;
- manager creation remains lazy and missing/unwritable optional state degrades according to the
  existing zero-configuration rules;
- the boot project is just the initially warm registry entry, not a special routing exception.

`runs_index` uses the same registry/read seam. It must not open a second manager for a project that
already has a live one.

### 1.3 Identity and event routing

At workspace/TUI boundaries, a run is identified by:

```text
TaskKey { project_id, run_id }
```

Run IDs remain unchanged on disk. The compound key prevents collisions between repositories and
must be used by row selection, menus, pending requests, notifications, live usage, thread routes,
and deletion/update handlers.

Every workspace run, usage, and deletion event carries its project ID. A project event updates:

- that project's cache entry;
- the matching row in the all-tasks index, if loaded;
- the open thread only when both project ID and run ID match;
- quick-task notifications using the compound key.

It MUST NOT modify another project's visible task list, selection, live usage, or thread.

### 1.4 Request freshness

Each async project-list load has a monotonically increasing request generation. A response is
applied only when both its project ID and generation match the cache entry's latest request. This
handles A → B → A navigation correctly; checking only the current project is insufficient when an
old A response returns after a new A request.

The project task state should be modeled approximately as:

```rust
struct ProjectTasksState {
    runs: Vec<ApiRun>,
    loading: bool,
    error: Option<String>,
    request_generation: u64,
    selection: Option<TaskKey>,
    filter: TaskFilter,
    live_usage: BTreeMap<String, ProcessUsage>,
}

project_tasks: BTreeMap<ProjectId, ProjectTasksState>
```

The all-tasks index has separate loading, error, selection, and filter state. Project and global
filters must not overwrite one another.

### 1.5 Task list presentation

Project route:

```text
TASKS — coducktor                                      + New task
Active 4 · Needs you 1 · Finished 12          filter: active ▾
STATUS     TASK                              RUNNER      UPDATED
running    Fix task scope                    Codex       now
needs you  Confirm migration behavior        Claude      4m
```

All-tasks route:

```text
ALL TASKS                                            4 projects
Active 7 · Needs you 2 · Finished 31          filter: active ▾
PROJECT      STATUS     TASK                         UPDATED
coducktor    running    Fix task scope               now
syzygy       needs you  Review generated migration   2m
```

Rules:

- the project header names the selected project; the global header says `ALL TASKS`;
- project rows never need a redundant Project column;
- global rows always include the project and open a project-qualified thread;
- empty project copy says `No tasks in <project>` and offers `New task`;
- a project load error stays local to that project and provides `r` to retry;
- switching projects restores that project's filter, selection, and scroll when possible;
- all row actions resolve their project from `TaskKey`, never from the currently selected sidebar
  project at execution time.

### 1.6 New task scope

The New Task screen shows a non-editable context line above the composer:

```text
New task in coducktor · /home/przvl/coducktor
```

The submitted request captures the project ID at submit time. Later navigation cannot retarget an
in-flight start. On success, the app opens the returned `(project_id, run_id)` thread and renders
the exact prompt immediately. On failure, the draft remains intact and focus returns to the
composer with a scoped error.

## 2. Agent thread experience

### 2.1 Information architecture

The existing task-level tabs (`Thread`, `Changes`, `Files`, `Commits`) remain. The Thread tab has
two projections of the same durable event log:

- **Conversation** (default): prompts, meaningful progress, final responses, questions, approvals,
  failures, and compact activity groups.
- **Activity**: the complete normalized item timeline, including nested subagents and detailed
  tool lifecycle.

This is a projection choice, not duplicated state. Switching views preserves the selected turn
and the nearest stable item anchor.

Wide layout:

```text
Fix task scope                                      RUNNING  03:14
coducktor · main · Codex / gpt-5.6-luna       [Thread] Changes Files Commits
Conversation  Activity
────────────────────────────────────────────────────────────────────
YOU · 10:42
Fix project task isolation and add regression tests.

AGENT · working
I found the client adapter discarding the selected project scope.
  ▸ Read 6 files
  ✓ Edited engine.rs                         +48 −12
  ● Running cargo test -p coducktor-client

                                              4 new events · G bottom
────────────────────────────────────────────────────────────────────
Follow up, steer the running agent, or ask a question…
Ctrl+Enter send/steer                                      Esc stop
```

Narrow terminals collapse metadata and tabs before hiding content. The prompt, current status,
final response, and composer always take priority over auxiliary panels.

### 2.2 Turn anatomy

Each turn has stable sections:

1. **Prompt** — exact user text, attachments, submission time, and optional delivery state.
2. **Activity** — commentary, plan, tools, reasoning summaries, questions, approvals, retries, and
   subagent work.
3. **Response** — the final agent message, or a clear terminal error/interrupted state.
4. **Outcome** — elapsed time, model/runner, token/cost data when trustworthy, files changed, tests
   observed, and unresolved requests.

For the initial turn, the prompt comes from `run.record.task`; later prompts come from durable user
message events. Do not synthesize or paraphrase prompt text. Optimistic prompts use a temporary
client item ID and a `sending` label until reconciled with the durable event.

Completed turns keep Prompt and Response expanded. Their Activity section collapses by default to
a summary such as `12 tool calls · 3 files changed · tests passed`, while user-expanded state is
preserved across streaming reconciliation.

### 2.3 Message hierarchy

Use normalized `MessagePhase`:

- `Commentary` is muted progress prose inside Activity. Consecutive short commentary updates may
  collapse to the latest line with a count; expansion reveals all of them.
- `Final` is rendered as the response card with full markdown treatment and copy action.
- an assistant message lacking a phase uses reducer context for backwards compatibility, but new
  runner adapters must set the phase explicitly.

User messages have a consistent visual marker (`YOU`) and are never styled as agent output.
System notes and compatibility diagnostics are visually quieter and cannot masquerade as a final
answer.

### 2.4 Current status

While a turn is active, one sticky status row summarizes the latest meaningful state:

- `Thinking…`
- `Reading crates/coducktor-client/src/engine.rs`
- `Editing 2 files…`
- `Running cargo test -p coducktor-client… 18s`
- `Waiting for your answer`
- `Waiting for approval`
- `Retrying in 4s`

Status derives deterministically from the active normalized item, then plan state, then recent
commentary; it is not a second model-generated summary. The row includes elapsed time and a clear
stop action. A stalled stream retains the last status and adds `no updates for <duration>` rather
than inventing activity.

### 2.5 Tool presentation

Known `ToolKind` values use a presenter registry. Each presenter returns a compact title, subject,
status, preview, and optional expanded detail. Unknown tools use the generic presenter and never
disappear.

| Kind | Compact form | Expanded content |
| --- | --- | --- |
| Read/list/search | `✓ Read engine.rs` / `✓ Searched “list_runs” · 8 matches` | relative paths, bounded matching lines |
| Edit/write/patch | `✓ Edited engine.rs +48 −12` | patch/diff, affected paths, diagnostics |
| Execute | `● Running cargo test… 18s` / `✓ Tests passed · 42` | command argv, cwd, bounded output, exit code, duration |
| Fetch | `✓ Fetched docs.rs · 24 KB` | URL, status, bounded response metadata |
| Task/subagent | `▸ Explorer · inspecting event routing` | nested child items assembled by `parent_item_id` |
| Approval/question | `! Permission required: cargo test` | exact request, choices, consequence |
| Unknown | `<status> tool-name` | normalized input/output/error JSON |

Rules:

- running, succeeded, failed, declined, cancelled, and interrupted are distinct states;
- paths are displayed relative to the task's project root when possible;
- commands display structured argv and cwd; no shell reconstruction is required for execution;
- output is bounded in the default view with an explicit expand/tail action;
- failure text and exit status remain visible when collapsed;
- repeated adjacent read/search operations may form a group, but failures and active items remain
  independently visible;
- file-change totals come from normalized patches/diffs, not ANSI parsing;
- presenter logic is pure and snapshot-testable.

### 2.6 Reasoning

Reasoning is collapsed by default as `Think · <summary or elapsed time>`. If the normalized event
contains a provider-visible summary, use it as the collapsed label. Expanded content shows only
the reasoning text intentionally emitted by the backend. The UI must not label ordinary tool
output as reasoning or imply access to hidden chain-of-thought.

Reasoning failures, truncation, or redaction are represented as state, not blank cards.

### 2.7 Plans and subagents

The latest plan is a compact progress rail (`2/5 complete`) in Conversation and a fully ordered
item in Activity. Historical plan snapshots remain inspectable but do not each consume full
height by default.

Subagent/tool children are assembled through `parent_item_id`. Conversation shows a root summary
and important child failures; Activity exposes the recursive tree. Orphaned children caused by
partial or legacy streams appear under an `Unlinked activity` group rather than being dropped.

### 2.8 Questions, approvals, and errors

A question or approval that blocks progress is mirrored into the composer dock, where keyboard
focus and available choices are obvious. It also remains in the durable timeline. The dock shows:

- the exact question/request;
- choices and their consequences when supplied;
- free-form input when allowed;
- `Enter` submit, `Esc` keep pending, and the applicable reject/skip shortcut.

Errors are attached to the turn or item that caused them. A recoverable error offers its real
action (`Retry`, `Edit prompt`, `Choose runner`, or `Open details`). A failed turn never presents
the last commentary message as a successful final answer.

### 2.9 Streaming and scroll

The transcript reconciles by stable item ID and preserves per-item expansion state.

- When the viewport is at the bottom, new deltas auto-follow.
- Any manual upward scroll disengages follow mode.
- While disengaged, the viewport remains anchored to a stable item and row offset; upstream item
  growth must not jump the reader.
- New activity displays `<n> new events · G bottom`.
- `G` returns to bottom and re-enables follow mode.
- Streaming text is coalesced to bounded redraw frequency while terminal input remains responsive.
- Completed items must not regress to running when an older update arrives.

### 2.10 Composer and the next turn

The composer remains visible at the bottom of Thread unless a blocking question/approval occupies
the dock. It auto-grows within a bounded height and preserves unsent text while navigating task
tabs.

Its label reflects behavior:

- settled task: `Follow up…` → starts a new turn;
- running and steer supported: `Steer the running agent…` → sends to the current turn;
- running and steer unsupported: `Queue a follow-up…` → queues locally with a visible `queued`
  state, or disables send if safe queuing cannot be guaranteed;
- failed/interrupted with session: `Continue this task…`;
- closed without a resumable session: explain that a new task is required.

The footer displays the actual key bindings resolved from the keymap. Send is disabled for empty
input. Double submission is prevented by a per-message pending ID. On failure, text returns to the
composer unchanged.

### 2.11 Review and completion

When a turn settles, the view lands on the final response without hiding preceding failures. The
outcome footer includes data only when it is known:

```text
Completed in 3m 14s · 3 files +86 −21 · 42 tests passed · 18.2k tokens
[Changes] [Files] [Commits] [Copy response]
```

The final response should describe work, but the UI independently summarizes observed file and
verification events. It must distinguish:

- tests observed to pass;
- tests observed to fail;
- tests not run or not represented in events;
- a command with an unknown semantic purpose.

After review, typing in the composer begins the next turn in the same thread. The prior final
response stays visible as conversational context.

## 3. Keyboard and focus contract

The thread has explicit focus targets: top tabs, Conversation/Activity selector, transcript,
blocking dock, and composer.

- `Ctrl+Left` / `Ctrl+Right`: move between shell/sidebar and content focus according to the global
  navigation contract; inside the thread, they do not silently change turns.
- `Tab` / `Shift+Tab`: move between visible thread focus targets.
- `Up` / `Down` or `j` / `k`: move/scroll within the focused timeline or option list.
- `Enter`: expand/collapse the selected activity, activate an action, or submit a selected answer.
- `i`: focus the composer when no blocking dock owns input.
- `Ctrl+Enter`: send according to the composer mode.
- `Esc`: close a transient overlay first; stopping an agent requires an explicit second action or
  confirmation so draft text is not lost.
- `G`: bottom and resume auto-follow; `g`: top of the current turn.
- `[` / `]`: previous/next turn when transcript focus is active.

Mouse hit targets may mirror every action, but no required workflow depends on a mouse. Focus is
always shown by more than color alone.

## 4. Architecture

### 4.1 Preserve the durable source of truth

Continue to persist normalized `RunEvent` history. UI projections are rebuilt from durable events
plus the current `ApiRun`; they are not separately persisted as a second transcript. v1 events
continue through the compatibility reducer.

Extend protocol shapes only where the existing fields cannot express required semantics. Favor:

- explicit `MessagePhase` on new assistant messages;
- stable item, turn, and parent IDs;
- lifecycle status and timing on tools;
- structured file locations, diffs, command argv/cwd, and usage;
- explicit turn completion reason;
- backend capability flags for steer, queue, cancel, approval, and resume behavior.

Unknown fields remain preserved according to the existing contract rules.

### 4.2 Add a thread projection layer

Do not make Ratatui widgets interpret raw event JSON. Introduce a pure projection between
`reduce_thread` and rendering:

```text
RunEvent[] + ApiRun
        │
        ▼
ThreadState (compatible reducer)
        │
        ▼
ThreadViewModel { turns, current_status, review_summary }
        │
        ├── Conversation projection
        └── Activity projection
```

Suggested types:

```rust
struct TurnViewModel {
    id: String,
    prompt: PromptView,
    activity: Vec<ActivityNode>,
    response: Option<ResponseView>,
    outcome: TurnOutcomeView,
}

struct ActivityNode {
    id: String,
    parent_id: Option<String>,
    presentation: ActivityPresentation,
    children: Vec<ActivityNode>,
}
```

Projection and tool presenters belong under `crates/coducktor-tui/src/screens/thread/`; they use
protocol types and receive project-root context but perform no filesystem or subprocess work.

### 4.3 Rendering state

Ephemeral UI state is keyed by stable IDs:

- selected view and turn;
- expanded activity IDs;
- viewport anchor `{ item_id, row_offset }`;
- follow mode and unseen event count;
- composer draft and pending client message ID;
- focused question/approval choice.

Reconciliation retains state for surviving IDs, initializes new IDs from defaults, and prunes IDs
that no longer exist. Rebuilding a projection must not reset expansion or scroll.

### 4.4 Engine boundary

Screens continue to depend on `Engine`. Scope repair belongs in `coducktor-client` and core seams,
not direct TUI filesystem access. Runner-specific payloads still stop in `coducktor-runners`; all
thread UI consumes `coducktor-protocol` normalized events.

## 5. Compatibility, performance, and failure handling

- Existing durable runs render without migration through the v1/v2 reducer.
- New optional fields are serde-defaulted and unknown keys survive round trips.
- A corrupt project state file remains in place after one warning; another project still loads.
- An unavailable registered project remains visible in project navigation and All Tasks with an
  unavailable/error state; it does not prevent startup.
- Transcript reconciliation is linear in changed/projected items, avoids parsing ANSI or markdown
  on every paint, and uses the existing caches for wrapping/rendering.
- Tool output and raw JSON previews are bounded before layout; expansion can page/tail but cannot
  allocate an unbounded render buffer.
- A task with thousands of events remains navigable without forcing all expanded content into one
  frame.

## 6. Acceptance criteria

### Scope correctness

1. With registered projects A and B, selecting A lists only A runs; selecting B lists only B runs.
2. All Tasks lists both sets exactly once and every row names its project.
3. Starting a task from B creates durable state, runner cwd, config selection, and worktree under B,
   even when Coducktor booted in A.
4. Identical run IDs in A and B do not collide in selection, updates, deletion, usage, notifications,
   or thread routing.
5. A delayed A list response cannot overwrite B, nor a newer A response after A → B → A.
6. A live B update changes B and All Tasks but not A.
7. Archive, read/unread, cancel, continue, delete, files, diff, commits, and worktree actions all
   resolve the row's project.
8. Unknown/unavailable projects return a scoped error without falling back to the boot repository.

### Conversation and activity

1. Every turn visibly begins with the user's exact prompt.
2. Commentary and final responses have distinct styling and placement.
3. Known tools render semantic compact rows; unknown tools retain a useful fallback.
4. Running, successful, failed, declined, cancelled, and interrupted tool states are distinguishable.
5. Reasoning is collapsed by default and no hidden reasoning is implied.
6. Nested items appear under their parent in Activity; orphaned compatible events remain visible.
7. A settled turn keeps its final response expanded and activity compact.
8. Questions and approvals are operable from the input dock and remain in history.
9. Errors cannot be mistaken for a successful final response.

### Streaming, review, and iteration

1. At bottom, streaming follows; after manual scroll, the anchor is stable and unseen count grows.
2. `G` returns to bottom and re-enables follow.
3. The current status and elapsed time update without stealing composer input.
4. Stop/cancel is discoverable and does not erase draft text.
5. A sent prompt appears optimistically once, reconciles once, and is restored on delivery failure.
6. The settled outcome accurately distinguishes observed pass/fail/not-run verification.
7. A follow-up starts or steers according to displayed backend capability and becomes the next
   durable prompt without leaving the thread.
8. The core prompt → observe → review → follow-up loop works at 80×24 and 120×40 using only the
   keyboard.

## 7. Test strategy

- Client/core integration tests use two temporary Git repositories, two project registry entries,
  and intentionally colliding run IDs.
- Engine conformance tests exercise every scoped task method, not only `list_runs`.
- TUI state tests cover out-of-order generations and project-qualified event routing.
- Reducer/projection table tests cover v1 and v2 fixtures, missing phases, partial streams,
  duplicate updates, out-of-order lifecycle updates, nested children, orphan children, and errors.
- Presenter snapshot tests cover each `ToolKind`, every terminal status, long paths, Unicode, ANSI
  stripping, bounded output, and unknown tools.
- Screen snapshots cover running/settled/failed/question/approval/multi-turn states at 80×24,
  100×30, and 120×40. Review snapshot changes manually.
- Interaction tests cover focus traversal, expand/collapse, auto-follow disengagement, `G`, stop,
  send failure, steer/queue capability, and preserved drafts.
- Manual terminal checks are recorded in `docs/tui/terminals.md` only after real interactive testing.

## 8. Release boundary

The redesign is ready only when project isolation and the complete conversational loop pass their
acceptance criteria together. Partial phases may land behind internal types or tests, but avoid
shipping Conversation/Activity controls that expose stale or cross-project data. No durable data
migration should be needed; any additive state evolution must follow the existing ordered,
idempotent, non-blocking migration rules.
