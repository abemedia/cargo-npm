use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::Binaries;
use crate::config::Job;
use crate::platform::{HOST_PLATFORM, parse_triple};

/// Finds the job's binaries under cargo's target directory.
///
/// Looks in `target/{triple}/release/`, falling back to `target/release/` for the host
/// platform. With `infer`, every triple with at least one binary counts and finding none is an
/// error; otherwise only the job's configured targets are checked and missing binaries are
/// simply absent.
pub fn locate(dir: &Path, job: &Job, infer: bool) -> Result<Binaries> {
    let mut candidates: Vec<(String, PathBuf)> = Vec::new();
    if infer {
        match fs::read_dir(dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry
                        .with_context(|| format!("failed to read entry in {}", dir.display()))?;
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name != "release" {
                        candidates.push((name, entry.path().join("release")));
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("failed to read {}", dir.display())),
        }
    } else {
        candidates.extend(
            job.targets
                .iter()
                .map(|triple| (triple.clone(), dir.join(triple).join("release"))),
        );
    }
    // target/release is the fallback for the host platform. A triple-specific dir that has no
    // bins (e.g. a failed cross-compile) lets it take over rather than shadowing it.
    if let Some(host) = HOST_PLATFORM.as_ref()
        && (infer || job.targets.contains(&host.triple))
    {
        candidates.push((host.triple.clone(), dir.join("release")));
    }

    let mut found = Binaries::new();
    for (triple, release_dir) in candidates {
        if found.contains_key(&triple) {
            continue;
        }
        let Some(platform) = parse_triple(&triple) else {
            if infer {
                eprintln!(
                    "warning: skipping unrecognised target triple '{triple}' - \
                     cargo-npm does not know how to map it to an npm platform"
                );
            }
            continue;
        };
        let bins: BTreeMap<String, PathBuf> = job
            .bins
            .iter()
            .map(|bin| (bin.clone(), release_dir.join(platform.bin_filename(bin))))
            .filter(|(_, path)| path.is_file())
            .collect();
        if !bins.is_empty() {
            found.insert(triple, bins);
        }
    }

    if infer && found.is_empty() {
        bail!("no binaries found in target directory - run `cargo build --release` first");
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn job(targets: &[&str]) -> Job {
        let mut job = Job::fake("my-tool", &["my-tool"]);
        job.targets = targets.iter().map(|t| (*t).to_string()).collect();
        job
    }

    fn build(target: &Path, triple: &str, bin: &str) {
        let dir = if triple.is_empty() {
            target.join("release")
        } else {
            target.join(triple).join("release")
        };
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(bin), b"bin").unwrap();
    }

    #[test]
    fn infer_errors_when_no_bins() {
        let tmp = TempDir::new().unwrap();
        let err = locate(&tmp.path().join("target"), &job(&[]), true).unwrap_err();
        assert!(err.to_string().contains("no binaries found"));
    }

    #[test]
    fn infer_finds_every_triple_dir() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("target");
        build(&target, "x86_64-unknown-linux-gnu", "my-tool");
        build(&target, "aarch64-apple-darwin", "my-tool");
        build(&target, "not-a-real-triple", "my-tool");

        let found = locate(&target, &job(&[]), true).unwrap();
        assert_eq!(
            found.keys().collect::<Vec<_>>(),
            ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]
        );
        assert_eq!(
            found["x86_64-unknown-linux-gnu"]["my-tool"],
            target.join("x86_64-unknown-linux-gnu/release/my-tool")
        );
    }

    #[test]
    fn explicit_targets_skip_missing_binaries() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("target");
        build(&target, "x86_64-unknown-linux-gnu", "my-tool");

        let job = job(&["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]);
        let found = locate(&target, &job, false).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found.contains_key("x86_64-unknown-linux-gnu"));
        assert!(
            locate(&tmp.path().join("missing"), &job, false)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn windows_binaries_have_exe_suffix() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("target");
        build(&target, "x86_64-pc-windows-msvc", "my-tool.exe");

        let found = locate(&target, &job(&["x86_64-pc-windows-msvc"]), false).unwrap();
        assert_eq!(
            found["x86_64-pc-windows-msvc"]["my-tool"],
            target.join("x86_64-pc-windows-msvc/release/my-tool.exe")
        );
    }

    #[test]
    fn host_falls_back_to_release_dir() {
        let Some(host) = HOST_PLATFORM.as_ref() else {
            return;
        };
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("target");
        build(&target, "", &host.bin_filename("my-tool"));

        let inferred = locate(&target, &job(&[]), true).unwrap();
        assert!(inferred.contains_key(&host.triple));
        let explicit = locate(&target, &job(&[&host.triple]), false).unwrap();
        assert_eq!(
            explicit[&host.triple]["my-tool"],
            target.join("release").join(host.bin_filename("my-tool"))
        );
    }
}
