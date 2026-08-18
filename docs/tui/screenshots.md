# Screenshots

coducktor is a headless-CI-built Rust TUI — there is no real terminal in this
sandbox to screenshot (see the same caveat in `terminals.md`). Rather than
fabricate images, these are honest **plain-text renders** pulled straight from
the app's own `insta` snapshot tests (`ratatui::backend::TestBackend` — the
same mechanism used for the primary UI verification tool), which
exercise the real render pipeline deterministically. Colors are not shown; the
actual TUI runs in full theme color per `docs/tui/terminals.md`.

Source: `crates/coducktor-tui/src/snapshots/coducktor_tui__app__tests__tasks_*.snap`.
Regenerate these excerpts after any UI change with `cargo insta test`.

## Tasks screen, 120×40 (sidebar visible)

```text
 [=] coducktor / main /p/main  [running 0] [needs 0]  [Ctrl+K]                                                          
  PROJECTS                   Tasks Archived 0    /                                                                      
  - main                    ┌TASKS — main──────────────────────────────────────────────────────────────────────────────┐
  > Tasks                   │No tasks in this project. Press n for New task.                                           │
    Scratchpad              │                                                                                          │
    IDE                     │                                                                                          │
    Terminal                │                                                                                          │
    Git                     │                                                                                          │
    GitHub                  │                                                                                          │
    Skills                  │                                                                                          │
    Workflows               │                                                                                          │
    Settings                │                                                                                          │
                            │                                                                                          │
  WORKSPACE                 │                                                                                          │
    All tasks               │                                                                                          │
                            │                                                                                          │
    Settings                │                                                                                          │
  NEEDS YOU                 │                                                                                          │
  WORKING                   │                                                                                          │
  RECENT                    │                                                                                          │
                            │                                                                                          │
                            │                                                                                          │
                            │                                                                                          │
                            │                                                                                          │
                            │                                                                                          │
                            │                                                                                          │
                            │                                                                                          │
                            │                                                                                          │
                            │                                                                                          │
                            │                                                                                          │
                            │                                                                                          │
                            │                                                                                          │
                            │                                                                                          │
                            │                                                                                          │
                            │                                                                                          │
                            │                                                                                          │
                            └──────────────────────────────────────────────────────────────────────────────────────────┘
 NORMAL  FOCUS: SIDEBAR — ↑↓ choose project or view · Enter open  ·  main  lazyvim  v0.1.0  [providers --]  ? help
```

## Tasks screen, 80×24 (sidebar auto-collapsed below the 100-column breakpoint)

```text
 [=] coducktor / main /p/main  [running 0] [needs 0]  [Ctrl+K]                  
 Tasks Archived 0    /                                                          
┌TASKS — main──────────────────────────────────────────────────────────────────┐
│STATUS TASK WORKFLOW br ± REF        IN/OUT     COST                          │
│No tasks yet. Describe a task to get started.                                 │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
 NORMAL  FOCUS: TASKS — ↑↓ choose task · Enter open · n new  ·  main  lazyvim  v0.1.0  [providers --]  ? help
```
