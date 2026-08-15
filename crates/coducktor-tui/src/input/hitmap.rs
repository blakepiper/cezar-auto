use ratatui::layout::Rect;

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
