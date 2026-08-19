# Keymap reference

Sourced directly from `crates/coducktor-tui/src/input/keymap.rs` (the global keymap)
and each screen's `handle_key` (screen-local bindings). Regenerate this by hand
whenever those change — there is no automated doc-gen for it yet.

## Global keymap (`default-keymap.toml`)

Works from anywhere in the app. Override any binding with a user keymap at
`$DUCK_HOME/keymap.toml` (or `~/.coducktor/keymap.toml` when `DUCK_HOME` is unset) —
user bindings are merged over these defaults, key by key.

| Key | Action |
|---|---|
| `q` | Quit |
| `t` | Jump to Tasks |
| `g` | Jump to global (cross-project) Tasks |
| `c` | New task |
| `Ctrl+B` | Toggle sidebar |
| `Ctrl+Left` / `Ctrl+Right` | Move keyboard focus one section left or right — sidebar → screen (and, in the IDE, sidebar → file tree → editor). Each press steps exactly one section; `j`/`k` or `Up`/`Down` move in the panel, `Enter` opens, `Esc` returns |

| `Up` / `Down` | Move the selection in the focused panel; on Tasks, `Enter` opens the selected task |
| `?` | Help overlay (context-filtered) |
| `:` | Open the command line |
| `Ctrl+O` | Navigate back |
| `Ctrl+I` | Navigate forward |

`Ctrl+Left` / `Ctrl+Right` are handled before the keymap (they move between
focus sections, so they are not rebindable).

The cockpit starts with the sidebar focused, so `Ctrl+Right` enters the initial Tasks screen.
The status line names the focused space and shows its local movement keys. `Ctrl+Left` from a
composer releases text input and returns to the sidebar, where normal bindings such as `q` work.

The left navigation panel shows a persistent arrow selector that the keyboard and
the mouse share: clicking a sidebar row with the mouse moves it there and activates
it, and `j`/`k`/`Up`/`Down` move it once the panel has focus (`Ctrl+Left` from
the Tasks screen). `Ctrl+Left`/`Ctrl+Right` step the keyboard
focus one section at a time (sidebar → screen, with the IDE's file tree between
them), so `Ctrl+Left` from a file in the IDE lands on the file tree and `Ctrl+Right`
from the sidebar lands back on it, never skipping a section. The selector starts on
the current project row and follows every navigation, so the highlighted row always
matches the screen you are on. `Enter` opens the highlighted row (a project row
switches to that project and a nav row opens its screen). Task filtering lives in the Tasks and
All Tasks headers. `Esc` (or `Right`) returns focus to the screen. The `:open <route>` command
remains available too.

Clicking a non-active project row switches the sidebar context to that registered project and
refreshes its tasks without moving focus out of the sidebar. Clicking the active project row still
expands or collapses its navigation. Project navigation contains Tasks and Scratchpad; New task is
an action opened from Tasks rather than a separate tab.

### Command line (`:`)

| Command | Effect |
|---|---|
| `:open <route>` | Navigate to a route (e.g. `:open /tasks`) |
| `:back` | Same as `Ctrl+O` |
| `:forward` | Same as `Ctrl+I` |
| `:theme <dark\|lazyvim>` | Switch theme |
| `:new` | Same as `c` |
| `:clear-scratchpad` | Clear the current scratchpad after confirmation |
| `:help` | Same as `?` |
| `:sidebar` | Toggle sidebar |
| `:quit` | Same as `q` |

## Screen-local bindings

Everything below only applies while that screen has focus, layered on top of the
global keymap above. `j`/`k` (and usually `Down`/`Up`) move the selection in every
list-shaped screen — that convention is consistent throughout rather than
re-documented per row.

### Tasks (`screens/tasks.rs`)

| Key | Action |
|---|---|
| `j` / `k` | Move selection |
| `t` | Toggle Current/Archived view |
| `/` | Filter |
| `a` | Archive selected |
| `r` | Toggle read/unread |
| `d` | Delete (with confirm) |
| `p` | Open the task's PR |
| `Enter` | Open the task thread |

### Global tasks (`screens/global_tasks.rs`)

| Key | Action |
|---|---|
| `j` / `k` | Move selection |
| `t` | Toggle Current/Archived view |
| `/` | Filter |
| `f` | Open the project-filter picker |
| `g` | Toggle grouping by tag |
| `a` | Archive selected |
| `r` | Toggle read/unread |
| `d` | Delete (with confirm) |
| `Enter` | Open the task thread |

### New task (`screens/new_task.rs`)

| Key | Action |
|---|---|
| `Tab` / `Shift+Tab` | Move between pill fields |
| `j`/`Down`, `k`/`Up` | Move within a pill's options |
| `Space` | Toggle Autonomous (when that pill is focused) |
| `i` / `Enter` | Focus the composer |
| `n` / `s` | Start the task |
| `Ctrl+V` / `Alt+V` (composer focused) | Paste clipboard text or attach a clipboard image |

### Scratchpad (`screens/scratchpad.rs`)

The Scratchpad is a free-form editor that saves after every edit. Notes live under
`$DUCK_HOME/scratchpads/` (normally `~/.coducktor/scratchpads/`), outside the project and Git.

| Key | Action |
|---|---|
| text / editing keys | Edit the project note and save locally |
| `Shift` + arrow keys | Select text; selection can span lines |
| `Ctrl+A` | Select the entire scratchpad |
| `Backspace` / `Delete` | Delete the selection, or edit at the caret |
| `Ctrl+C` / `Ctrl+X` | Copy / cut the selection |
| `Ctrl+V` | Paste text from the clipboard |
| `Ctrl+S` | Save immediately |
| `Ctrl+K` | Clear the scratchpad after confirmation |
| `Ctrl+Left` | Return focus to the sidebar |

### Task thread (`screens/thread/mod.rs`)

| Key | Action |
|---|---|
| `i` | Focus the composer |
| `Ctrl+V` / `Alt+V` (composer focused) | Paste clipboard text or attach a clipboard image |
| `j`/`Down`, `k`/`Up` | Scroll the transcript |
| `G` | Jump to bottom (re-enables sticky-bottom) |
| `Tab` / `Shift+Tab` | Select the next/previous expandable tool or reasoning item |
| `Enter` | Toggle the selected timeline item |
| `R` | Retry/load the previous history page |
| `f` | Finish the run |
| `a` | Archive |
| `[` / `]` | Step-rail / hit-map navigation |
| `Esc` | Return focus to the transcript |
| Ask card: `j`/`k`/`Down`/`Up` | Move between options |
| Ask card: `Tab`/`Right`, `Shift+Tab`/`Left` | Move between questions |
| Ask card: `Enter` | Toggle the focused option |
| Review notes: printable keys | Type into the review note |
| Review notes: `Enter` | Newline |

### Task git tabs (`screens/task_git/mod.rs`) — Changes / Files / Commits

| Key | Action |
|---|---|
| `[` / `]` | Switch tab |
| `Tab` | Move focus between tree and diff |
| `m` | Toggle unified/split diff mode |
| `w` | Toggle whitespace |
| `c` | Open the commit dialog |
| `p` | Push |
| `j`/`Down`, `k`/`Up` | Move selection (tree, diff scroll, files list, or commits list — whichever has focus) |
| `Enter` | Open the selected entry / commit |

### Repo git (`screens/repo_git.rs`)

| Key | Action |
|---|---|
| `[` / `]` | Switch tab |
| `m` | Toggle diff mode (Changes tab) |
| `n` | New branch (Branches tab) |
| `j`/`Down`, `k`/`Up` | Move selection |
| `Enter` | Confirm the new-branch dialog |

### Compare variants (`screens/compare.rs`)

| Key | Action |
|---|---|
| `Tab`/`Right`, `Shift+Tab`/`Left` | Move between variants |
| `j`/`Down`, `k`/`Up` | Scroll the diff |
| `Enter` | Pick the selected variant |

### IDE (`screens/ide/mod.rs`)

| Key | Action |
|---|---|
| `s` | Save |
| `e` | Open in `$EDITOR` |
| `j`/`Down`, `k`/`Up` | Move the tree selection |
| `Enter` / `Right` | Open the selected entry |
| `h` / `u` / `Left` | Go up a directory |
| `Tab` | Move focus to the editor |
| `Ctrl+Left` | One section left: editor → file tree, file tree → sidebar |
| `Ctrl+Right` | One section right: sidebar → file tree, file tree → editor |
| `Esc` | Back |
| `Ctrl+S` (editor) | Save |

### GitHub (`screens/github/mod.rs`)

| Key | Action |
|---|---|
| `Tab` | Switch list tab |
| `j`/`Down`, `k`/`Up` | Move selection |
| `c` | Switch detail tab |
| `m` | Cycle merge method |
| `r` | Hand this item to an agent |
| `w` | Cycle workflow (in the hand-to-agent card) |
| `s` | Open the skill picker |
| `R` | Refresh |
| `o` | Open externally (via the `open` crate) |
| `Space` | Toggle a skill (in the skill picker) |
| `Esc` | Back / close the focused panel |

### Skills (`screens/skills.rs`)

| Key | Action |
|---|---|
| `/` | Filter |
| `j`/`Down`, `k`/`Up` | Move selection |
| `Esc` | Close filter / back |

### Settings (`screens/settings/mod.rs`)

The Workspace sidebar's **Settings** entry opens the global route (`/settings`) with Projects,
Appearance, Accounts, Notifications, and Resources. The Settings entry under a project keeps the
project-scoped sections available.

| Key | Action |
|---|---|
| `Tab` / `Shift+Tab` | Switch section |
| `j`/`Down`, `k`/`Up` | Move the focused row |
| `Left` / `Right` | Cycle an option field |
| `Enter` | Activate the focused row |
| `d` | Delete the focused row (accounts, projects, etc.) |
| `Ctrl+S` (agent-config editor) | Save the config file |
| `Esc` | Back |
