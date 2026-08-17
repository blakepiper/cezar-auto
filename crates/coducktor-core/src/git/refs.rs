//! Mirrors the ref-safety and task-branch readers from the former git layer.

use std::sync::LazyLock;

use regex::Regex;

/// Reject option-like git revision arguments (#431). A `-`/`--`-prefixed base/from ref is
/// git argument injection: `git diff --output=/path` writes an arbitrary file,
/// `--upload-pack=<cmd>` runs a command, etc. Every ref coducktor feeds to git is already
/// gated upstream by `git rev-parse --verify` / `git check-ref-format`, which reject
/// option-like values — this is the explicit last line of defense so a future refactor
/// can't silently drop that gate. Empty refs are rejected too: git would resolve them to
/// an unexpected default rather than the intended revision.
pub fn is_safe_git_ref(ref_: &str) -> bool {
    !ref_.is_empty() && !ref_.starts_with('-')
}

// DUAL-READ SHIM (spec §2.2.2): task branches written after the rename use `duck/`, while
// existing repositories may still contain the legacy prefix. Writers never call this helper;
// readers use it so old worktrees remain discoverable.
static TASK_BRANCH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:cez|duck)/").expect("fixed task branch pattern"));

pub fn is_task_branch(branch: &str) -> bool {
    TASK_BRANCH_RE.is_match(branch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_refs() {
        for ref_ in [
            "main",
            "origin/main",
            "duck/ab12cd34",
            "HEAD",
            "feature/x",
            "a1b2c3d",
        ] {
            assert!(is_safe_git_ref(ref_), "expected {ref_:?} to be accepted");
        }
    }

    #[test]
    fn rejects_option_like_or_empty_refs() {
        for ref_ in ["-x", "--output=/tmp/evil", "--upload-pack=evil", "-", ""] {
            assert!(!is_safe_git_ref(ref_), "expected {ref_:?} to be rejected");
        }
    }

    #[test]
    fn task_branch_reader_accepts_both_generations() {
        assert!(is_task_branch("duck/ab12cd34"));
        assert!(is_task_branch(concat!("ce", "z/ab12cd34")));
        assert!(!is_task_branch("feature/ab12cd34"));
    }
}
