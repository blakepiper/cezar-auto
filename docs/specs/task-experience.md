# Task experience

Status: current product specification. This replaces the task-list and thread presentation in
`task-scope-and-agent-thread-ux.md` without changing its project-scoping or durable-event rules.

## Task browser

Project Tasks and workspace All Tasks use the same card-list language. `Current` contains every
unarchived task and `Archived` contains explicitly archived tasks. Current cards are grouped as:

- `Needs you`: waiting, review, and unseen terminal outcomes;
- `Working`: queued and running;
- `Recent`: seen done, failed, and cancelled outcomes.

Groups and cards use the most recent meaningful activity. Archive ordering uses `archivedAt`.
Legacy records fall back through finished, started, and created timestamps. Marking a task read
does not change meaningful activity. Each bounded card shows status, readable title, up to two
wrapped lines from the exact initial request, relative activity time, and only metadata that is
available. All Tasks also names the project. Search covers title, prompt, project, workflow,
branch, and reference metadata.

Cards have visible borders and a distinct selection marker. The browser keeps keyboard and mouse
open, search, archive/read/delete, and PR actions. It has no table columns, horizontal scrolling,
folding, or arbitrary sort mode. Selection and scroll are scoped independently to each project;
All Tasks owns separate state. The sidebar contains project navigation, project Tasks, All Tasks,
workspace Settings, attention counts, and notifications, but no duplicate task filters or task
snippets.

## Session timeline

A task has one virtualized chronological timeline. There is no Conversation/Activity mode.
Initial requests and follow-up prompts are exact durable user messages. Assistant commentary is
muted progress and cannot be presented as a final response. Provider-visible reasoning is
collapsed. Tool calls are semantic rows with pending, running, successful, failed, or declined
state; details can be expanded with mouse or keyboard. Plans and subagents appear compactly in
the timeline. Final assistant messages use the Markdown message renderer and outcomes explicitly
name completion, failure, interruption, or an unobserved legacy outcome.

Routine tool and reasoning details default closed after completion. Failures remain visible in
their row even while details are closed. The active turn exposes its current phase, elapsed time,
token count, and running tool directly above the composer. A persistent composer says `FOLLOW UP`
for active and follow-up-capable runs or `ANSWER` when waiting for input, with explicit queueing,
sending, and retry states. A durable question or review action places its focused controls
directly above the composer.

Scrolling up disengages live-tail following. `G` returns to the tail and clears its unseen count.
`Tab` and `BackTab` select expandable reasoning/tool items and `Enter` toggles the selected item.
Mouse clicks select and toggle the same items.

## Complete history

Opening a task loads the newest history page and retains its older cursor. Reaching the top or
pressing `R` requests the previous page. Pages merge by event sequence, deduplicate, and preserve
the visible top-item anchor. Loading and failure are explicit timeline rows; a failure offers a
retry and never implies that the visible history is complete.

`RunRecord.updatedAt` records meaningful activity such as event append or lifecycle mutation;
`archivedAt` remains separate. `RunIndexEntry` carries those optional timestamps and a locally
generated `promptPreview`. The preview collapses whitespace and is bounded to 240 Unicode scalar
values; it is not an AI summary. All fields are optional for compatibility, and existing v1/v2
event reduction remains the transcript boundary.
