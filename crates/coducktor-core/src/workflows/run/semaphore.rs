//! Injected workspace/project capacity and repository-root lease seams.

use std::collections::VecDeque;

/// Workspace-wide coordinator. Implementations may aggregate several project managers; core only
/// asks for admission and release and never reads a config file here.
pub trait WorkspaceSemaphore {
    fn try_acquire(&mut self, run_id: &str, project_id: &str) -> bool;
    fn release(&mut self, run_id: &str, project_id: &str);
    fn busy_slots(&self) -> usize;
    fn max_parallel(&self) -> usize;
}

/// Exclusive repository-root lease. Worktree-backed jobs can inject a no-op implementation;
/// in-place jobs inject the process-wide lease owned by their integration layer.
pub trait RepositoryRootLease {
    fn try_acquire(&mut self, run_id: &str) -> bool;
    fn release(&mut self, run_id: &str);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueHead {
    pub project_id: String,
    pub run_id: String,
    pub created_at_epoch_ms: u64,
}

/// Longest-waiting project first, with project and run ids as deterministic tie breakers.
pub fn fair_queue_order(heads: &[QueueHead]) -> Vec<String> {
    let mut ordered = heads.to_vec();
    ordered.sort_by(|left, right| {
        left.created_at_epoch_ms
            .cmp(&right.created_at_epoch_ms)
            .then_with(|| left.project_id.cmp(&right.project_id))
            .then_with(|| left.run_id.cmp(&right.run_id))
    });
    ordered.into_iter().map(|head| head.run_id).collect()
}

pub fn capacity_available(
    workspace_busy: usize,
    workspace_max: usize,
    project_busy: usize,
    project_max: usize,
) -> bool {
    workspace_busy < workspace_max && project_busy < project_max
}

#[derive(Debug, Default)]
pub struct InMemoryRepositoryRootLease {
    owner: Option<String>,
    waiters: VecDeque<String>,
}

impl RepositoryRootLease for InMemoryRepositoryRootLease {
    fn try_acquire(&mut self, run_id: &str) -> bool {
        if self.owner.as_deref().is_some_and(|owner| owner == run_id) {
            return true;
        }
        if self.owner.is_some() {
            if !self.waiters.iter().any(|waiter| waiter == run_id) {
                self.waiters.push_back(run_id.to_owned());
            }
            return false;
        }
        if self.waiters.front().is_some_and(|waiter| waiter != run_id) {
            return false;
        }
        self.waiters.retain(|waiter| waiter != run_id);
        self.owner = Some(run_id.to_owned());
        true
    }

    fn release(&mut self, run_id: &str) {
        if self.owner.as_deref() == Some(run_id) {
            self.owner = None;
        }
        self.waiters.retain(|waiter| waiter != run_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fairness_prefers_the_oldest_project_queue() {
        let order = fair_queue_order(&[
            QueueHead {
                project_id: "b".to_owned(),
                run_id: "b1".to_owned(),
                created_at_epoch_ms: 20,
            },
            QueueHead {
                project_id: "a".to_owned(),
                run_id: "a1".to_owned(),
                created_at_epoch_ms: 10,
            },
        ]);
        assert_eq!(order, ["a1", "b1"]);
        assert!(capacity_available(1, 2, 0, 1));
        assert!(!capacity_available(2, 2, 0, 1));
    }

    #[test]
    fn repository_lease_is_fifo_and_releases_cleanly() {
        let mut lease = InMemoryRepositoryRootLease::default();
        assert!(lease.try_acquire("a"));
        assert!(!lease.try_acquire("b"));
        assert!(!lease.try_acquire("c"));
        lease.release("a");
        assert!(!lease.try_acquire("c"));
        assert!(lease.try_acquire("b"));
        lease.release("b");
        assert!(lease.try_acquire("c"));
    }
}
