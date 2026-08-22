# Keymap reference

Coducktor's cockpit uses Neovim's Normal-mode grammar. Product actions do not occupy bare
printable keys; click their visible controls or use an Ex command. Literal text surfaces such as
the terminal, Scratchpad, and embedded editors keep their editing input, while `Ctrl-W` still
starts cockpit window navigation.

User overrides are read from `$DUCK_HOME/keymap.toml` (normally
`~/.coducktor/keymap.toml`) and merge over `crates/coducktor-tui/default-keymap.toml`.

## Normal mode

| Key | Meaning |
|---|---|
| `h` / `j` / `k` / `l` | Move left / down / up / right in the focused view |
| `gg` / `G` | Jump to the first / last item |
| `Ctrl+U` / `Ctrl+D` | Move half a page up / down |
| `/` | Start search |
| `n` / `N` | Next / previous search match |
| `i` | Enter Insert mode in a task composer |
| `gt` / `gT` | Next / previous tab |
| `:` | Open the Ex command line |
| `Ctrl+O` / `Ctrl+I` | Older / newer cockpit location |

`g` and `Ctrl-W` are visible prefixes in the status line. `Esc` or an invalid suffix cancels a
prefix without triggering another action.

## Windows

| Key | Meaning |
|---|---|
| `Ctrl-W h/j/k/l` | Focus the window in that direction |
| `Ctrl-W w` | Cycle to the next window |
| `Ctrl-W p` | Return to the previously focused window |

The sidebar is the leftmost window. Screen panes follow it in visual order, so an IDE route is
sidebar → tree → editor. The second key may be typed with or without `Ctrl` held. There are no
vertically stacked cockpit panes today, so `Ctrl-W j/k` is a safe no-op where no target exists.

## Insert mode

New Task and newly opened task sessions start with their composer in Insert mode. `Esc` returns
to Normal mode and `i` re-enters Insert mode. `Esc` never stops a task; use `:stop`, which keeps
the existing confirmation.

The terminal, Scratchpad, config editor, commit/name dialogs, and other literal text controls
accept their normal editing keys. `Ctrl-W` remains reserved for cockpit window navigation.

## Ex commands

| Command | Effect |
|---|---|
| `:open <route>` | Navigate to a route, for example `:open /tasks` |
| `:back` / `:forward` | Move through cockpit history |
| `:stop` | Stop the current active task after confirmation |
| `:finish` | Finish the current eligible task |
| `:archive` | Archive the current eligible task |
| `:delete` | Delete the current terminal task or removable settings row after confirmation |
| `:new` | Open New Task |
| `:theme <dark\|lazyvim\|lakes>` | Switch theme |
| `:clear-scratchpad` | Clear the current scratchpad after confirmation |
| `:sidebar` | Toggle the sidebar |
| `:help` | Open the key and command reference |
| `:q` / `:quit` | Quit, preserving the existing quit confirmation policy |

## Screen behavior

- Tasks, All Tasks, Skills, Settings, Git lists, workflow steps, and similar lists use `j`/`k`
  and arrow keys for selection. `Enter` opens or activates the selection.
- Task Session, Changes, Files, and Commits use `gt`/`gT`. Repo Git, GitHub, Workflows, and
  Compare tabs use the same grammar.
- Task transcripts use `j`/`k`, `gg`/`G`, `Ctrl-U`/`Ctrl-D`, `/`, `n`, and `N`. `Enter` toggles
  a selected expandable item. `Tab`/`Shift-Tab` move through the visible task lifecycle
  buttons, and `Enter` activates the focused button.
- On a project Tasks view, `Tab`/`Shift-Tab` move between the task cards and the New task
  button; `Enter` opens the focused control.
- The IDE tree uses `h` or `Left` to go to the parent and `Enter` or `Right` to open an entry.
- Ask cards and confirmation dialogs retain their local selection/confirmation keys while open.
- Composer and editor clipboard/editing shortcuts apply only while that text surface owns input.

All visible product controls remain mouse-operable. This includes task row menus, task lifecycle
buttons, Git and GitHub controls, workflow Save/Import/Export/Delete controls, tabs, settings
rows, confirmation dialogs, sidebar navigation, and composer buttons.
