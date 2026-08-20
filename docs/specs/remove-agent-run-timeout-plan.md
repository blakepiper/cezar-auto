# Remove agent run timeouts

## Decision

Agent turns have no wall-clock deadline. A terminal cockpit must allow a user-requested agent
to keep working while it is making progress or waiting for an external check. Users can cancel a
run explicitly; the existing EOF and shutdown safeguards still reap processes during teardown.

## Scope

- Remove the timeout field and default from the runner seam.
- Remove backend timeout escalation paths and their tests for Claude, Codex, OpenCode, and pi.
- Retain bounded timeouts used only to discover or start a local service.
