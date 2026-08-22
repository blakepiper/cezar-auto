# Unreleased

## Rust terminal release

- Coducktor is now a single Rust binary with `coducktor` and `duck` entrypoints.
- The interactive cockpit runs in the terminal through an in-process `Engine`.
- The old browser cockpit, npm distribution, HTTP server, service supervisor,
  remote hosting, bookmarklet handoff, and hosted-deployment surfaces were
  removed.
- New configuration uses the `DUCK_*` namespace. Existing state directories,
  marker text, task branches, JSON keys, run records, and NDJSON logs remain
  readable through startup migration and compatibility shims.
- Local GitHub reads and PR actions, project cloning, local skills, agent
  accounts, worktrees, workflows, and the headless task commands remain
  supported.
- Follow-up and finish calls no longer freeze unrelated cockpit operations; live thread projection
  is incremental, preserves render caches, and keeps reads isolated from mutation workers.
- Parked sessions now use `idle`; `waiting` is reserved for real user input, so ordinary turn
  endings no longer raise needs-you notifications. Legacy `waiting` records remain readable and
  load as `idle` unless their durable event log contains an unanswered structured ask.
- Live events use independent per-topic channels. Receiver lag and sequence holes are visible in
  the debug HUD and trigger a durable thread/workspace refresh instead of silently losing output.

Any future compatibility change belongs in this section with its migration or
degradation path. Retired release notes and one-time implementation plans are
not part of the current product documentation.
