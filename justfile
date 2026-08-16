# coducktor — Rust TUI task runner. `just --list` for all recipes.
# Mirrors exactly what CI runs (.github/workflows/ci.yml's Rust job), so a green
# `just lint`/`just test` locally is a green PR.

# Build the release binaries (coducktor, duck).
build:
    cargo build --workspace --release

# Install `coducktor` and `duck` onto PATH from this checkout (same as ./install.sh
# minus the rustup/Node prerequisite checks).
install:
    cargo install --path crates/coducktor-tui --locked --force

# Run the full Rust test suite.
test:
    cargo test --workspace --all-targets

# Formatting + clippy, deny-on-warnings — what CI actually gates on.
lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings

# Review pending insta snapshot changes interactively.
snapshots:
    cargo insta test --workspace --review

# Auto-format the tree (not run by CI; use before committing).
fmt:
    cargo fmt --all
