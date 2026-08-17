# Backward compatibility

Coducktor is a local Rust terminal application distributed as one binary:
`coducktor` and its short `duck` alias. There is no browser client, npm package,
HTTP server, listening port, or hosted deployment surface to keep compatible.

## Durable state

Repository state lives under `.ai/coducktor/`; per-user workspace state lives
under `~/.coducktor/` (or the directory selected by `DUCK_HOME`). State is
plain JSON, NDJSON, Markdown, and YAML. The current reader/writer paths are
implemented in `crates/coducktor-core/src/`.

The startup migration handles the previous state-directory spelling in both
locations. If only the old directory exists, it moves it to the current name.
If both exist, the current directory wins and the old directory is reported but
never deleted. Migrations are ordered, additive, idempotent, and non-blocking.

Readers must continue to:

- preserve unknown JSON keys during read-modify-write operations;
- salvage valid entries from partially corrupt collections;
- leave corrupt files in place after one warning and boot with defaults;
- write durable files atomically with private permissions; and
- keep existing run records and append-only NDJSON event logs readable.

New writes use the `DUCK_*` environment namespace, the current `duck/` task
branch prefix, and the current `DUCK:*` marker spelling. Readers retain the
compatibility regexes for the previous marker and task-branch spellings. This
is the only intentional compatibility surface for the retired product naming.

## User-authored formats

Workflow YAML is loaded from `.ai/coducktor/workflows/`. Skills are Markdown
files with their documented frontmatter and discovery rules. The built-in
workflow and skill catalog remain available when user-authored files are
missing or malformed.

The command surface is defined by `crates/coducktor-tui/src/cli.rs` and the
headless implementation in `src/headless.rs`. The binary supports the
interactive TUI plus `run`, `init`, `usage`, `doctor`, and `projects` commands.
There are no `serve`, port, browser-open, remote-hosting, or npm-install
compatibility promises.

## Agent event protocol

Agent-specific wire formats stop at `crates/coducktor-runners`. The durable
v1 flat event kinds and normalized v2 UI events are defined by the contract and
protocol crates and remain readable for existing recordings. New backends must
map both event layers, preserve stable item ids, and degrade unsupported
capabilities without preventing startup.

The compatibility sources are:

- `AGENT_PROTOCOL.md` for runner input, lifecycle, teardown, and event rules;
- `crates/coducktor-contract/src/` for persisted and request/response shapes;
- `crates/coducktor-protocol/src/` for normalized UI events; and
- `crates/coducktor-core/src/workspace/migrations.rs` for state migration.

Any deliberate change to these surfaces must be called out in `CHANGELOG.md`
with a migration or degradation path. Historical design plans and retired
browser/service contracts are intentionally not retained in this repository.
