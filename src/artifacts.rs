use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::platform::{HOST_PLATFORM, Os, Platform, parse_triple};

/// Scans `target_dir` for triples that have at least one of the given binaries built.
///
/// Checks both `target/{triple}/release/` subdirectories and `target/release/` for
/// the host platform. Returns an error if no binaries are found at all.
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
        if !bins.iter().any(|bin| {
            release_dir.join(bin).exists() || release_dir.join(format!("{bin}.exe")).exists()
        }) {
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
        bail!("no binaries found in target directory - run `cargo build --release` first");
    }
    Ok(targets)
}

/// Copies built binaries for a single platform into `dest_dir`.
///
/// Looks for binaries in `target/{triple}/release/`, falling back to
/// `target/release/` for the host platform. Missing binaries are silently skipped.
pub fn copy_bins(
    bins: &[String],
    target_dir: &Path,
    platform: &Platform,
    dest_dir: &Path,
) -> Result<()> {
    let bin_filename = |bin: &str| -> String {
        if platform.os == Os::Win32 {
            format!("{bin}.exe")
        } else {
            bin.to_string()
        }
    };

    let triple_dir = target_dir.join(&platform.triple).join("release");
    let src_dir = if triple_dir.exists()
        && bins
            .iter()
            .any(|b| triple_dir.join(bin_filename(b)).exists())
    {
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

    for bin in bins {
        let filename = bin_filename(bin);
        let src = src_dir.join(&filename);
        if !src.exists() {
            continue;
        }
        let dest = dest_dir.join(&filename);
        fs::copy(&src, &dest)
            .with_context(|| format!("failed to copy binary to {}", dest.display()))?;
    }
    Ok(())
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
