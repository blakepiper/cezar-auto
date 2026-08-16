use std::path::PathBuf;

use crate::driver::GithubDriver;
use crate::model::{ForgeKind, GithubRepoRef};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRemote {
    pub host: String,
    pub owner: String,
    pub repo: String,
}

pub fn parse_remote(remote: &str) -> Option<ParsedRemote> {
    let remote = remote.trim().trim_end_matches('/');
    let (host, path) = if let Some(rest) = remote
        .strip_prefix("https://")
        .or_else(|| remote.strip_prefix("http://"))
        .or_else(|| remote.strip_prefix("ssh://"))
        .or_else(|| remote.strip_prefix("git://"))
        .or_else(|| remote.strip_prefix("git+ssh://"))
    {
        let rest = rest.rsplit_once('@').map_or(rest, |(_, rest)| rest);
        let (host, path) = rest.split_once('/')?;
        let host = host.split(':').next()?;
        (host, path)
    } else {
        let (host, path) = remote.split_once(':')?;
        if host.contains('/') || path.starts_with('/') {
            return None;
        }
        (host.rsplit_once('@').map_or(host, |(_, host)| host), path)
    };
    let mut path_parts = path
        .trim_end_matches('/')
        .split('/')
        .filter(|piece| !piece.is_empty());
    let raw_repo = path_parts.next_back()?;
    let repo = raw_repo.strip_suffix(".git").unwrap_or(raw_repo);
    let owner = path_parts.next_back()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(ParsedRemote {
        host: host.to_ascii_lowercase(),
        owner: owner.to_owned(),
        repo: repo.to_owned(),
    })
}

pub fn forge_kind_of_remote(remote: Option<&str>) -> Option<ForgeKind> {
    let parsed = remote.and_then(parse_remote)?;
    (parsed.host == "github.com").then_some(ForgeKind::Github)
}

pub fn forge_web_root(remote: Option<&str>) -> Option<String> {
    let parsed = remote.and_then(parse_remote)?;
    (parsed.host == "github.com")
        .then(|| format!("https://github.com/{}/{}", parsed.owner, parsed.repo))
}

pub fn resolve_forge(repo_root: impl Into<PathBuf>, remote: Option<&str>) -> Option<GithubDriver> {
    let parsed = remote.and_then(parse_remote)?;
    (parsed.host == "github.com").then(|| {
        GithubDriver::new(
            repo_root,
            Some(GithubRepoRef {
                owner: parsed.owner,
                repo: parsed.repo,
            }),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_remote_forms_and_rejects_local_paths() {
        for remote in [
            "https://github.com/acme/demo.git",
            "https://user:token@github.com/acme/demo",
            "ssh://git@github.com:2222/acme/demo.git",
            "git@github.com:acme/demo.git",
        ] {
            assert_eq!(
                parse_remote(remote),
                Some(ParsedRemote {
                    host: "github.com".into(),
                    owner: "acme".into(),
                    repo: "demo".into()
                })
            );
        }
        assert!(parse_remote("/srv/git/demo.git").is_none());
        assert!(parse_remote("https://github.com/only-owner").is_none());
    }

    #[test]
    fn classifies_only_github_as_a_known_forge() {
        assert_eq!(
            forge_kind_of_remote(Some("git@github.com:acme/demo.git")),
            Some(ForgeKind::Github)
        );
        assert_eq!(
            forge_kind_of_remote(Some("git@gitlab.com:acme/demo.git")),
            None
        );
    }
}
