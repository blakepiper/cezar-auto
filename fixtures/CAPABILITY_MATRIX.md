# Runner capability and fixture matrix

The normalized event fixtures below are sanitized, version-independent contract samples. A cell
names the fixture that exercises the capability; `degraded` means the runner reports its lack of
that capability as a normalized outcome rather than waiting for an unsupported RPC response.

| Capability | Codex | Claude | OpenCode | pi |
| --- | --- | --- | --- | --- |
| First turn / text | `text-turn` | `text-turn` | `text-turn` | `rpc-lifecycle` |
| Follow-up / resume | `command-lifecycle` | `task-tools-plan` | `patch-and-step-finish` | `rpc-lifecycle` |
| Built-in tools / shell | `command-lifecycle`, `todo-list` | `bash-and-screenshot`, `thinking-edit-write-todo` | `tool-lifecycle` | `rpc-lifecycle` |
| Custom or MCP tool | `file-change-and-mcp` | `task-tools-plan` | `tool-lifecycle` | degraded |
| PTY / image | `file-change-and-mcp` | `bash-and-screenshot` | degraded | degraded |
| Delegation | `sub-agent-activity`, `collab-agent-tool-call`, `collab-tool-call` | `subagent-task` | `subtask-nested`, `subtask-overlapping` | degraded |
| Plan / usage | `turn-plan-updated`, `todo-list`, `reasoning-stream`, `reasoning-snapshot-arrays` | `task-tools-plan` | `todowrite-plan` | `rpc-lifecycle` |
| Review / approval | `review-mode` | degraded | degraded | degraded |
| Question / permission | explicit JSON-RPC decline or durable park | `failed-and-denied` (headless answer seam unsupported) | `session-error` | degraded |
| Cancellation / timeout / teardown | app-server mock and `turn-failed` | `stub-ignores-eof-exits-143`, `failed-and-denied` | serve mock and `session-error` | RPC mock lifecycle |

Every `*.ndjson` fixture in this matrix is replayed by
`crates/coducktor-runners/tests/golden.rs`; the adjacent `*.expected.json` file asserts the
normalized event sequence. Fixtures contain no credentials, prompts, account names, or raw
provider captures.
