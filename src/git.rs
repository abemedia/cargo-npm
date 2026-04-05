use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const BEGIN: &str = "# cargo-npm begin";
const END: &str = "# cargo-npm end";

/// Walks up the directory tree from `start` until a `.git` directory or file is found.
pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Resolves the actual git directory given a repository root (where .git exists).
/// Handles both .git directories and .git files (gitdir: ...).
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

/// Updates `.git/info/exclude` with binary paths between cargo-npm markers.
///
/// Walks up from `starting_dir` to locate the git root, so this works correctly
/// in monorepos where `.git` is above the Cargo workspace root.
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
    fn git_file_pointing_to_worktree_is_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path();

        // Simulate a worktree/submodule: .git is a file, not a directory.
        let git_dir = tmp.path().join("actual-git-dir");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(
            repo_root.join(".git"),
            format!("gitdir: {}", git_dir.display()),
        )
        .unwrap();

        let entries = vec![repo_root.join("npm/my-tool-linux-x64/my-tool")];
        update_git_exclude(repo_root, &entries).unwrap();

        let content = fs::read_to_string(git_dir.join("info/exclude")).unwrap();
        assert_eq!(
            content,
            "# cargo-npm begin\nnpm/my-tool-linux-x64/my-tool\n# cargo-npm end\n"
        );
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
