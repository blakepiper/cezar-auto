// ---- IDE helpers --------------------------------------------------------------------------

const IDE_FILE_MAX_BYTES: usize = 1_000_000;
const IDE_DIRECTORY_MAX_ENTRIES: usize = 2_000;

fn ide_display_path(root: &Path, target: &Path) -> String {
    target
        .strip_prefix(root)
        .ok()
        .map(|relative| {
            relative
                .components()
                .filter_map(|component| match component {
                    std::path::Component::Normal(value) => Some(value.to_string_lossy()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_default()
}

fn normalize_ide_path(root: &Path, path: &str) -> Result<PathBuf, EngineError> {
    if path.chars().count() > 4_096
        || path.contains('\0')
        || path.contains('\\')
        || Path::new(path).is_absolute()
    {
        return Err(EngineError::Conflict {
            reason: "invalid project path".to_owned(),
        });
    }
    let mut target = root.to_path_buf();
    for component in Path::new(path).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(value) => target.push(value),
            std::path::Component::ParentDir => {
                if target == root || !target.pop() {
                    return Err(EngineError::Conflict {
                        reason: "path is outside the project".to_owned(),
                    });
                }
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(EngineError::Conflict {
                    reason: "invalid project path".to_owned(),
                });
            }
        }
    }
    Ok(target)
}

fn resolve_ide_path(
    root: &Path,
    path: &str,
    directory: bool,
) -> Result<(PathBuf, PathBuf), EngineError> {
    let project_root = std::fs::canonicalize(root).map_err(|_| EngineError::NotFound)?;
    let lexical = normalize_ide_path(&project_root, path)?;
    let target = std::fs::canonicalize(&lexical).map_err(|_| EngineError::NotFound)?;
    if !target.starts_with(&project_root) {
        return Err(EngineError::Conflict {
            reason: "path is outside the project".to_owned(),
        });
    }
    if target != lexical {
        return Err(EngineError::Conflict {
            reason: "symbolic links are not editable".to_owned(),
        });
    }
    let metadata = std::fs::symlink_metadata(&target).map_err(|_| EngineError::NotFound)?;
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(EngineError::NotFound);
    }
    Ok((project_root, target))
}

fn ide_list_directory(root: &Path, path: &str) -> Result<IdeDirectoryResponse, EngineError> {
    let (project_root, target) = resolve_ide_path(root, path, true)?;
    let entries = std::fs::read_dir(&target).map_err(|_| EngineError::NotFound)?;
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| EngineError::NotFound)?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let file_type = entry.file_type().map_err(|_| EngineError::NotFound)?;
        if file_type.is_dir() || file_type.is_file() {
            candidates.push((
                name.to_string_lossy().into_owned(),
                entry.path(),
                file_type.is_dir(),
            ));
        }
    }
    candidates.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.0.cmp(&right.0)));
    let truncated = candidates.len() > IDE_DIRECTORY_MAX_ENTRIES;
    let mut output = Vec::new();
    for (name, entry_path, is_directory) in candidates.into_iter().take(IDE_DIRECTORY_MAX_ENTRIES) {
        let path = ide_display_path(&project_root, &entry_path);
        if is_directory {
            output.push(IdeEntry {
                name,
                path,
                entry_type: IdeEntryType::Dir,
                size: None,
            });
        } else if let Ok(metadata) = std::fs::metadata(&entry_path)
            && metadata.is_file()
        {
            output.push(IdeEntry {
                name,
                path,
                entry_type: IdeEntryType::File,
                size: Some(metadata.len()),
            });
        }
    }
    Ok(IdeDirectoryResponse {
        path: if path.is_empty() {
            String::new()
        } else {
            ide_display_path(&project_root, &target)
        },
        entries: output,
        truncated,
    })
}

fn ide_read_file(root: &Path, path: &str) -> Result<IdeFileResponse, EngineError> {
    if path.is_empty() {
        return Err(EngineError::Conflict {
            reason: "path is required".to_owned(),
        });
    }
    let (project_root, target) = resolve_ide_path(root, path, false)?;
    let metadata = std::fs::metadata(&target).map_err(|_| EngineError::NotFound)?;
    if metadata.len() > IDE_FILE_MAX_BYTES as u64 {
        return Err(EngineError::Conflict {
            reason: "file is too large to edit".to_owned(),
        });
    }
    let bytes = std::fs::read(&target).map_err(|_| EngineError::NotFound)?;
    if bytes.contains(&0) {
        return Err(EngineError::Conflict {
            reason: "binary files cannot be edited".to_owned(),
        });
    }
    let content = String::from_utf8(bytes.clone()).map_err(|_| EngineError::Conflict {
        reason: "binary files cannot be edited".to_owned(),
    })?;
    Ok(IdeFileResponse {
        path: ide_display_path(&project_root, &target),
        content,
        size: bytes.len() as u64,
    })
}

fn ide_write_file(root: &Path, path: &str, content: &str) -> Result<IdeFileResponse, EngineError> {
    if path.is_empty() {
        return Err(EngineError::Conflict {
            reason: "path is required".to_owned(),
        });
    }
    if content.len() > IDE_FILE_MAX_BYTES {
        return Err(EngineError::Conflict {
            reason: "file is too large to edit".to_owned(),
        });
    }
    let (_, target) = resolve_ide_path(root, path, false)?;
    std::fs::write(&target, content.as_bytes()).map_err(|error| EngineError::Conflict {
        reason: error.to_string(),
    })?;
    ide_read_file(root, path)
}

