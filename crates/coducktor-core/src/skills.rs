//! Local skill discovery.
//!
//! A skill is a Markdown file with optional YAML-ish frontmatter (`name`, `description`).
//! Discovered from the repo's `.ai/coducktor/skills/` (this tool's own dir), `.ai/skills/`
//! (shared with other agent tooling), the `npx skills` install dirs (`.agents/skills` plus
//! the per-agent mirrors), and two global dirs under the user's home.
//!
//! Discovery is local-only. The contract's legacy `team` field remains parseable for old records,
//! but newly discovered skills never use it.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use coducktor_contract::skills::{Skill, SkillSource};

use crate::paths::EnvSource;

/// Precedence order — earlier dirs win name collisions. Agent-specific directories are scanned
/// as well as the shared project directory, then duplicate names are removed.
pub const SKILL_DIRS: &[(&str, SkillSource)] = &[
    (".ai/coducktor/skills", SkillSource::Legacy),
    (".ai/skills", SkillSource::Ai),
    (".agents/skills", SkillSource::Agents),
    (".claude/skills", SkillSource::Agents),
    (".codex/skills", SkillSource::Agents),
    (".cursor/skills", SkillSource::Agents),
    (".opencode/skills", SkillSource::Agents),
];

/// The application-provided planning mode exposed by the New Task source picker.
pub const BUILT_IN_PLANNING_SKILL_NAME: &str = "planning";
pub const BUILT_IN_PLANNING_SKILL_DESCRIPTION: &str =
    "Think through the task and return a concise plan without making changes";
pub const BUILT_IN_PLANNING_SKILL_BODY: &str = r#"You are in planning mode.

Analyze the user's task and the repository before proposing a solution. Return a concise,
ordered implementation plan that names the important files or areas, explains key decisions and
tradeoffs, and includes the verification steps that should be run.

Do not edit files, create files, delete files, commit changes, or run commands that modify the
repository. End after presenting the plan."#;

/// Deliberately the plain home dir ([`crate::paths::real_home_dir`]) and not an
/// agent-profile-aware path: a skill is CONTENT — a playbook — not identity, and a second
/// Claude login is not a second skill library.
const GLOBAL_SKILL_DIRS: &[&str] = &[".agents/skills", ".claude/skills"];

/// Discover the merged skill catalog for a repo. Name collisions resolve local-first and missing
/// directories are fine; an empty catalog is fully supported.
pub fn discover_skills(repo_root: &Path, env: &dyn EnvSource) -> Vec<Skill> {
    let home = crate::paths::real_home_dir(env);
    let mut lists: Vec<Vec<Skill>> = Vec::new();
    for (dir, source) in SKILL_DIRS {
        lists.push(read_markdown_skills(&repo_root.join(dir), *source));
    }
    for dir in GLOBAL_SKILL_DIRS {
        lists.push(read_markdown_skills(&home.join(dir), SkillSource::Global));
    }

    let mut merged: Vec<Skill> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for skills in lists {
        for skill in skills {
            if !seen.insert(skill.name.clone()) {
                continue;
            }
            merged.push(skill);
        }
    }
    if !merged
        .iter()
        .any(|skill| skill.name == BUILT_IN_PLANNING_SKILL_NAME)
    {
        merged.push(Skill {
            name: BUILT_IN_PLANNING_SKILL_NAME.to_owned(),
            description: Some(BUILT_IN_PLANNING_SKILL_DESCRIPTION.to_owned()),
            interactive: None,
            body: BUILT_IN_PLANNING_SKILL_BODY.to_owned(),
            path: "builtin:planning".to_owned(),
            source: SkillSource::BuiltIn,
            team: None,
        });
    }
    merged.sort_by(|a, b| a.name.cmp(&b.name));
    merged
}

/// Walk a skills dir for entrypoints, following directory symlinks. Once a directory
/// contains `SKILL.md`, it is one directory-based skill and its supporting Markdown (for
/// example `references/*.md`) is not scanned. Other directories retain the legacy recursive
/// `*.md` discovery behavior.
fn skill_entry_paths(dir: &Path, depth: i32, visited: &mut HashSet<PathBuf>) -> Vec<PathBuf> {
    if depth < 0 {
        return Vec::new();
    }
    let Ok(real) = fs::canonicalize(dir) else {
        return Vec::new(); // missing dir or dangling symlink
    };
    if !visited.insert(real) {
        return Vec::new();
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let entries: Vec<_> = entries.filter_map(Result::ok).collect();

    let skill_entry = entries
        .iter()
        .find(|e| e.file_name().to_str() == Some("SKILL.md"));
    if let Some(entry) = skill_entry {
        let skill_path = dir.join(entry.file_name());
        if fs::metadata(&skill_path).is_ok_and(|m| m.is_file()) {
            return vec![skill_path];
        }
        // A dangling or unreadable SKILL.md does not hide other valid entries.
    }

    let mut paths = Vec::new();
    for entry in entries {
        let path = entry.path();
        let is_dir = match entry.file_type() {
            Ok(ft) if ft.is_symlink() => match fs::metadata(&path) {
                Ok(m) => m.is_dir(), // stat follows the link
                Err(_) => continue,  // dangling symlink
            },
            Ok(ft) => ft.is_dir(),
            Err(_) => continue,
        };
        if is_dir {
            paths.extend(skill_entry_paths(&path, depth - 1, visited));
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            paths.push(path);
        }
    }
    paths
}

fn read_markdown_skills(dir: &Path, source: SkillSource) -> Vec<Skill> {
    let paths = skill_entry_paths(dir, 4, &mut HashSet::new());
    let mut skills = Vec::new();
    for abs_path in paths {
        let Ok(raw) = fs::read_to_string(&abs_path) else {
            continue;
        };
        let (frontmatter, body) = parse_frontmatter(&raw);
        let base = abs_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        // The `SKILL.md` convention names the skill after its directory.
        let fallback = if base.eq_ignore_ascii_case("skill") {
            abs_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .unwrap_or(base)
                .to_owned()
        } else {
            base.to_owned()
        };
        let name = frontmatter
            .get("name")
            .and_then(FrontmatterValue::as_scalar)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or(fallback);
        let description = frontmatter
            .get("description")
            .and_then(FrontmatterValue::as_scalar)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let interactive = frontmatter
            .get("interactive")
            .and_then(FrontmatterValue::as_scalar)
            .filter(|v| *v == "true")
            .map(|_| true);
        skills.push(Skill {
            name,
            description,
            interactive,
            body,
            path: abs_path.to_string_lossy().into_owned(),
            source,
            team: None,
        });
    }
    skills
}

enum FrontmatterValue {
    Scalar(String),
    // The parsed items are never read — no frontmatter key this module consumes
    // (`name`/`description`/`interactive`) is ever an array — but the variant itself is
    // load-bearing: it's what makes `interactive: [true]` distinct from the scalar
    // `interactive: "true"` in `as_scalar`'s match below (see the `array.md` case in
    // `discover_skills`'s tests).
    #[allow(dead_code)]
    List(Vec<String>),
}

impl FrontmatterValue {
    fn as_scalar(&self) -> Option<&str> {
        match self {
            FrontmatterValue::Scalar(s) => Some(s),
            FrontmatterValue::List(_) => None,
        }
    }
}

/// Tiny purpose-built frontmatter parser — a leading `---\n … \n---\n` block with
/// `key: value` lines, `key: [a, b]` inline arrays and `key:` + `  - a` block arrays.
/// Deliberately not full YAML so a parser dependency isn't needed for skill files.
fn parse_frontmatter(raw: &str) -> (std::collections::HashMap<String, FrontmatterValue>, String) {
    // Normalize CRLF and lone CR — otherwise frontmatter is silently dropped.
    let text = raw.replace("\r\n", "\n").replace('\r', "\n");
    if !text.starts_with("---\n") {
        return (Default::default(), raw.to_owned());
    }

    // Match the closing delimiter only on its own line so a `---` thematic break inside the
    // body doesn't terminate the block early.
    let end = find_from(&text, "\n---\n", 4);
    let end_at_eof = text.strip_suffix("\n---").map(|s| s.len());
    let close_at = end.or(end_at_eof);
    let Some(close_at) = close_at else {
        return (Default::default(), raw.to_owned());
    };

    let block = &text[4..close_at];
    let after_delimiter =
        end.and_then(|_| text[close_at + 1..].find('\n').map(|i| close_at + 1 + i));
    let body = match after_delimiter {
        Some(i) => text[i + 1..].to_owned(),
        None => String::new(),
    };

    let mut frontmatter = std::collections::HashMap::new();
    let lines: Vec<&str> = block.split('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }
        let Some((key, rest)) = split_key_value(line) else {
            i += 1;
            continue;
        };
        let rest = rest.trim();

        if rest.is_empty() {
            let mut items = Vec::new();
            while i + 1 < lines.len() && is_list_item(lines[i + 1]) {
                let item = lines[i + 1].trim_start();
                let item = item.strip_prefix('-').unwrap_or(item).trim_start();
                items.push(strip_quotes(item));
                i += 1;
            }
            frontmatter.insert(key, FrontmatterValue::List(items));
            i += 1;
            continue;
        }

        if let Some(inner) = rest.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            let inner = inner.trim();
            let items = if inner.is_empty() {
                Vec::new()
            } else {
                inner
                    .split(',')
                    .map(|s| strip_quotes(s.trim()))
                    .filter(|s| !s.is_empty())
                    .collect()
            };
            frontmatter.insert(key, FrontmatterValue::List(items));
            i += 1;
            continue;
        }

        frontmatter.insert(key, FrontmatterValue::Scalar(strip_quotes(rest)));
        i += 1;
    }

    (frontmatter, body)
}

fn find_from(text: &str, needle: &str, from: usize) -> Option<usize> {
    text.get(from..)?.find(needle).map(|i| i + from)
}

/// `/^\s*-\s+/` — a leading dash followed by at least one whitespace character.
fn is_list_item(line: &str) -> bool {
    line.trim_start()
        .strip_prefix('-')
        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

/// `^([A-Za-z0-9_-]+):\s*(.*)$`
fn split_key_value(line: &str) -> Option<(String, &str)> {
    let colon = line.find(':')?;
    let key = &line[..colon];
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some((key.to_owned(), &line[colon + 1..]))
}

fn strip_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        s[1..s.len() - 1].to_owned()
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_env::FixedEnv;
    use std::os::unix::fs::symlink;

    #[test]
    fn includes_the_built_in_planning_skill() {
        let dir = tempfile::tempdir().unwrap();
        let env = FixedEnv::new(&[("HOME", dir.path().to_str().unwrap())]);
        let skills = discover_skills(dir.path(), &env);
        let planning = skills
            .iter()
            .find(|skill| skill.name == BUILT_IN_PLANNING_SKILL_NAME)
            .unwrap();
        assert_eq!(planning.source, SkillSource::BuiltIn);
        assert_eq!(planning.body, BUILT_IN_PLANNING_SKILL_BODY);
        assert_eq!(
            planning.description.as_deref(),
            Some(BUILT_IN_PLANNING_SKILL_DESCRIPTION)
        );
    }

    #[test]
    fn recognizes_only_scalar_true_as_the_interactive_composer_hint() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".ai/coducktor/skills");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(
            skills_dir.join("true.md"),
            "---\r\ninteractive: \"true\"\r\n---\r\nBody",
        )
        .unwrap();
        fs::write(
            skills_dir.join("false.md"),
            "---\ninteractive: false\n---\nBody",
        )
        .unwrap();
        fs::write(
            skills_dir.join("array.md"),
            "---\ninteractive: [true]\n---\nBody",
        )
        .unwrap();
        fs::write(
            skills_dir.join("yes.md"),
            "---\ninteractive: yes\n---\nBody",
        )
        .unwrap();
        fs::write(skills_dir.join("missing.md"), "Body").unwrap();

        let env = FixedEnv::new(&[("HOME", dir.path().to_str().unwrap())]);
        let skills: Vec<_> = discover_skills(dir.path(), &env)
            .into_iter()
            .filter(|s| s.source == SkillSource::Legacy)
            .collect();
        let true_skill = skills.iter().find(|s| s.name == "true").unwrap();
        assert_eq!(true_skill.interactive, Some(true));
        assert_eq!(true_skill.body, "Body");
        for name in ["false", "array", "yes", "missing"] {
            let skill = skills.iter().find(|s| s.name == name).unwrap();
            assert_eq!(skill.interactive, None, "{name}");
        }
    }

    #[test]
    fn keeps_flat_and_skill_md_skills_while_excluding_nested_reference_files() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".ai/coducktor/skills");
        fs::create_dir_all(skills_dir.join("om-example/references")).unwrap();
        fs::create_dir_all(skills_dir.join("legacy/nested")).unwrap();
        fs::write(skills_dir.join("flat.md"), "# Flat skill").unwrap();
        fs::write(skills_dir.join("legacy/nested/legacy.md"), "# Legacy skill").unwrap();
        fs::write(skills_dir.join("om-example/SKILL.md"), "# Example skill").unwrap();
        fs::write(
            skills_dir.join("om-example/references/agentic-setup.md"),
            "# Supporting doc",
        )
        .unwrap();

        let env = FixedEnv::new(&[("HOME", dir.path().to_str().unwrap())]);
        let skills: Vec<_> = discover_skills(dir.path(), &env)
            .into_iter()
            .filter(|s| s.source == SkillSource::Legacy)
            .collect();
        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["flat", "legacy", "om-example"]);
        assert!(!skills.iter().any(|s| s.name == "agentic-setup"));
    }

    #[test]
    fn follows_npx_skills_directory_mirrors_and_deduplicates_them_by_skill_name() {
        let dir = tempfile::tempdir().unwrap();
        let canonical_dir = dir.path().join(".agents/skills/om-example");
        let mirror_root = dir.path().join(".claude/skills");
        fs::create_dir_all(&canonical_dir).unwrap();
        fs::create_dir_all(&mirror_root).unwrap();
        fs::write(canonical_dir.join("SKILL.md"), "# Example skill").unwrap();
        symlink(
            "../../.agents/skills/om-example",
            mirror_root.join("om-example"),
        )
        .unwrap();

        let env = FixedEnv::new(&[("HOME", dir.path().to_str().unwrap())]);
        let skills: Vec<_> = discover_skills(dir.path(), &env)
            .into_iter()
            .filter(|s| s.name == "om-example")
            .collect();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].source, SkillSource::Agents);
    }
}
