# Terminal support matrix

Coducktor feature-detects and degrades — it never assumes a terminal capability.
Detection still needs a real interactive terminal to exercise, so this checklist
records what is implemented and which terminal observations remain unverified.

## Current implementation

- **Color.** `ColorCapability::detect()` (`crates/coducktor-tui/src/theme.rs`) reads `COLORTERM` for
  `truecolor`/`24bit` → 24-bit RGB; else falls back to 256-color if `TERM` contains
  `"256"`, else 16-color. Three named themes (`light`/`dark`/`lazyvim`), no `system`
  theme, no separate accent picker. The chosen theme is persisted in
  `~/.coducktor/ui-state.json` and restored on later launches.
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
- **Welcome splash.** Interactive launches reveal the embedded root `logo.txt` line by line, then
  enter the normal shell after a short hold; a key skips it and `q` still quits. Manually exercised
  in this workspace's `script` pseudo-terminal at `120x60` on 2026-08-17: logo frames, shell
  transition, splash `q` skip, and alternate-screen restoration all behaved as intended.
- **Global settings.** Manually exercised through `:open /settings` in the same `script` pseudo-terminal
  at `120x40` on 2026-08-17: the `Global settings` panel exposed `Add repository` and `Appearance`,
  and quitting restored the alternate screen.
- **Tasks keyboard focus.** Manually exercised through the real TUI in an 80x24 pseudo-terminal
  on 2026-08-17: from the focused sidebar, `Ctrl+Right` moved control into the Tasks table,
  `Down` highlighted the first task row, and `Enter` opened that task's thread. Quitting restored
  the alternate screen.
- **Focus feedback.** The status line names the keyboard-owned space (for example `SIDEBAR`,
  `TASKS`, `COMMIT LIST`, or `GIT DETAIL`) and lists its movement keys. Manual PTY smoke test on
  2026-08-18: `Ctrl+Right` changed the focus label from `SIDEBAR` to `TASKS`, `n` opened the New
  task composer, `Ctrl+Left` returned focus to `SIDEBAR`, and `q` exited cleanly. Project expansion
  and switching retain sidebar focus.
- **Embedded project terminals.** The per-project Terminal tab (`screens/terminal.rs` + `pty.rs`)
  runs a real `$SHELL` inside the cockpit — no external terminal emulator is spawned. Each
  project gets one persistent session (`portable-pty` master pair + a background reader thread
  feeding a `vt100` parser), keyed in `TerminalUi::sessions` and kept alive across navigation.
  The shell's grid renders inside the tab with per-cell colors and a reversed cursor block;
  every key on the tab goes to the shell (Esc included), scrollback is browsable with the mouse
  wheel, and bracketed paste is enabled for the cockpit lifetime. Pasting multiline clipboard
  content into the shell arrives as one chunk.
  Leaving the tab: `Ctrl+Left` to the sidebar, mouse, or a sidebar nav row; a dead shell falls
  back to degraded keys (Enter/r restarts, Esc leaves). Resize follows the tab's rect and
  forwards SIGWINCH to the shell. Sessions are killed on quit via the session `Drop`.
  The parser grid, key encoding, scrollback, and the spawn/error states are covered by unit
  tests and insta snapshots. Manually exercised through the real TUI in an 80x24 pseudo-terminal
  on 2026-08-17: opened `/p/coducktor/terminal`, verified the shell prompt and project cwd,
  ran `printf 'manual-terminal-check\n'` and saw its output in the pane, sent `Ctrl+C`, used
  `Ctrl+Left` to reach the sidebar, navigated to Git, and quit with the alternate screen restored.
  Mouse-wheel scrollback and bracketed paste remain unverified in a live terminal.
- **Composer paste.** With bracketed paste enabled for the cockpit lifetime, `Event::Paste` inserts
  clipboard text at the caret in New Task and thread composers, including newlines. Text above
  1,000 characters is shown as `[Pasted Content N chars]` and expanded exactly when submitted.
  With the composer focused, `Ctrl+V` (or `Alt+V`) reads the native clipboard directly: image data
  is PNG-encoded and shown as `[Image #N]`, while text follows the same paste path. Native clipboard
  access degrades to a scoped notice when no display/clipboard provider is available; it is covered
  by pure encoding, composer, new-task, follow-up, and retry-state tests but still needs live checks
  across the terminal matrix below.

## Task experience smoke test

- **80×24 pseudo-terminal, 2026-08-18 (parked transcript pass).** Built and launched the real
  `coducktor` binary, opened the stored waiting task from the Tasks screen, and verified the
  Session header and dock both said it was waiting for an answer while no stale `Working…` row
  remained in the transcript. The stored autonomous history was also exercised by reducer tests
  that assert every orchestration pass is placed immediately before the response it triggered.
  `q` restored the alternate screen.

- **80×24 pseudo-terminal, 2026-08-18 (live activity pass).** Built and launched the real
  `coducktor` binary with `DUCK_DRY_RUN=1`, submitted `verify throbber and finish once`, and
  observed the Session screen immediately. The activity glyph advanced through the braille
  frames while the status moved from `Queued` to `Thinking…` and tool activity streamed below.
  The deliberately marker-less dry-run backend stopped at the new `autonomous continuation
  (4/4)` ceiling and parked for input instead of repeating forty times. `q` restored the
  alternate screen. Responsive in-flight process cancellation is covered by the manager-lock
  integration test because this mock completed each individual turn too quickly for a reliable
  manual key race.

- **120×40 pseudo-terminal, 2026-08-18.** Built and launched the real `duck` binary against a
  temporary Git repository with `DUCK_DRY_RUN=1`, opened New task, and submitted `Show the prompt
  and stream agent activity`. Submission immediately replaced the composer with the run's Session
  screen: the exact prompt was visible on the first queued frame, then the header/status changed to
  `running` / `Thinking…`, and the dry-run runner's eventual process error appeared inline as a
  failed activity/outcome instead of a blank loading screen. `q` exited and restored the alternate
  screen.

- **80×24 pseudo-terminal, 2026-08-18.** Launched the real `duck` binary against the coducktor
  repository. The Tasks screen showed the `Current`/`Needs you`/`Finished` stats row and the
  bordered `TASKS — coducktor` panel title without rendering the project title twice. `q` exited
  and restored the alternate screen.

- **80×24 pseudo-terminal, 2026-08-17.** Launched the real `duck` binary against the coducktor
  repository. The project Tasks screen rendered `Current`, a bordered `Needs You` card with a
  visible `▶` selection marker, exact prompt text, relative time, runner, and workflow metadata.
  The sidebar contained project navigation plus workspace All Tasks/Settings and no task-filter
  dashboard or snippets. `Ctrl+Right` moved focus into Tasks and `Enter` opened the selected task.
  The loaded Session showed one integrated prompt/commentary/tool/outcome timeline, a collapsed
  expandable `git status` tool row, an inline running subagent row, explicit cancellation, and a
  persistent follow-up composer. `q` exited and restored the alternate screen. Pagination could
  not be exercised because this stored task fit on one history page; multi-page behavior is
  covered by reducer/state tests.

## Known gaps — not yet wired, not a detection failure

- **Kitty keyboard protocol.** `PushKeyboardEnhancementFlags` is not enabled.
  Terminals that support it (kitty, Ghostty, WezTerm) get ordinary `crossterm` key
  events, not the enhanced disambiguation (distinguishing e.g. `Ctrl+I` from `Tab`).

The remaining gap is real and worth picking up in a later pass; it is listed here so the matrix
below isn't misread as "these terminals fail the Kitty keyboard protocol."

## A caveat on how this checklist was produced

This document was authored from a **headless CI/agent sandbox with no attached TTY**
(`[ -t 1 ]` is false; there is no real terminal to run `duck` in interactively). Every
row below reflects either (a) what the code demonstrably does by reading the
capability-detection source above, or (b) is honestly marked **untested** rather than
guessed at. Do not trust a row marked "expected" as verified — replace it with a real
result the first time someone runs `duck` in that terminal.

| Terminal | Truecolor | Images | Mouse | Bracketed paste | Kitty keyboard protocol | Notes |
|---|---|---|---|---|---|---|
| Ghostty | expected (`COLORTERM=truecolor` typical) | expected (kitty graphics protocol) | expected | wired | not wired (see above) | Untested — needs manual verification. |
| kitty | expected | expected (kitty graphics protocol, kitty invented it) | expected | wired | not wired | Untested — needs manual verification. |
| WezTerm | expected | expected (kitty graphics protocol) | expected | wired | not wired | Untested — needs manual verification. |
| iTerm2 | expected | expected (iTerm2 inline image protocol) | expected | wired | not wired | Untested — needs manual verification. |
| Terminal.app (macOS) | no (256-color only) | no protocol — halfblock fallback | expected | wired | not wired | Untested — needs manual verification. |
| Alacritty | expected | no protocol — halfblock fallback (no image protocol support) | expected | wired | not wired | Untested — needs manual verification. |
| tmux | depends on `COLORTERM` passthrough config | usually breaks image protocols unless passthrough is configured | expected, may need `set -g mouse on` | wired | not wired | Untested — needs manual verification; image support inside tmux is notoriously configuration-sensitive regardless of the inner terminal. |
| GNU screen | no (very limited color passthrough historically) | no protocol — halfblock fallback | uncertain | wired | not wired | Untested — needs manual verification. |
| This sandbox (headless, `TERM=xterm-256color`, `COLORTERM=truecolor`, no TTY) | `ColorCapability::detect()` would report `TrueColor` from the env vars alone | `Picker::from_query_stdio()` cannot be meaningfully exercised without a real TTY to probe | N/A, no TTY | wired; no TTY to exercise | not wired | The one row in this table backed by an actual run of the detection code, not a terminal. |

## Updating this file

When you run `duck` in one of the terminals above, replace its "expected"/"untested"
cells with what actually happened, and note the terminal's version. This file is a
living checklist, not a one-time deliverable.
