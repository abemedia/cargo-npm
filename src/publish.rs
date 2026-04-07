use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::task::JoinSet;

use crate::config::Job;
use crate::npm;
use crate::platform::{self, Os, Platform};

/// A reference to a main package and it's platform packages ready for publishing.
pub struct Package {
    dir: PathBuf,
    name: String,
    version: String,
    bins: Vec<String>,
    deps: Vec<PlatformPackage>,
}

struct PlatformPackage {
    dir: PathBuf,
    name: String,
    version: String,
    platform: Platform,
}

/// Prepare a package for publishing by validating the main package and its platform-specific packages.
///
/// Verifies that the main package directory exists, that its `package.json` contains the expected fields
/// and that any declared bin shims exist. Derives platform packages from the main package's
/// `optionalDependencies`, optionally checks that configured targets are present, and validates each
/// platform package's metadata and required binaries.
///
/// # Errors
///
/// Returns an error if any verification step fails (missing directories or files, mismatched metadata,
/// missing platform packages for configured targets, or other validation errors).
pub fn prepare(output_dir: &Path, job: &Job) -> Result<Package> {
    let main_dir = output_dir.join(&job.name);
    let main_json = verify_main_package(&main_dir, job)?;
    let pkgs = platform_pkgs_from_deps(output_dir, job, &main_json)?;
    if job.targets_explicit && !job.targets.is_empty() {
        check_configured_targets(output_dir, job, &pkgs)?;
    }
    for pkg in &pkgs {
        verify_platform_package(pkg, job)?;
    }
    Ok(Package {
        dir: main_dir,
        name: job.name.clone(),
        version: job.meta.version.clone(),
        bins: job.bins.clone(),
        deps: pkgs,
    })
}

/// Publish platform-specific packages concurrently, then publish the main package.
///
/// On success the function returns `Ok(())`. It will return an error if any packaging,
/// npm publish, or registry-check operation fails for any platform package or for the
/// main package.
pub async fn publish(pkg: Package, extra_args: Arc<Vec<String>>) -> Result<()> {
    let bins = Arc::new(pkg.bins);
    let mut platform_set = JoinSet::new();
    for dep in pkg.deps {
        let bins = bins.clone();
        let extra_args = extra_args.clone();
        platform_set.spawn(async move {
            if is_published(&dep.name, &dep.version).await? {
                println!("skipping {}@{} (already published)", dep.name, dep.version);
                return Ok(());
            }
            let (dir, bins, platform) = (dep.dir.clone(), bins.clone(), dep.platform.clone());
            let tgz =
                tokio::task::spawn_blocking(move || pack_platform_package(&dir, &bins, &platform))
                    .await??;
            run_npm_publish(tgz.path(), &dep.name, &dep.version, &extra_args).await
        });
    }
    while let Some(res) = platform_set.join_next().await {
        res??;
    }
    if is_published(&pkg.name, &pkg.version).await? {
        println!(
            "skipping {}@{} (already published)",
            &pkg.name, &pkg.version
        );
        return Ok(());
    }
    run_npm_publish(&pkg.dir, &pkg.name, &pkg.version, &extra_args).await
}

/// Validate the main package directory and return its parsed `package.json`.
///
/// Reads and parses `package.json` in `main_dir`, verifies that the file contains
/// the expected fields for the main package (as produced by `npm::build_main_json`),
/// and ensures any `bin` entries reference files that exist inside `main_dir`.
///
/// # Errors
///
/// Returns an error if `main_dir` does not exist, if `package.json` cannot be read
/// or parsed, if required fields do not match the expected main package JSON, or
/// if any declared `bin` file is missing.
///
/// # Returns
///
/// The parsed `package.json` as a `serde_json::Value`.
fn verify_main_package(main_dir: &Path, job: &Job) -> Result<Value> {
    if !main_dir.exists() {
        bail!(
            "package '{}' not found - run `cargo npm generate` first",
            job.name
        );
    }
    let path = main_dir.join("package.json");
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read package.json for {}", job.name))?;
    let actual: Value = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse package.json for {}", job.name))?;

    check_fields(
        &npm::build_main_json(job, &std::collections::BTreeMap::new())?,
        &actual,
        &job.name,
    )?;

    if let Some(bin_map) = actual["bin"].as_object() {
        for (_, path) in bin_map {
            if let Some(rel) = path.as_str() {
                let abs = main_dir.join(rel);
                if !abs.exists() {
                    bail!(
                        "bin file '{}' for '{}' not found - run `cargo npm generate` first",
                        rel,
                        job.name
                    );
                }
            }
        }
    }

    Ok(actual)
}

/// Derives platform package entries from the main package's `optionalDependencies`.
///
/// Parses `main_json["optionalDependencies"]` and for each dependency whose name
/// begins with `job.prefix` attempts to parse the suffix into a `Platform`
/// (formats: `<os>-<cpu>` or `<os>-<cpu>-<libc>`). For matching entries it
/// validates the dependency version equals `job.meta.version` and computes the
/// package output directory (handles scoped names like `@scope/name`).
///
/// Errors if `optionalDependencies` is missing or invalid, if it is empty, or
/// if any matching dependency has a version different from `job.meta.version`.
///
/// # Returns
///
/// A `Vec<PlatformPackage>` containing one entry per recognized platform package.
fn platform_pkgs_from_deps(
    output_dir: &Path,
    job: &Job,
    main_json: &Value,
) -> Result<Vec<PlatformPackage>> {
    let optional_deps: HashMap<String, String> =
        serde_json::from_value(main_json["optionalDependencies"].clone()).with_context(|| {
            format!(
                "invalid optionalDependencies for '{}' - run `cargo npm generate` first",
                job.name
            )
        })?;

    if optional_deps.is_empty() {
        bail!(
            "no platform packages found for '{}' - run `cargo npm generate` first",
            job.name
        );
    }

    let mut pkgs = Vec::new();
    for (name, version) in optional_deps {
        let Some(platform) = name.strip_prefix(&job.prefix).and_then(|suffix| {
            match suffix.splitn(4, '-').collect::<Vec<_>>().as_slice() {
                [os, cpu] => Some(Platform {
                    triple: suffix.to_string(),
                    os: os.parse().ok()?,
                    cpu: cpu.parse().ok()?,
                    libc: None,
                }),
                [os, cpu, libc] => Some(Platform {
                    triple: suffix.to_string(),
                    os: os.parse().ok()?,
                    cpu: cpu.parse().ok()?,
                    libc: Some(libc.parse().ok()?),
                }),
                _ => None,
            }
        }) else {
            continue;
        };
        if version != job.meta.version {
            bail!(
                "'{name}' version in optionalDependencies is {version}, expected {} - \
                 run `cargo npm generate` to update",
                job.meta.version
            );
        }
        let dir = match name.split_once('/') {
            Some((scope, local)) => output_dir.join(scope).join(local),
            None => output_dir.join(&name),
        };
        pkgs.push(PlatformPackage {
            dir,
            name,
            version,
            platform,
        });
    }

    Ok(pkgs)
}

/// Ensures every target configured in `job.targets` has a corresponding platform package in `pkgs`.
///
/// The function parses and normalizes the configured target triples, computes the expected
/// npm platform package name for each, and verifies that a package with that name exists in
/// `pkgs`. If any expected platform packages are missing, an error is returned listing the
/// missing package names and referencing `output_dir`.
///
/// # Returns
///
/// `Ok(())` when all configured targets are present; an error describing the missing packages otherwise.
fn check_configured_targets(output_dir: &Path, job: &Job, pkgs: &[PlatformPackage]) -> Result<()> {
    let mut configured: Vec<Platform> = job
        .targets
        .iter()
        .filter_map(|t| platform::parse_triple(t))
        .collect();
    platform::normalise_libc(&mut configured);

    let pkg_names: HashSet<&str> = pkgs.iter().map(|p| p.name.as_str()).collect();
    let missing: Vec<String> = configured
        .iter()
        .filter_map(|p| {
            let name = npm::platform_package_name(&job.prefix, p);
            (!pkg_names.contains(name.as_str())).then_some(name)
        })
        .collect();

    if !missing.is_empty() {
        bail!(
            "platform package(s) {} missing from '{}' - \
             run `cargo npm generate` with all configured targets",
            missing.join(", "),
            output_dir.display()
        );
    }
    Ok(())
}

/// Validates a platform package's metadata and required binaries.
///
/// Reads and parses the package's `package.json`, verifies that all fields
/// expected for this platform (as produced by `npm::build_platform_json`) are
/// present and equal, and checks that every binary listed in `job.bins` exists
/// in the package directory (using `{bin}.exe` for Windows platforms).
///
/// Returns `Ok(())` if all expected JSON fields match and all required binaries
/// are present; returns an error describing the first mismatch or missing file
/// otherwise.
fn verify_platform_package(pkg: &PlatformPackage, job: &Job) -> Result<()> {
    if !pkg.dir.exists() {
        bail!(
            "package '{}' not found - run `cargo npm generate` first",
            job.name
        );
    }
    let path = pkg.dir.join("package.json");
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read package.json for {}", pkg.name))?;
    let actual: Value = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse package.json for {}", pkg.name))?;

    check_fields(
        &npm::build_platform_json(&pkg.name, job, &pkg.platform),
        &actual,
        &pkg.name,
    )?;
    for bin in &job.bins {
        let bin_file = if pkg.platform.os == Os::Win32 {
            format!("{bin}.exe")
        } else {
            bin.clone()
        };
        if !pkg.dir.join(&bin_file).exists() {
            bail!(
                "binary '{bin_file}' missing from '{}' - run `cargo npm generate` to copy artifacts",
                pkg.name
            );
        }
    }
    Ok(())
}

/// Validate that all fields and values in an expected JSON fragment are present in an actual JSON value.
///
/// Compares `expected` against `actual` recursively. When both values are objects, only keys present in
/// `expected` are checked (extra keys in `actual` are ignored). For non-object values, the values must
/// be equal. On mismatch, returns an error with the JSON path and the differing expected/actual values;
/// the error message also suggests running `cargo npm generate` to update expectations.
fn check_fields(expected: &Value, actual: &Value, path: &str) -> Result<()> {
    match (expected, actual) {
        (Value::Object(eo), Value::Object(_)) => {
            for (k, ev) in eo {
                check_fields(ev, &actual[k.as_str()], &format!("{path}.{k}"))?;
            }
        }
        _ if expected != actual => {
            bail!("{path}: expected {expected}, got {actual} - run `cargo npm generate` to update");
        }
        _ => {}
    }
    Ok(())
}

#[cfg(windows)]
const NPM: &str = "npm.cmd";
#[cfg(not(windows))]
const NPM: &str = "npm";

/// Checks that `npm` is available on `PATH`.
pub fn which_npm() -> Result<()> {
    let status = Command::new(NPM)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => Ok(()),
        _ => bail!("`npm` not found in PATH - please install Node.js"),
    }
}

/// Creates a deterministic `.tgz` tarball for a platform package suitable for npm publishing.
///
/// The archive will include `package.json`, the specified binaries (using `.exe` on Windows),
/// and any license files found in `dir`. Each entry is stored under the `package/` prefix,
/// file permissions are set to executable for binaries and readable for other files, and a
/// fixed mtime is used to make the tarball reproducible.
///
/// # Errors
///
/// Returns an error if creating the temp file, reading files from `dir`, or writing the tar/gzip
/// streams fails.
fn pack_platform_package(
    dir: &Path,
    bins: &[String],
    platform: &Platform,
) -> Result<tempfile::NamedTempFile> {
    let tmp = tempfile::Builder::new()
        .suffix(".tgz")
        .tempfile()
        .context("failed to create temp file for tarball")?;
    let gz = flate2::write::GzEncoder::new(tmp.as_file().try_clone()?, flate2::Compression::best());
    let mut ar = tar::Builder::new(gz);

    let mut entries: Vec<(PathBuf, String, u32)> = Vec::new();

    entries.push((dir.join("package.json"), "package.json".to_owned(), 0o644));

    for bin in bins {
        let bin_file = if platform.os == Os::Win32 {
            format!("{bin}.exe")
        } else {
            bin.clone()
        };
        entries.push((dir.join(&bin_file), bin_file, 0o755));
    }

    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry
            .with_context(|| format!("failed to read directory entry in {}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if npm::is_license_file(&name) {
            entries.push((entry.path(), name, 0o644));
        }
    }

    entries.sort_by(|a, b| a.1.cmp(&b.1));

    for (src, name, mode) in &entries {
        let mut file =
            fs::File::open(src).with_context(|| format!("failed to open {}", src.display()))?;
        let mut header = tar::Header::new_ustar();
        header.set_size(file.metadata()?.len());
        header.set_mode(*mode);
        header.set_mtime(499_162_500); // npm's fixed reproducibility epoch: 1985-10-26T08:15:00Z
        header.set_entry_type(tar::EntryType::Regular);
        ar.append_data(&mut header, format!("package/{name}"), &mut file)
            .with_context(|| format!("failed to add {} to archive", src.display()))?;
    }

    ar.into_inner()
        .context("failed to finalise tar archive")?
        .finish()
        .context("failed to finalise gzip stream")?;

    Ok(tmp)
}

/// Publishes a package (directory or tarball) to the npm registry using the configured `NPM` command.
///
/// The publication runs `npm publish <path>` with any `extra_args` appended, captures the combined
/// stdout/stderr output, prints an atomic header `publishing {name}@{version}...` followed by the
/// captured output, and returns an error if the `npm` process cannot be spawned or exits
/// unsuccessfully.
///
/// # Parameters
///
/// - `path`: path to the package directory or packaged tarball to publish.
/// - `name`: npm package name used in printed messages and error reporting.
/// - `version`: npm package version used in printed messages and error reporting.
/// - `extra_args`: additional command-line arguments to pass to `npm publish`.
///
/// # Returns
///
/// `Ok(())` if `npm publish` completes successfully; `Err(...)` if the command fails to run,
/// if reading the command output fails, or if `npm publish` exits with a non-zero status.
async fn run_npm_publish(
    path: &Path,
    name: &str,
    version: &str,
    extra_args: &[String],
) -> Result<()> {
    let (mut output, stdout) = io::pipe()?;
    let stderr = stdout.try_clone()?;

    let mut cmd = tokio::process::Command::new(NPM);
    cmd.arg("publish").arg(path);
    for arg in extra_args {
        cmd.arg(arg);
    }
    cmd.stdout(stdout).stderr(stderr);

    let mut child = cmd.spawn().context("failed to run npm publish")?;

    // Release the PipeWriter handles retained by Command.
    // Without this the parent holds open write ends and read_to_string never sees EOF.
    drop(cmd);

    let read_task = tokio::task::spawn_blocking(move || {
        let mut s = String::new();
        io::Read::read_to_string(&mut output, &mut s)?;
        Ok::<_, io::Error>(s)
    });

    let status = child.wait().await.context("failed to run npm publish")?;
    let output = read_task.await??;

    // Print atomically to avoid interleaving with concurrent publishes.
    let mut out = format!("publishing {name}@{version}...\n");
    if !output.is_empty() {
        out.push_str(&output);
    }
    print!("{out}");

    if !status.success() {
        bail!("npm publish failed for {name}@{version}");
    }

    Ok(())
}

/// Checks whether the specified npm package version is published on the registry.
///
/// Returns `true` if the registry reports the package version exists, `false` if the registry
/// reports the package/version is not found. Returns an error for other failures (for example,
/// if the `npm view` command fails for reasons other than a 404).
async fn is_published(name: &str, version: &str) -> Result<bool> {
    let output = tokio::process::Command::new(NPM)
        .args(["view", &format!("{name}@{version}"), "version"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("failed to run npm view")?;
    if output.status.success() {
        return Ok(true);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("E404") {
        return Ok(false);
    }
    bail!("npm view failed for {name}@{version}: {stderr}");
}