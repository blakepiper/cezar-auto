use ratatui::layout::Rect;

use crate::app::{RepoGitTab, TaskGitTab};
use crate::screens::thread::ThreadAction;
use crate::widgets::table::ColumnId;

/// A task-git (`screens/task_git`) screen control — routed by `apply_hit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskGitAction {
    SwitchTab(TaskGitTab),
    SelectTreeRow(usize),
    SelectCommit(usize),
    SelectFileEntry(usize),
    FilesUp,
    ToggleMode,
    ToggleWrap,
    OpenCommitDialog,
    CloseCommitDialog,
    SubmitCommit,
    Push,
    CreatePr,
}

/// A repo-git (`screens/repo_git`) screen control — routed by `apply_hit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoGitAction {
    SwitchTab(RepoGitTab),
    SelectTreeRow(usize),
    SelectCommit(usize),
    SelectBranch(usize),
    ToggleMode,
    ToggleWrap,
    NewBranch,
}

/// An IDE (`screens/ide`) screen control — routed by `apply_hit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdeAction {
    SelectEntry(usize),
    GoUp,
    SwitchFocus,
    Save,
    OpenInEditor,
}

/// A compare-variants (`screens/compare`) screen control — routed by `apply_hit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareAction {
    Pick(usize),
}

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
    /// A tasks-overview "Compare" strip — click to open the compare screen for that group.
    OpenCompare(String),
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
    /// A task-thread screen control — routed by the screen.
    ThreadScreen(ThreadAction),
    /// A task-git screen control — routed by the screen.
    TaskGitScreen(TaskGitAction),
    /// An IDE screen control — routed by the screen.
    IdeScreen(IdeAction),
    /// A repo-git screen control — routed by the screen.
    RepoGitScreen(RepoGitAction),
    /// A compare-variants screen control — routed by the screen.
    CompareScreen(CompareAction),
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
