# Runner capability and fixture matrix

The normalized event fixtures below are sanitized, version-independent contract samples. A cell
names the fixture that exercises the capability. `n/a` means the capability has no corresponding
event in that runner's own wire protocol at all — there is nothing to degrade from, verified by
reading the runner's mapper for an exhaustive absence of any matching event type. `unverified`
means the code path has not been proven by a fixture and its behavior on an unsupported request is
not yet confirmed safe — see the remediation plan's evidence notes for what is known.

| Capability | Codex | Claude | OpenCode | pi |
| --- | --- | --- | --- | --- |
| First turn / text | `text-turn` | `text-turn` | `text-turn` | `rpc-lifecycle` |
| Follow-up / resume | `command-lifecycle` | `task-tools-plan` | `patch-and-step-finish` | `rpc-lifecycle` |
| Built-in tools / shell | `command-lifecycle`, `todo-list` | `bash-and-screenshot`, `thinking-edit-write-todo` | `tool-lifecycle` | `rpc-lifecycle` |
| Custom or MCP tool | `file-change-and-mcp` | `task-tools-plan` | `tool-lifecycle` | `rpc-lifecycle` (no per-tool special-casing; any tool name, including an MCP one, already replays through this fixture's generic path) |
| PTY / image | `file-change-and-mcp` | `bash-and-screenshot` | unverified — non-string tool output serializes to a text field, never a typed `image` event | `tool-result-image` |
| Delegation | `sub-agent-activity`, `collab-agent-tool-call`, `collab-tool-call` | `subagent-task` | `subtask-nested`, `subtask-overlapping` | n/a — no delegation/subagent event exists in pi's RPC vocabulary |
| Plan / usage | `turn-plan-updated`, `todo-list`, `reasoning-stream`, `reasoning-snapshot-arrays` | `task-tools-plan` | `todowrite-plan` | `rpc-lifecycle` |
| Review / approval | `review-mode` | `failed-and-denied` (same `--permission-mode dontAsk` denial path as Question/permission — Claude has no separate review-mode concept) | `permission-request` (recognized, rejected through OpenCode's HTTP reply route, and reported as a failed turn because no durable interactive answer seam exists) | n/a — no review-mode event exists in pi's RPC vocabulary |
| Question / permission | explicit JSON-RPC decline or durable park | `failed-and-denied` (headless answer seam unsupported) | `permission-request` (permission is explicitly declined; no provider wait) | the question half is handled runner-neutrally via the `DUCK:ASK` marker, tested by `coducktor-core`'s own `runs::ask` unit tests rather than a golden fixture here; the permission half is n/a — no interactive tool-approval RPC exists in pi's protocol |
| Cancellation / timeout / teardown | app-server mock and `turn-failed` | `stub-ignores-eof-exits-143`, `failed-and-denied` | serve mock and `session-error` | RPC mock lifecycle |

Every `*.ndjson` fixture in this matrix is replayed by
`crates/coducktor-runners/tests/golden.rs`; the adjacent `*.expected.json` file asserts the
normalized event sequence. Fixtures contain no credentials, prompts, account names, or raw
provider captures.
