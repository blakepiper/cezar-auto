# Terminal support matrix

Coducktor feature-detects and degrades — it never assumes a terminal capability.
Detection still needs a real interactive terminal to exercise, so this checklist
records what is implemented and which terminal observations remain unverified.

## Current implementation

- **Color.** `ColorCapability::detect()` (`crates/coducktor-tui/src/theme.rs`) reads `COLORTERM` for
  `truecolor`/`24bit` → 24-bit RGB; else falls back to 256-color if `TERM` contains
  `"256"`, else 16-color. Three named themes (`light`/`dark`/`lazyvim`), no `system`
  theme, no separate accent picker.
- **Images.** `ImageSupport::detect()` (`crates/coducktor-tui/src/image.rs`) calls `ratatui-image`'s
  `Picker::from_query_stdio()`, which probes the terminal (kitty graphics protocol,
  iTerm2 protocol, or sixel) over stdio at startup; falls back to a halfblock Unicode
  renderer on any color terminal, or a bordered placeholder + `o`-to-open otherwise.
  This probe **requires a real interactive TTY** — see the caveat below.
- **Mouse.** `crossterm`'s mouse capture is enabled unconditionally in
  `terminal::setup()` (`crates/coducktor-tui/src/terminal.rs`). Click/hover/drag work wherever the
  terminal reports mouse events at all; there's no separate capability gate.
- **Alternate screen + raw mode.** Always on; restored via the panic hook and on
  clean exit (`terminal::restore()`).

## Known gaps — not yet wired, not a detection failure

- **Bracketed paste.** `crossterm::event::{Enable,Disable}BracketedPaste` is not
  called anywhere in the tree yet, and there is no `Event::Paste` handler. Pasting
  multi-line text currently arrives as a burst of individual key events, not a paste
  event — this is an implementation gap, not something to "test" per terminal.
- **Kitty keyboard protocol.** `PushKeyboardEnhancementFlags` is not enabled.
  Terminals that support it (kitty, Ghostty, WezTerm) get ordinary `crossterm` key
  events, not the enhanced disambiguation (distinguishing e.g. `Ctrl+I` from `Tab`).

Both are real, worth picking up in a later pass; they're listed here so the matrix
below isn't misread as "these terminals fail bracketed paste."

## A caveat on how this checklist was produced

This document was authored from a **headless CI/agent sandbox with no attached TTY**
(`[ -t 1 ]` is false; there is no real terminal to run `duck` in interactively). Every
row below reflects either (a) what the code demonstrably does by reading the
capability-detection source above, or (b) is honestly marked **untested** rather than
guessed at. Do not trust a row marked "expected" as verified — replace it with a real
result the first time someone runs `duck` in that terminal.

| Terminal | Truecolor | Images | Mouse | Bracketed paste | Kitty keyboard protocol | Notes |
|---|---|---|---|---|---|---|
| Ghostty | expected (`COLORTERM=truecolor` typical) | expected (kitty graphics protocol) | expected | not wired (see above) | not wired (see above) | Untested — needs manual verification. |
| kitty | expected | expected (kitty graphics protocol, kitty invented it) | expected | not wired | not wired | Untested — needs manual verification. |
| WezTerm | expected | expected (kitty graphics protocol) | expected | not wired | not wired | Untested — needs manual verification. |
| iTerm2 | expected | expected (iTerm2 inline image protocol) | expected | not wired | not wired | Untested — needs manual verification. |
| Terminal.app (macOS) | no (256-color only) | no protocol — halfblock fallback | expected | not wired | not wired | Untested — needs manual verification. |
| Alacritty | expected | no protocol — halfblock fallback (no image protocol support) | expected | not wired | not wired | Untested — needs manual verification. |
| tmux | depends on `COLORTERM` passthrough config | usually breaks image protocols unless passthrough is configured | expected, may need `set -g mouse on` | not wired | not wired | Untested — needs manual verification; image support inside tmux is notoriously configuration-sensitive regardless of the inner terminal. |
| GNU screen | no (very limited color passthrough historically) | no protocol — halfblock fallback | uncertain | not wired | not wired | Untested — needs manual verification. |
| This sandbox (headless, `TERM=xterm-256color`, `COLORTERM=truecolor`, no TTY) | `ColorCapability::detect()` would report `TrueColor` from the env vars alone | `Picker::from_query_stdio()` cannot be meaningfully exercised without a real TTY to probe | N/A, no TTY | not wired | not wired | The one row in this table backed by an actual run of the detection code, not a terminal. |

## Updating this file

When you run `duck` in one of the terminals above, replace its "expected"/"untested"
cells with what actually happened, and note the terminal's version. This file is a
living checklist, not a one-time deliverable.
