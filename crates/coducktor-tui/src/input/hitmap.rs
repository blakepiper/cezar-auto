use ratatui::layout::Rect;

use crate::widgets::table::ColumnId;

/// A new-task screen control (a pill, a button, or the composer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewTaskAction {
    SourcePill,
    RunnerPill,
    ModelPill,
    ReasoningPill,
    VariantsPill,
    BasePill,
    AccountPill,
    AutonomousPill,
    Start,
    Plan,
    Compose,
    /// A suggestion chip below the composer — inserts the template into the draft.
    Suggestion(usize),
}

/// Clickable actions registered while a frame renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitAction {
    ProjectToggle(String),
    Tasks,
    GlobalTasks,
    NewTask,
    Inbox,
    Ide,
    RepoGit,
    Github,
    Skills,
    Workflows,
    Settings,
    ActiveTasks,
    ArchivedTasks,
    ToggleSidebar,
    Help,
    SidebarEdge,
    Back,
    Forward,
    Quit,
    /// A column header on the active run table — click to sort.
    TableHeader(ColumnId),
    /// A row of the active run table — click to open, right-click to menu.
    TableRow(usize),
    /// The Tasks title row's "Mark all read" action.
    MarkAllRead,
    /// The Tasks title row's "Archive finished" action.
    ArchiveFinished,
    /// A row of the open new-task picker overlay.
    PickerRow(usize),
    /// The new-task composer's paperclip (attach row).
    ComposerAttach,
    /// Remove the attachment at this index from the composer's attachment row.
    ComposerRemoveAttachment(usize),
    /// A new-task screen control (pill/button/composer) — routed by the screen.
    NewTaskScreen(NewTaskAction),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HitRect {
    rect: Rect,
    z: u8,
    action: HitAction,
}

/// Per-frame hit-test map. Higher z-order regions win overlapping clicks.
#[derive(Debug, Clone, Default)]
pub struct HitMap {
    rects: Vec<HitRect>,
}

impl HitMap {
    pub fn clear(&mut self) {
        self.rects.clear();
    }

    pub fn register(&mut self, rect: Rect, z: u8, action: HitAction) {
        self.rects.push(HitRect { rect, z, action });
    }

    pub fn hit(&self, column: u16, row: u16) -> Option<HitAction> {
        self.rects
            .iter()
            .filter(|entry| {
                column >= entry.rect.x
                    && column < entry.rect.x.saturating_add(entry.rect.width)
                    && row >= entry.rect.y
                    && row < entry.rect.y.saturating_add(entry.rect.height)
            })
            .max_by_key(|entry| entry.z)
            .map(|entry| entry.action.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highest_z_region_wins() {
        let mut map = HitMap::default();
        map.register(Rect::new(0, 0, 10, 3), 1, HitAction::Tasks);
        map.register(Rect::new(2, 1, 4, 1), 2, HitAction::GlobalTasks);

        assert_eq!(map.hit(3, 1), Some(HitAction::GlobalTasks));
        assert_eq!(map.hit(9, 2), Some(HitAction::Tasks));
        assert_eq!(map.hit(11, 1), None);
    }
}
