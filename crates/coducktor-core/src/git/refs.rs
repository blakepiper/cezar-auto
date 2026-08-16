//! Mirrors `packages/cezar/src/git-refs.ts`.

/// Reject option-like git revision arguments (#431). A `-`/`--`-prefixed base/from ref is
/// git argument injection: `git diff --output=/path` writes an arbitrary file,
/// `--upload-pack=<cmd>` runs a command, etc. Every ref cezar feeds to git is already
/// gated upstream by `git rev-parse --verify` / `git check-ref-format`, which reject
/// option-like values — this is the explicit last line of defense so a future refactor
/// can't silently drop that gate. Empty refs are rejected too: git would resolve them to
/// an unexpected default rather than the intended revision.
pub fn is_safe_git_ref(ref_: &str) -> bool {
    !ref_.is_empty() && !ref_.starts_with('-')
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
}
