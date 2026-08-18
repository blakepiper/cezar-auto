//! Per-project quick notes under `~/.coducktor/scratchpads/`, deliberately outside Git repos.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::paths::{EnvSource, coducktor_home_dir};

fn safe_project_key(project_id: &str) -> String {
    project_id
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn scratchpad_path(env: &dyn EnvSource, project_id: &str) -> PathBuf {
    coducktor_home_dir(env)
        .join("scratchpads")
        .join(format!("{}.md", safe_project_key(project_id)))
}

pub fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

pub fn write(path: &Path, content: &str) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scratchpad path has no parent",
        ));
    };
    fs::create_dir_all(parent)?;
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    fs::rename(temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestEnv(PathBuf);

    impl EnvSource for TestEnv {
        fn get(&self, key: &str) -> Option<String> {
            (key == "DUCK_HOME").then(|| self.0.display().to_string())
        }
    }

    #[test]
    fn project_notes_are_outside_the_repository_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = scratchpad_path(&TestEnv(dir.path().to_owned()), "project/a");
        assert!(path.starts_with(dir.path().join("scratchpads")));
        write(&path, "todo\n").unwrap();
        assert_eq!(read(&path), "todo\n");
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("70726f6a6563742f61.md")
        );
    }
}
