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

Any future compatibility change belongs in this section with its migration or
degradation path. Retired release notes and one-time implementation plans are
not part of the current product documentation.
