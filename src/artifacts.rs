use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::platform::{HOST_PLATFORM, Os, Platform, parse_triple};

/// Discover target triples under `target_dir` that contain at least one of the specified binaries.
///
/// Scans `target/{triple}/release/` subdirectories and falls back to `target/release/` for the host
/// platform. On success, returns a set of recognized target triples that contain at least one of
/// the provided binary names. If no matching binaries are found in any candidate release directory,
/// an error is returned advising to build the artifacts first.
///
/// # Parameters
///
/// - `bins`: list of binary names to look for (e.g., `["my-tool"]`).
/// - `target_dir`: path to the Cargo `target` directory to scan.
///
/// # Errors
///
/// Returns an error when no binaries matching `bins` are found in any inspected release directory.
pub fn infer_targets(bins: &[String], target_dir: &Path) -> Result<HashSet<String>> {
    let mut candidates: Vec<(String, PathBuf)> = Vec::new();
    match fs::read_dir(target_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry
                    .with_context(|| format!("failed to read entry in {}", target_dir.display()))?;
                let name = entry.file_name().to_string_lossy().to_string();
                if name != "release" {
                    candidates.push((name, entry.path().join("release")));
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read {}", target_dir.display()));
        }
    }
    // Always append target/release as a fallback candidate for the host platform.
    // If a triple-specific dir exists but has no bins (e.g. failed cross-compile),
    // the dedup below lets target/release take over rather than erroring out.
    if let Some(host) = HOST_PLATFORM.as_ref() {
        candidates.push((host.triple.clone(), target_dir.join("release")));
    }

    let mut targets = HashSet::new();
    for (triple, release_dir) in candidates {
        if targets.contains(&triple) {
            continue; // already found via a triple-specific dir
        }
        if !release_dir.exists() {
            continue;
        }
        if scan_bins(&release_dir, bins).is_empty() {
            continue;
        }
        match parse_triple(&triple) {
            Some(_) => {
                targets.insert(triple);
            }
            None => eprintln!(
                "warning: skipping unrecognised target triple '{triple}' - \
                 cargo-npm does not know how to map it to an npm platform"
            ),
        }
    }

    if targets.is_empty() {
        bail!("no binaries found in target/ - run `cargo build --release` first");
    }
    Ok(targets)
}

/// Copy built binaries for a specific platform into `dest_dir`.
///
/// Looks for binaries in `target/{triple}/release/`, and for the host triple will
/// fall back to `target/release/` if no per-triple release directory is present.
/// Binaries that are not found are skipped; copy and permission-setting failures
/// are returned as errors. On Windows the destination filenames will have `.exe`
/// appended; on Unix copied files are made executable (`0o755`).
pub fn copy_bins(
    bins: &[String],
    target_dir: &Path,
    platform: &Platform,
    dest_dir: &Path,
) -> Result<()> {
    let triple_dir = target_dir.join(&platform.triple).join("release");
    let src_dir = if triple_dir.exists() && !scan_bins(&triple_dir, bins).is_empty() {
        triple_dir
    } else if HOST_PLATFORM
        .as_ref()
        .is_some_and(|h| h.triple == platform.triple)
    {
        let release_dir = target_dir.join("release");
        if release_dir.exists() {
            release_dir
        } else {
            return Ok(());
        }
    } else {
        return Ok(());
    };

    for (bin_name, src_path) in scan_bins(&src_dir, bins) {
        let dest_name = if platform.os == Os::Win32 {
            format!("{bin_name}.exe")
        } else {
            bin_name.clone()
        };
        let dest = dest_dir.join(&dest_name);
        fs::copy(&src_path, &dest)
            .with_context(|| format!("failed to copy binary to {}", dest.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o755);
            fs::set_permissions(&dest, perms).context("failed to set binary permissions")?;
        }
    }
    Ok(())
}

/// Find requested binaries in a directory, mapping each requested name to the discovered file path.
///
/// For each name in `bins`, checks `dir/<name>` first and, if that does not exist, checks `dir/<name>.exe`. If a matching file is found, the function inserts an entry mapping the requested binary name (without `.exe`) to the file's `PathBuf`. Names with no matching file are omitted from the result.
fn scan_bins(dir: &Path, bins: &[String]) -> HashMap<String, PathBuf> {
    let mut found = HashMap::new();
    for bin in bins {
        let bin_path = dir.join(bin);
        let bin_exe = dir.join(format!("{bin}.exe"));
        if bin_path.exists() {
            found.insert(bin.clone(), bin_path);
        } else if bin_exe.exists() {
            found.insert(bin.clone(), bin_exe);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::HOST_PLATFORM;
    use tempfile::TempDir;

    fn bins() -> Vec<String> {
        vec!["my-tool".to_string()]
    }

    #[test]
    fn infer_targets_errors_when_no_bins() {
        let tmp = TempDir::new().unwrap();
        let result = infer_targets(&bins(), &tmp.path().join("target"));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no binaries found")
        );
    }

    #[test]
    fn infer_targets_finds_triple_dir() {
        let tmp = TempDir::new().unwrap();
        let triple = "x86_64-unknown-linux-gnu";
        let target = tmp.path().join("target");
        fs::create_dir_all(target.join(triple).join("release")).unwrap();
        fs::write(target.join(triple).join("release").join("my-tool"), b"bin").unwrap();

        let targets = infer_targets(&bins(), &target).unwrap();
        assert_eq!(targets, HashSet::from([triple.to_string()]));
    }

    #[test]
    fn infer_targets_falls_back_to_release_dir_for_host() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir_all(target.join("release")).unwrap();
        #[cfg(target_os = "windows")]
        fs::write(target.join("release").join("my-tool.exe"), b"bin").unwrap();
        #[cfg(not(target_os = "windows"))]
        fs::write(target.join("release").join("my-tool"), b"bin").unwrap();

        if HOST_PLATFORM.is_some() {
            let targets = infer_targets(&bins(), &target).unwrap();
            assert_eq!(
                targets,
                HashSet::from([HOST_PLATFORM.as_ref().unwrap().triple.clone()])
            );
        }
    }

    #[test]
    fn infer_targets_multiple_triples() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("target");
        let triples = HashSet::from([
            "x86_64-unknown-linux-gnu".to_string(),
            "aarch64-apple-darwin".to_string(),
        ]);
        for triple in &triples {
            fs::create_dir_all(target.join(triple).join("release")).unwrap();
            fs::write(target.join(triple).join("release").join("my-tool"), b"bin").unwrap();
        }

        let targets = infer_targets(&bins(), &target).unwrap();
        assert_eq!(targets, triples);
    }

    #[test]
    fn copy_bins_copies_to_dest() {
        let tmp = TempDir::new().unwrap();
        let triple = "x86_64-unknown-linux-gnu";
        let target = tmp.path().join("target");
        let dest = tmp.path().join("dest");
        fs::create_dir_all(target.join(triple).join("release")).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(target.join(triple).join("release").join("my-tool"), b"bin").unwrap();

        let platform = parse_triple(triple).unwrap();
        copy_bins(&bins(), &target, &platform, &dest).unwrap();

        assert!(dest.join("my-tool").exists());
    }

    #[test]
    fn copy_bins_skips_missing_binaries() {
        let tmp = TempDir::new().unwrap();
        let triple = "x86_64-unknown-linux-gnu";
        let target = tmp.path().join("target");
        let dest = tmp.path().join("dest");
        fs::create_dir_all(target.join(triple).join("release")).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let platform = parse_triple(triple).unwrap();
        copy_bins(&bins(), &target, &platform, &dest).unwrap();

        assert!(!dest.join("my-tool").exists());
    }

    #[test]
    fn copy_bins_falls_back_to_release_dir_for_host() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("target");
        let dest = tmp.path().join("dest");
        fs::create_dir_all(target.join("release")).unwrap();
        fs::create_dir_all(&dest).unwrap();

        if let Some(host) = HOST_PLATFORM.as_ref() {
            #[cfg(target_os = "windows")]
            fs::write(target.join("release").join("my-tool.exe"), b"bin").unwrap();
            #[cfg(not(target_os = "windows"))]
            fs::write(target.join("release").join("my-tool"), b"bin").unwrap();

            copy_bins(&bins(), &target, host, &dest).unwrap();
            assert!(
                dest.join("my-tool").exists() || dest.join("my-tool.exe").exists(),
                "binary should have been copied"
            );
        }
    }
}