use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
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

/// A reference to a main package and its platform packages ready for publishing.
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
    files: Vec<OsString>,
}

/// Verifies a job's generated npm packages and returns them ready for publishing.
/// Checks that the main package directory exists, its `package.json` is correct and bin
/// shims are present, then verifies each platform package has the right metadata and binaries.
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

/// Publishes platform packages concurrently, then publishes the main package.
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
            let dir = dep.dir.clone();
            let bins = bins.clone();
            let platform = dep.platform.clone();
            let files = dep.files.clone();
            let tgz = tokio::task::spawn_blocking(move || {
                pack_platform_package(&dir, &bins, &platform, &files)
            })
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

    for file in npm::list_extra_files(
        &job.crate_dir,
        job.meta.license_file.as_deref(),
        job.meta.readme_file.as_deref(),
        true,
    )? {
        let dest = file.file_name().unwrap();
        if !main_dir.join(dest).exists() {
            bail!(
                "'{}' missing from '{}' - run `cargo npm generate` first",
                dest.display(),
                job.name
            );
        }
    }

    Ok(actual)
}

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

    let files: Vec<OsString> = npm::list_extra_files(
        &job.crate_dir,
        job.meta.license_file.as_deref(),
        None,
        false,
    )?
    .into_iter()
    .map(|p| p.file_name().unwrap().to_owned())
    .collect();

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
            files: files.clone(),
        });
    }

    if pkgs.is_empty() {
        bail!(
            "no platform packages found for '{}' - run `cargo npm generate` first",
            job.name
        );
    }

    Ok(pkgs)
}

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

fn verify_platform_package(pkg: &PlatformPackage, job: &Job) -> Result<()> {
    if !pkg.dir.exists() {
        bail!(
            "package '{}' not found - run `cargo npm generate` first",
            pkg.name
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
    for name in &pkg.files {
        if !pkg.dir.join(name).exists() {
            bail!(
                "'{}' missing from '{}' - run `cargo npm generate` first",
                name.display(),
                pkg.name
            );
        }
    }
    Ok(())
}

/// Recursively checks that every field in `expected` exists with the correct value in `actual`.
/// For object values, only the expected keys are checked (extra keys in `actual` are ignored).
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

/// Builds a `.tgz` for a platform package with correct Unix permission bits.
/// Required for publishing from Windows where the permission bits of the binary would be lost.
fn pack_platform_package(
    dir: &Path,
    bins: &[String],
    platform: &Platform,
    files: &[OsString],
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

    for name in files {
        entries.push((dir.join(name), name.to_string_lossy().into(), 0o644));
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
    cmd.stdout(stdout).stderr(stderr).kill_on_drop(true);

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

/// Returns `true` if `name@version` already exists on the registry.
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
