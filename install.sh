#!/usr/bin/env bash
# Build coducktor (and its `duck` alias) from this checkout and install both onto
# PATH via `cargo install`. Source-first by design (spec "Resolved decisions"):
# no curl-pipe-to-shell hosting, no release artifacts, no auto-update check.
# `git pull && ./install.sh` again is the update mechanism.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$repo_root"

echo "==> Checking prerequisites"

if ! command -v rustup >/dev/null 2>&1; then
  cat <<'EOF' >&2
error: rustup not found.

coducktor pins its Rust toolchain via rust-toolchain.toml, which rustup reads
automatically. Install rustup first:

    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

Then re-run ./install.sh.
EOF
  exit 1
fi
echo "    rustup: $(rustup --version | head -n1)"

echo "==> Building and installing coducktor + duck (release profile)"
cargo install --path crates/coducktor-tui --locked --force

cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"

echo "==> Installed"
for name in coducktor duck; do
  if [ -x "$cargo_bin/$name" ]; then
    echo "    $cargo_bin/$name"
  fi
done

case ":$PATH:" in
  *":$cargo_bin:"*) ;;
  *)
    echo
    echo "note: $cargo_bin is not on your PATH. Add it, e.g.:"
    echo "    export PATH=\"$cargo_bin:\$PATH\""
    ;;
esac

echo
echo "Run 'duck' (or 'coducktor') to start."
