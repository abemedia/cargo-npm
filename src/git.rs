use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const BEGIN: &str = "# cargo-npm begin";
const END: &str = "# cargo-npm end";

/// Finds the repository root by walking upward from `start` until an entry named `.git` (file or directory) is found.
///
/// Returns `Some(PathBuf)` pointing to the first ancestor directory that contains a `.git` entry, or `None` if no such ancestor exists.
pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Resolve the repository's effective git directory from a repository root that contains a `.git` entry.
///
/// If `<repo_root>/.git` is a directory, that directory is returned. If `<repo_root>/.git` is a file
/// containing a `gitdir: <path>` line, the referenced path is returned (absolute paths are returned as-is,
/// relative paths are interpreted relative to `repo_root`). Returns `None` if `.git` is missing or the file
/// cannot be parsed.
fn resolve_git_dir(repo_root: &Path) -> Option<PathBuf> {
    let dot_git = repo_root.join(".git");
    if dot_git.is_dir() {
        Some(dot_git)
    } else if dot_git.is_file() {
        let content = std::fs::read_to_string(&dot_git).ok()?;
        let prefix = "gitdir: ";
        let path = content.trim().strip_prefix(prefix)?.trim();
        let git_dir = Path::new(path);
        if git_dir.is_absolute() {
            Some(git_dir.to_path_buf())
        } else {
            Some(repo_root.join(git_dir))
        }
    } else {
        None
    }
}

/// Update the repository `.git/info/exclude` file with a marker-bounded section listing the given paths relative to the git root.
///
/// This locates the git repository root by walking upward from `starting_dir` and resolves the actual git directory (handles both `.git` directories and gitdir files). It then writes a managed section delimited by the `# cargo-npm begin` / `# cargo-npm end` markers containing each provided path converted to a git-root-relative, forward-slash-separated string. If there is no git repository found, or if `entries` is empty and no exclude file exists, the function does nothing. Filesystem read/write errors are propagated via the returned `Result`.
pub fn update_git_exclude(starting_dir: &Path, entries: &[PathBuf]) -> Result<()> {
    let Some(git_root) = find_git_root(starting_dir) else {
        return Ok(());
    };
    let Some(git_dir) = resolve_git_dir(&git_root) else {
        return Ok(());
    };

    let info_dir = git_dir.join("info");
    let exclude_path = info_dir.join("exclude");

    let relative_entries: Vec<String> = entries
        .iter()
        .filter_map(|p| p.strip_prefix(&git_root).ok())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();

    // Nothing to do if there are no entries and no existing file to update.
    if relative_entries.is_empty() && !exclude_path.exists() {
        return Ok(());
    }

    if !exclude_path.exists() {
        fs::create_dir_all(&info_dir)?;
    }

    let existing = if exclude_path.exists() {
        fs::read_to_string(&exclude_path)
            .with_context(|| format!("failed to read {}", exclude_path.display()))?
    } else {
        String::new()
    };

    let new_section = if relative_entries.is_empty() {
        String::new()
    } else {
        format!("{BEGIN}\n{}\n{END}", relative_entries.join("\n"))
    };

    let updated = if let (Some(begin_pos), Some(end_pos)) =
        (existing.find(BEGIN), existing.find(END))
        && begin_pos < end_pos
    {
        let end_of_end = end_pos + END.len();
        format!(
            "{}{}{}",
            &existing[..begin_pos],
            new_section,
            &existing[end_of_end..]
        )
    } else if !new_section.is_empty() {
        if existing.is_empty() || existing.ends_with('\n') {
            format!("{existing}{new_section}\n")
        } else {
            format!("{existing}\n{new_section}\n")
        }
    } else {
        existing
    };

    fs::write(&exclude_path, updated)
        .with_context(|| format!("failed to write {}", exclude_path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_git_dir() -> TempDir {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();
        tmp
    }

    #[test]
    fn no_git_dir_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        // No .git directory anywhere in the tree.
        update_git_exclude(tmp.path(), &[tmp.path().join("bin/foo")]).unwrap();
        assert!(!tmp.path().join(".git").exists());
    }

    #[test]
    fn writes_entries_to_new_exclude_file() {
        let tmp = make_git_dir();
        let entries = vec![tmp.path().join("npm/my-tool-linux-x64/my-tool")];
        update_git_exclude(tmp.path(), &entries).unwrap();

        let content = fs::read_to_string(tmp.path().join(".git/info/exclude")).unwrap();
        assert_eq!(
            content,
            "# cargo-npm begin\nnpm/my-tool-linux-x64/my-tool\n# cargo-npm end\n"
        );
    }

    #[test]
    fn appends_to_existing_exclude_file_without_markers() {
        let tmp = make_git_dir();
        let info_dir = tmp.path().join(".git/info");
        fs::create_dir_all(&info_dir).unwrap();
        fs::write(info_dir.join("exclude"), "*.log\n.DS_Store\n").unwrap();

        let entries = vec![tmp.path().join("npm/my-tool-linux-x64/my-tool")];
        update_git_exclude(tmp.path(), &entries).unwrap();

        let content = fs::read_to_string(info_dir.join("exclude")).unwrap();
        assert_eq!(
            content,
            "*.log\n.DS_Store\n# cargo-npm begin\nnpm/my-tool-linux-x64/my-tool\n# cargo-npm end\n"
        );
    }

    #[test]
    fn replaces_existing_cargo_npm_section() {
        let tmp = make_git_dir();
        let info_dir = tmp.path().join(".git/info");
        fs::create_dir_all(&info_dir).unwrap();
        fs::write(
            info_dir.join("exclude"),
            "*.log\n# cargo-npm begin\nold-entry\n# cargo-npm end\n.DS_Store\n",
        )
        .unwrap();

        let entries = vec![tmp.path().join("npm/my-tool-linux-x64/my-tool")];
        update_git_exclude(tmp.path(), &entries).unwrap();

        let content = fs::read_to_string(info_dir.join("exclude")).unwrap();
        assert_eq!(
            content,
            "*.log\n# cargo-npm begin\nnpm/my-tool-linux-x64/my-tool\n# cargo-npm end\n.DS_Store\n"
        );
    }
}