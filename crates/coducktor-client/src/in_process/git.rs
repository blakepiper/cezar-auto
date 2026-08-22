// ---- open-targets helpers ------------------------------------------------------------------
// name (renamed `open_targets` -> `open_targets_list` to avoid colliding with the method above)

fn executable_on_path(binary: &str) -> bool {
    executable_in_path(binary, std::env::var_os("PATH").as_deref())
}

fn executable_in_path(binary: &str, path: Option<&std::ffi::OsStr>) -> bool {
    if binary.is_empty() {
        return false;
    }
    let path = path.unwrap_or_default();
    for directory in std::env::split_paths(path) {
        let candidate = directory.join(binary);
        let Ok(metadata) = std::fs::metadata(candidate) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        return true;
    }
    false
}

fn configured_executable(provider: &str, default: &str) -> bool {
    let env_name = format!("DUCK_{}_BIN", provider.to_ascii_uppercase());
    std::env::var(&env_name)
        .ok()
        .filter(|path| !path.trim().is_empty())
        .is_some_and(|path| Path::new(&path).is_file())
        || executable_on_path(default)
}

fn installed_mac_app(target: &str) -> Option<&'static str> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let names: &[&str] = match target {
        "vscode" => &["Visual Studio Code"],
        "cursor" => &["Cursor"],
        "zed" => &["Zed"],
        "windsurf" => &["Windsurf"],
        "sublime" => &["Sublime Text"],
        "idea" => &[
            "IntelliJ IDEA",
            "IntelliJ IDEA CE",
            "IntelliJ IDEA Ultimate",
        ],
        "pycharm" => &["PyCharm", "PyCharm CE", "PyCharm Professional"],
        "webstorm" => &["WebStorm"],
        "goland" => &["GoLand"],
        "rubymine" => &["RubyMine"],
        "phpstorm" => &["PhpStorm"],
        "clion" => &["CLion"],
        "rider" => &["Rider"],
        "android-studio" => &["Android Studio"],
        "xcode" => &["Xcode"],
        "warp" => &["Warp"],
        _ => return None,
    };
    let mut roots = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    names
        .iter()
        .find(|name| {
            roots
                .iter()
                .map(|root| root.join(format!("{name}.app")))
                .any(|path| path.is_dir())
        })
        .copied()
}

fn open_targets_list() -> Vec<coducktor_contract::OpenTarget> {
    let file_manager = if cfg!(target_os = "macos") {
        "Finder"
    } else if cfg!(target_os = "windows") {
        "Explorer"
    } else {
        "Files"
    };
    let mut targets = vec![
        coducktor_contract::OpenTarget {
            id: "finder".to_owned(),
            label: file_manager.to_owned(),
            icon: Some("folder".to_owned()),
        },
        coducktor_contract::OpenTarget {
            id: "terminal".to_owned(),
            label: "Terminal".to_owned(),
            icon: Some("terminal".to_owned()),
        },
    ];
    for (id, label, icon, binary) in [
        ("vscode", "VS Code", "vscode", "code"),
        ("cursor", "Cursor", "cursor", "cursor"),
        ("zed", "Zed", "zed", "zed"),
        ("windsurf", "Windsurf", "windsurf", "windsurf"),
        ("sublime", "Sublime Text", "sublime", "subl"),
        ("idea", "IntelliJ IDEA", "idea", "idea"),
        ("pycharm", "PyCharm", "pycharm", "pycharm"),
        ("webstorm", "WebStorm", "webstorm", "webstorm"),
        ("goland", "GoLand", "goland", "goland"),
        ("rubymine", "RubyMine", "rubymine", "rubymine"),
        ("phpstorm", "PhpStorm", "phpstorm", "phpstorm"),
        ("clion", "CLion", "clion", "clion"),
        ("rider", "Rider", "rider", "rider"),
        (
            "android-studio",
            "Android Studio",
            "android-studio",
            "studio",
        ),
        ("warp", "Warp", "warp", "warp"),
    ] {
        if executable_on_path(binary) {
            targets.push(coducktor_contract::OpenTarget {
                id: id.to_owned(),
                label: label.to_owned(),
                icon: Some(icon.to_owned()),
            });
        }
    }
    for (id, label, icon) in [
        ("vscode", "VS Code", "vscode"),
        ("cursor", "Cursor", "cursor"),
        ("zed", "Zed", "zed"),
        ("windsurf", "Windsurf", "windsurf"),
        ("sublime", "Sublime Text", "sublime"),
        ("idea", "IntelliJ IDEA", "idea"),
        ("pycharm", "PyCharm", "pycharm"),
        ("webstorm", "WebStorm", "webstorm"),
        ("goland", "GoLand", "goland"),
        ("rubymine", "RubyMine", "rubymine"),
        ("phpstorm", "PhpStorm", "phpstorm"),
        ("clion", "CLion", "clion"),
        ("rider", "Rider", "rider"),
        ("android-studio", "Android Studio", "android-studio"),
        ("xcode", "Xcode", "xcode"),
        ("warp", "Warp", "warp"),
    ] {
        if installed_mac_app(id).is_some() && !targets.iter().any(|target| target.id == id) {
            targets.push(coducktor_contract::OpenTarget {
                id: id.to_owned(),
                label: label.to_owned(),
                icon: Some(icon.to_owned()),
            });
        }
    }
    for (provider, label, icon, binary) in [
        ("claude", "Claude CLI", "claude", "claude"),
        ("codex", "Codex CLI", "codex", "codex"),
        ("opencode", "OpenCode", "opencode", "opencode"),
        ("pi", "pi CLI", "pi", "pi"),
    ] {
        if configured_executable(provider, binary) {
            targets.push(coducktor_contract::OpenTarget {
                id: format!("cli:{provider}"),
                label: label.to_owned(),
                icon: Some(icon.to_owned()),
            });
        }
    }
    targets
}

fn open_target_command(target: &str, root: &Path) -> Option<(String, Vec<String>)> {
    if target == "finder" {
        if cfg!(target_os = "macos") {
            return Some(("open".to_owned(), vec![root.to_string_lossy().into_owned()]));
        }
        if cfg!(target_os = "windows") {
            return Some((
                "explorer".to_owned(),
                vec![root.to_string_lossy().into_owned()],
            ));
        }
        return Some((
            "xdg-open".to_owned(),
            vec![root.to_string_lossy().into_owned()],
        ));
    }
    if target == "terminal" {
        if cfg!(target_os = "macos") {
            return Some((
                "open".to_owned(),
                vec![
                    "-a".to_owned(),
                    "Terminal".to_owned(),
                    root.to_string_lossy().into_owned(),
                ],
            ));
        }
        if cfg!(target_os = "windows") {
            return Some((
                "explorer".to_owned(),
                vec![root.to_string_lossy().into_owned()],
            ));
        }
        return linux_terminal_command(root);
    }
    let binary = match target {
        "vscode" => "code",
        "cursor" => "cursor",
        "zed" => "zed",
        "windsurf" => "windsurf",
        "sublime" => "subl",
        "idea" => "idea",
        "pycharm" => "pycharm",
        "webstorm" => "webstorm",
        "goland" => "goland",
        "rubymine" => "rubymine",
        "phpstorm" => "phpstorm",
        "clion" => "clion",
        "rider" => "rider",
        "android-studio" => "studio",
        "xcode" => "xcode",
        "warp" => "warp",
        _ => return None,
    };
    if !executable_on_path(binary)
        && let Some(app) = installed_mac_app(target)
    {
        return Some((
            "open".to_owned(),
            vec![
                "-a".to_owned(),
                app.to_owned(),
                root.to_string_lossy().into_owned(),
            ],
        ));
    }
    executable_on_path(binary)
        .then(|| (binary.to_owned(), vec![root.to_string_lossy().into_owned()]))
}

/// The first Linux terminal emulator present on this machine, with the argument spelling
/// that opens it at `root`. `x-terminal-emulator` comes first (Debian's alternatives slot);
/// everything after it is a direct probe so machines without it still get a terminal.
fn linux_terminal_command(root: &Path) -> Option<(String, Vec<String>)> {
    linux_terminal_command_in(root, std::env::var_os("PATH").as_deref())
}

fn linux_terminal_command_in(
    root: &Path,
    path: Option<&std::ffi::OsStr>,
) -> Option<(String, Vec<String>)> {
    let root_str = root.to_string_lossy().into_owned();
    for binary in [
        "x-terminal-emulator",
        "gnome-terminal",
        "konsole",
        "xfce4-terminal",
        "alacritty",
        "kitty",
        "foot",
        "wezterm",
        "xterm",
    ] {
        if !executable_in_path(binary, path) {
            continue;
        }
        let args = match binary {
            "x-terminal-emulator" | "xfce4-terminal" | "alacritty" | "foot" => {
                vec!["--working-directory".to_owned(), root_str.clone()]
            }
            "gnome-terminal" => vec![format!("--working-directory={root_str}")],
            "konsole" => vec!["--workdir".to_owned(), root_str.clone()],
            "kitty" => vec!["--directory".to_owned(), root_str.clone()],
            "wezterm" => vec!["start".to_owned(), "--cwd".to_owned(), root_str.clone()],
            "xterm" => vec![
                "-e".to_owned(),
                "sh".to_owned(),
                "-c".to_owned(),
                format!("cd {root_str} && exec $SHELL"),
            ],
            _ => continue,
        };
        return Some((binary.to_owned(), args));
    }
    None
}

fn open_target(root: &Path, target: &str) -> bool {
    let Some((program, args)) = open_target_command(target, root) else {
        return false;
    };
    Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

// ---- worktree helpers ----------------------------------------------------------------------

fn worktree_run_status(status: coducktor_contract::RunStatus) -> WorktreeRunStatus {
    match status {
        coducktor_contract::RunStatus::Queued => WorktreeRunStatus::Queued,
        coducktor_contract::RunStatus::Running => WorktreeRunStatus::Running,
        coducktor_contract::RunStatus::Idle => WorktreeRunStatus::Idle,
        coducktor_contract::RunStatus::Waiting => WorktreeRunStatus::Waiting,
        coducktor_contract::RunStatus::Review => WorktreeRunStatus::Review,
        coducktor_contract::RunStatus::Done => WorktreeRunStatus::Done,
        coducktor_contract::RunStatus::Failed => WorktreeRunStatus::Failed,
        coducktor_contract::RunStatus::Cancelled => WorktreeRunStatus::Cancelled,
    }
}

fn worktree_size_bytes(path: &Path) -> Option<u64> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.is_file() {
        return Some(metadata.len());
    }
    if !metadata.is_dir() {
        return Some(0);
    }
    let mut total = 0_u64;
    for entry in std::fs::read_dir(path).ok()? {
        total = total.checked_add(worktree_size_bytes(&entry.ok()?.path())?)?;
    }
    Some(total)
}

// ---- repo/run git helpers ------------------------------------------------------------------
// name (git shelling, worktree browsing, diff/compare) ------------------------------------

const NO_WORKTREE: &str = "no worktree — this task ran directly in the repo working tree";
const WORKTREE_FILE_CONTENT_CAP: u64 = 512_000;

fn git_capture(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        // A credential or host-key prompt must fail fast, never block on the TUI's raw-mode
        // terminal: `output()` inherits stdin by default, and git would otherwise sit reading
        // a tty it can never get a usable answer from — this is what turned automatic
        // (unattended) commits/pushes into an uncancellable, uninterruptible hang.
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .env_remove("SSH_ASKPASS_REQUIRE")
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if error.is_empty() {
            "git command failed".to_owned()
        } else {
            error
        })
    }
}

fn git_capture_owned(root: &Path, args: &[String]) -> Result<String, String> {
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    git_capture(root, &refs)
}

fn repo_info_at(root: &Path) -> Option<RepoInfo> {
    let repo_root = git_capture(root, &["rev-parse", "--show-toplevel"])
        .ok()?
        .trim()
        .to_owned();
    let branch = git_capture(
        Path::new(&repo_root),
        &["rev-parse", "--abbrev-ref", "HEAD"],
    )
    .ok()
    .map(|value| value.trim().to_owned())
    .filter(|value| !value.is_empty())
    .unwrap_or_else(|| "HEAD".to_owned());
    let remote = git_capture(Path::new(&repo_root), &["remote", "get-url", "origin"])
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let name = git_capture(Path::new(&repo_root), &["remote"])
                .ok()?
                .lines()
                .map(str::trim)
                .find(|value| !value.is_empty())
                .map(ToOwned::to_owned)?;
            git_capture(Path::new(&repo_root), &["remote", "get-url", name.as_str()])
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        });
    Some(RepoInfo {
        root: repo_root,
        branch,
        remote,
    })
}

fn repo_status(root: &Path) -> Vec<StatusEntry> {
    git_capture(root, &["status", "--porcelain"])
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| StatusEntry {
            status: line.get(..2).unwrap_or(line).trim().to_owned(),
            path: line.get(3..).unwrap_or_default().to_owned(),
        })
        .collect()
}

fn repo_log(root: &Path) -> Vec<LogEntry> {
    git_capture(
        root,
        &["log", "-20", "--pretty=format:%h%x1f%s%x1f%an%x1f%cr"],
    )
    .unwrap_or_default()
    .lines()
    .filter_map(|line| {
        let mut fields = line.split('\x1f');
        Some(LogEntry {
            hash: fields.next()?.to_owned(),
            subject: fields.next()?.to_owned(),
            author: fields.next()?.to_owned(),
            when: fields.next()?.to_owned(),
        })
    })
    .collect()
}

fn repo_branches(root: &Path) -> Vec<String> {
    let mut branches = std::collections::BTreeSet::new();
    for args in [
        &["branch", "--list", "--format=%(refname:short)"][..],
        &["branch", "-r", "--list", "--format=%(refname:short)"][..],
    ] {
        for name in git_capture(root, args)
            .unwrap_or_default()
            .lines()
            .map(str::trim)
        {
            if name.is_empty() || name.contains("HEAD") {
                continue;
            }
            let name = name.strip_prefix("origin/").unwrap_or(name);
            if !coducktor_core::git::refs::is_task_branch(name) {
                branches.insert(name.to_owned());
            }
        }
    }
    branches.into_iter().collect()
}

fn cap_git_text(text: String, cap: usize) -> String {
    let Some((end, _)) = text.char_indices().nth(cap) else {
        return text;
    };
    format!("{}\n… (diff truncated)", &text[..end])
}

fn diff_revision_args(revisions: &[String], suffix: &[String]) -> Vec<String> {
    let mut args = vec![
        "diff".to_owned(),
        "--no-color".to_owned(),
        "--find-renames".to_owned(),
        "--find-copies".to_owned(),
    ];
    args.extend(revisions.iter().cloned());
    args.extend(suffix.iter().cloned());
    args
}

fn changed_file_status(value: &str) -> ChangedFileStatus {
    match value.chars().next().unwrap_or('M') {
        'A' => ChangedFileStatus::Added,
        'D' => ChangedFileStatus::Deleted,
        'R' => ChangedFileStatus::Renamed,
        'C' => ChangedFileStatus::Copied,
        _ => ChangedFileStatus::Modified,
    }
}

fn collect_git_changes(root: &Path, revisions: &[String]) -> Result<ChangesPayload, String> {
    let names = git_capture_owned(
        root,
        &diff_revision_args(revisions, &["--name-status".to_owned()]),
    )?;
    let numstats = git_capture_owned(
        root,
        &diff_revision_args(revisions, &["--numstat".to_owned()]),
    )?;
    let mut counts = std::collections::HashMap::new();
    for line in numstats.lines() {
        let mut fields = line.split('\t');
        let adds = fields.next().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
        let dels = fields.next().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
        let path = fields.collect::<Vec<_>>().join("\t");
        if !path.is_empty() {
            let path = if let Some((_, new)) = path.rsplit_once(" => ") {
                new.to_owned()
            } else {
                path
            };
            counts.insert(path, (adds, dels, adds.is_nan() || dels.is_nan()));
        }
    }
    let mut files = Vec::new();
    for line in names.lines().filter(|line| !line.is_empty()) {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() < 2 {
            continue;
        }
        let status = changed_file_status(fields[0]);
        let (path, old_path) = if matches!(
            status,
            ChangedFileStatus::Renamed | ChangedFileStatus::Copied
        ) && fields.len() >= 3
        {
            (fields[2].to_owned(), Some(fields[1].to_owned()))
        } else {
            (fields[1].to_owned(), None)
        };
        let (adds, dels, binary) = counts
            .get(&path)
            .copied()
            .map_or((0.0, 0.0, false), |(adds, dels, binary)| {
                (adds, dels, binary)
            });
        let patch_args = diff_revision_args(
            revisions,
            &[
                "--patch".to_owned(),
                "--unified=20".to_owned(),
                "--".to_owned(),
                path.clone(),
            ],
        );
        let patch = git_capture_owned(root, &patch_args).unwrap_or_default();
        let binary = binary || patch.contains("Binary files");
        files.push(ChangedFile {
            path,
            old_path,
            status,
            adds,
            dels,
            binary,
            image: None,
            patch: cap_git_text(patch, 200_000),
        });
    }
    let adds = files.iter().map(|file| file.adds).sum();
    let dels = files.iter().map(|file| file.dels).sum();
    Ok(ChangesPayload {
        stat: RepoDiffStat {
            adds,
            dels,
            files: files.len() as f64,
        },
        files,
        repointed_head: None,
    })
}

fn valid_commit_hash(value: &str) -> bool {
    (4..=40).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn repo_commit_payload(root: &Path, sha: &str) -> Result<RepoCommitPayload, String> {
    if !valid_commit_hash(sha) {
        return Err("not a commit hash".to_owned());
    }
    let metadata = git_capture(
        root,
        &["show", "-s", "--format=%H%x1f%s%x1f%an%x1f%cr", sha],
    )?;
    let mut fields = metadata.trim().split('\x1f');
    let full_sha = fields.next().unwrap_or(sha).to_owned();
    let subject = fields.next().unwrap_or_default().to_owned();
    let author = fields.next().unwrap_or_default().to_owned();
    let when = fields.next().unwrap_or_default().to_owned();
    let parents = git_capture(root, &["rev-list", "--parents", "-n", "1", sha])?;
    let changes = if let Some(parent) = parents.split_whitespace().nth(1) {
        collect_git_changes(root, &[parent.to_owned(), sha.to_owned()])?
    } else {
        ChangesPayload {
            files: Vec::new(),
            stat: RepoDiffStat {
                adds: 0.0,
                dels: 0.0,
                files: 0.0,
            },
            repointed_head: None,
        }
    };
    Ok(RepoCommitPayload {
        sha: full_sha,
        subject,
        author,
        when,
        files: changes.files,
        stat: changes.stat,
    })
}

fn run_changes_payload(root: &Path, base: &str) -> Result<ChangesPayload, String> {
    if !coducktor_core::git::refs::is_safe_git_ref(base) {
        return Err("refusing option-like base ref".to_owned());
    }
    collect_git_changes(root, &[base.to_owned()])
}

fn run_worktree_of(run: &coducktor_contract::RunRecord) -> Option<PathBuf> {
    run.worktree_path
        .as_deref()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
}

fn contains_git_component(path: &str) -> bool {
    Path::new(path)
        .components()
        .any(|component| component == std::path::Component::Normal(".git".as_ref()))
}

fn read_worktree_path(root: &Path, relative: &str) -> Result<WorktreeEntry, String> {
    if relative.contains('\0') || contains_git_component(relative) {
        return Err("invalid path".to_owned());
    }
    let real_root =
        std::fs::canonicalize(root).map_err(|_| "worktree is unavailable".to_owned())?;
    let target = root.join(relative);
    let metadata = std::fs::symlink_metadata(&target).map_err(|_| {
        format!(
            "no such file or directory in the worktree: {}",
            if relative.is_empty() { "/" } else { relative }
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err("symlinks are not served".to_owned());
    }
    let real_target =
        std::fs::canonicalize(&target).map_err(|_| "worktree path is unavailable".to_owned())?;
    if real_target != real_root && !real_target.starts_with(&real_root) {
        return Err(format!("path escapes the worktree: {relative}"));
    }
    let display = real_target
        .strip_prefix(&real_root)
        .ok()
        .map(|path| {
            path.to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        })
        .unwrap_or_default();
    if metadata.is_dir() {
        let mut entries = Vec::new();
        let directory = std::fs::read_dir(&target).map_err(|error| error.to_string())?;
        for entry in directory.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue;
            }
            let child_metadata = match std::fs::symlink_metadata(entry.path()) {
                Ok(metadata) if !metadata.file_type().is_symlink() => metadata,
                _ => continue,
            };
            let entry_type = if child_metadata.is_dir() {
                WorktreeEntryType::Dir
            } else if child_metadata.is_file() {
                WorktreeEntryType::File
            } else {
                continue;
            };
            entries.push(WorktreeDirEntry {
                name,
                entry_type,
                size: child_metadata
                    .is_file()
                    .then_some(child_metadata.len() as f64),
            });
        }
        entries.sort_by(|left, right| {
            let left_dir = matches!(left.entry_type, WorktreeEntryType::Dir);
            let right_dir = matches!(right.entry_type, WorktreeEntryType::Dir);
            right_dir
                .cmp(&left_dir)
                .then_with(|| left.name.cmp(&right.name))
        });
        return Ok(WorktreeEntry::Dir {
            path: display,
            entries,
        });
    }
    if !metadata.is_file() {
        return Err(format!("not a regular file: {display}"));
    }
    let size = metadata.len();
    let too_large = size > WORKTREE_FILE_CONTENT_CAP;
    let mut sample = Vec::new();
    if let Ok(mut file) = std::fs::File::open(&target) {
        use std::io::Read as _;
        let mut buffer = [0_u8; 8_192];
        let read = file.read(&mut buffer).unwrap_or(0);
        sample.extend_from_slice(&buffer[..read]);
    }
    let binary = sample.contains(&0);
    let content = if binary || too_large {
        None
    } else {
        std::fs::read(&target)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    };
    Ok(WorktreeEntry::File {
        path: display,
        size: size as f64,
        binary,
        too_large,
        content,
    })
}

fn image_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

/// Raw bytes are only served for an image that is within the content cap; everything else is a
/// conflict.
fn read_worktree_raw(root: &Path, relative: &str) -> Result<Vec<u8>, EngineError> {
    let entry =
        read_worktree_path(root, relative).map_err(|reason| EngineError::Conflict { reason })?;
    let WorktreeEntry::File {
        path, too_large, ..
    } = &entry
    else {
        return Err(EngineError::Conflict {
            reason: format!("raw serving is limited to images: {relative}"),
        });
    };
    let mime = image_content_type(Path::new(path));
    if !mime.starts_with("image/") {
        return Err(EngineError::Conflict {
            reason: format!("raw serving is limited to images: {path}"),
        });
    }
    if *too_large {
        return Err(EngineError::Conflict {
            reason: format!("file too large to serve raw: {path}"),
        });
    }
    std::fs::read(root.join(path)).map_err(io_err)
}

fn repo_response(repo_root: &Path) -> RepoResponse {
    let Some(info) = repo_info_at(repo_root) else {
        return RepoResponse::Empty(EmptyRepoResponse {
            info: None,
            status: Vec::new(),
            log: Vec::new(),
            branches: Vec::new(),
            base_branch: None,
        });
    };
    let workspace = workspace_config_for(repo_root);
    let config = coducktor_core::config::load_config(
        &repo_config_path(Path::new(&info.root)),
        &workspace.agent_defaults,
    );
    RepoResponse::Present(PresentRepoResponse {
        info: info.clone(),
        status: repo_status(Path::new(&info.root)),
        log: repo_log(Path::new(&info.root)),
        branches: repo_branches(Path::new(&info.root)),
        base_branch: config.base_branch,
    })
}

fn create_repo_branch(
    repo_root: &Path,
    input: &RepoBranchRequest,
) -> Result<RepoBranchResponse, EngineError> {
    let Some(info) = repo_info_at(repo_root) else {
        return Err(EngineError::Conflict {
            reason: "not a git repository".to_owned(),
        });
    };
    let name = input.name.trim();
    if name.is_empty() || name.len() > 200 || !coducktor_core::git::refs::is_safe_git_ref(name) {
        return Err(EngineError::Conflict {
            reason: format!("invalid branch name: {name}"),
        });
    }
    let root = Path::new(&info.root);
    if git_capture(root, &["check-ref-format", "--branch", name]).is_err() {
        return Err(EngineError::Conflict {
            reason: format!("invalid branch name: {name}"),
        });
    }
    let exists = git_capture(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ],
    )
    .is_ok();
    let args = if exists {
        vec!["checkout".to_owned(), name.to_owned()]
    } else if let Some(from) = input
        .from
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !coducktor_core::git::refs::is_safe_git_ref(from)
            || git_capture(
                root,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{from}^{{commit}}"),
                ],
            )
            .is_err()
        {
            return Err(EngineError::Conflict {
                reason: format!("unknown start point: {from}"),
            });
        }
        vec![
            "checkout".to_owned(),
            "-b".to_owned(),
            name.to_owned(),
            from.to_owned(),
        ]
    } else {
        vec!["checkout".to_owned(), "-b".to_owned(), name.to_owned()]
    };
    if let Err(error) = git_capture_owned(root, &args) {
        return Err(EngineError::Conflict { reason: error });
    }
    Ok(RepoBranchResponse {
        branch: name.to_owned(),
        created: !exists,
    })
}

