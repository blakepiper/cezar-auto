# Software delivery process

Coducktor is maintained directly on `main`. A change is complete when the implementation,
focused tests, workspace validation gate and documentation are complete; then it is committed
and pushed to `origin main`.

## Work loop

1. Read the applicable source map in `AGENTS.md` and the relevant spec.
2. Inspect the current behavior and preserve unrelated worktree changes.
3. Implement a focused change with a regression test for the diagnosed behavior.
4. Run the focused tests, then the full Rust gate.
5. Review snapshots and compatibility docs, commit with a spec-oriented message, and push.

Numbered steps in `.ai/specs/` use one commit and one push per step. Do not mark a step complete
until its acceptance criterion and checks are green and the pushed hash is recorded.

## Validation gate

```text
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

User-facing terminal work also requires a manual result in `docs/tui/terminals.md` when a real
terminal is available. Headless snapshots and tests are still required, but they are not a
substitute for terminal-specific observations.

## Risk guidance

- High risk: run lifecycle, state-file compatibility, worktree or branch handling, and runner
  process teardown.
- Medium risk: a single screen or engine family.
- Low risk: isolated documentation or snapshot updates.

Any compatibility break must name the removed capability in `CHANGELOG.md` and
`BACKWARD_COMPATIBILITY.md`. Changes must not reintroduce a listener, browser launch, publishing
path or a required configuration file.
