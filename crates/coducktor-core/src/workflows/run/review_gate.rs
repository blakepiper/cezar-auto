//! Pure diff-first review-gate policy.

use coducktor_contract::runs::RunStatus;

pub fn enabled(config_value: Option<bool>, env_value: Option<&str>) -> bool {
    config_value.unwrap_or_else(|| env_value == Some("1"))
}

pub fn settle_status(worktree_has_diff: bool, gate_enabled: bool, autonomous: bool) -> RunStatus {
    if worktree_has_diff && gate_enabled && !autonomous {
        RunStatus::Review
    } else {
        RunStatus::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_overrides_the_exact_env_toggle() {
        assert!(!enabled(None, Some("true")));
        assert!(enabled(None, Some("1")));
        assert!(!enabled(Some(false), Some("1")));
        assert!(enabled(Some(true), Some("0")));
    }

    #[test]
    fn autonomous_or_clean_success_skips_review() {
        assert_eq!(settle_status(true, true, false), RunStatus::Review);
        assert_eq!(settle_status(true, true, true), RunStatus::Done);
        assert_eq!(settle_status(false, true, false), RunStatus::Done);
    }
}
