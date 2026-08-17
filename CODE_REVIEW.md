# Code review rules

Review the Rust workspace as a local, single-user terminal application. The final product has no
browser client, npm package, HTTP server or service supervisor.

## Review priorities

1. Run lifecycle correctness: durable state, recovery, leases, worktrees and session teardown.
2. Graceful degradation: missing GitHub CLI, agent CLI, credentials, Git or network must produce
   an honest smaller capability, never data loss or a boot failure.
3. State compatibility: JSON unknown keys survive, corrupt entries are salvaged, NDJSON remains
   append-only, and pre-rename state is migrated without destructive cleanup.
4. Process safety: child commands use argument arrays, bounded input and an explicit environment;
   no agent output is inherited by the user's terminal.
5. Simplicity: screens depend only on `Engine`; backend and filesystem details stay below that seam.

## Checklist

- Public persisted shapes are defined in `coducktor-contract` or `coducktor-protocol` and retain
  defaults, optional fields, unknown keys and per-entry salvage where the compatibility contract
  requires them.
- Atomic writes use temporary files and rename with private permissions. Corrupt input remains on
  disk after one warning; the process continues with in-memory defaults.
- Git arguments reject option-like revisions; user-controlled paths and branch names are bounded.
- Missing external tools and offline GitHub calls return a domain error or unavailable result,
  never a panic. User-facing errors are one readable line.
- The runner seam emits the legacy flat stream and the normalized UI stream, and golden fixtures
  cover each claimed capability for every backend.
- The current writer vocabulary is used for new state, prompts, environment variables and branch
  names. The two documented compatibility regexes remain intact for old marker text and branches.
- No deleted browser, hosted, publishing, remote-skill or automation surface is reintroduced.

## Validation

```text
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Review changed `insta` snapshots and record real terminal observations in `docs/tui/terminals.md`.
