# Agent tools, delegation, and recoverable permissions

## Outcome

Coducktor continues to run coding-agent commands headlessly and keeps the embedded Terminal tab
under direct user control. Provider-native delegation is available under the default agent-tool
policy, and an opted-in provider approval request becomes a durable `Needs You` state that can be
answered from the task thread instead of hanging or collapsing into an opaque session failure.

## Boundaries

- Do not expose the Terminal tab's PTY to an agent or let an agent navigate the cockpit.
- Do not invent a Coducktor-owned subagent runtime. Delegation remains owned by each agent CLI.
- Preserve the existing workflow `allowedTools` override and backend-specific safety behavior.
- Keep autonomous execution as the zero-configuration default. `DUCK_APPROVAL_GATE=1` is the
  explicit opt-in for tool approvals.
- A malformed or unsupported provider approval request fails closed and leaves the process usable.

## Implementation

1. Add the provider-native `Task`/`Agent` delegation spellings to the default tool vocabulary.
   Backends that ignore per-tool allowlists remain unchanged; allowlist-driven backends can use
   the spelling their installed agent CLI exposes.
2. Teach the Codex app-server adapter to use `on-request` approval policy when
   `DUCK_APPROVAL_GATE=1`, recognize command and file-change approval RPCs, persist a normalized
   `permission.requested` event, and park the live session in `Waiting`.
3. Reuse the thread's structured choice card for normalized permission requests. The response is
   translated back to the exact pending app-server RPC and a `permission.resolved` event is
   persisted before the turn continues.
4. Preserve ordinary tool-level `failed` and `declined` states. Free-form or unrecognized approval
   responses decline rather than grant access.
5. Cover default delegation, approval-policy selection, request/response mapping, reducer state,
   and documentation. Run the full workspace test, clippy, format, and dependency-tree gates.

## Follow-up, not part of this change

A future interactive-command feature should be a separate bounded PTY tool contract with explicit
approval, input/output limits, cancellation, and teardown. It should not share the user's Terminal
tab session.
