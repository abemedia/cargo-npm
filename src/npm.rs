use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use crate::config::{Job, Mode};
use crate::git_url;
use crate::platform::{Libc, Os, Platform};

/// Metadata about a generated npm package, returned after writing it to disk.
pub struct PackageInfo {
    /// npm package name.
    pub name: String,
    /// Package version string.
    pub version: String,
    /// `None` for the main package.
    pub platform: Option<Platform>,
}

/// Builds the npm package name for a platform-specific package
/// (e.g. `my-tool-linux-x64` or `my-tool-linux-x64-musl`).
pub fn platform_package_name(prefix: &str, platform: &Platform) -> String {
    if platform.libc == Some(Libc::Musl) {
        return format!("{}{}-{}-musl", prefix, platform.os, platform.cpu);
    }
    format!("{}{}-{}", prefix, platform.os, platform.cpu)
}

/// Writes a platform-specific npm package to `output_dir`.
///
/// Creates the package directory, writes `package.json` with `os`/`cpu` constraints,
/// and copies any LICENSE files. Binaries are copied separately via `artifacts::copy_bins`.
pub fn generate_platform_package(
    output_dir: &Path,
    platform: &Platform,
    pkg_name: &str,
    job: &Job,
) -> Result<PackageInfo> {
    let dir = output_dir.join(pkg_name);
    if dir.exists() {
        fs::remove_dir_all(&dir).with_context(|| format!("failed to remove {}", dir.display()))?;
    }
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;

    write_json(
        &dir.join("package.json"),
        &build_platform_json(pkg_name, job, platform),
    )?;
    for name in list_extra_files(
        &job.crate_dir,
        job.meta.license_file.as_deref(),
        None,
        false,
    )? {
        let dest = name.file_name().unwrap();
        fs::copy(job.crate_dir.join(&name), dir.join(dest))
            .with_context(|| format!("failed to copy {}", name.display()))?;
    }

    Ok(PackageInfo {
        name: pkg_name.to_string(),
        version: job.meta.version.clone(),
        platform: Some(platform.clone()),
    })
}

/// Builds the `package.json` value for a platform-specific package.
pub fn build_platform_json(pkg_name: &str, job: &Job, platform: &Platform) -> Value {
    let mut pkg_json = json!({
        "name": pkg_name,
        "version": job.meta.version,
    });
    if let Some(v) = &job.meta.homepage {
        pkg_json["homepage"] = json!(v);
    }
    if let Some(v) = &job.meta.license {
        pkg_json["license"] = json!(v);
    }
    if let Some(v) = &job.meta.repository {
        pkg_json["repository"] = json!({ "type": "git", "url": git_url::normalize(v) });
    }
    pkg_json["os"] = json!([platform.os.to_string()]);
    pkg_json["cpu"] = json!([platform.cpu.to_string()]);
    if let Some(libc) = &platform.libc {
        pkg_json["libc"] = json!([libc.to_string()]);
    }
    if let Some(custom) = &job.meta.custom {
        for (k, v) in custom {
            match k.as_str() {
                "homepage" | "license" | "repository" | "publishConfig" => pkg_json[k] = v.clone(),
                _ => {}
            }
        }
    }
    pkg_json
}

/// Writes the main package to `output_dir`.
///
/// This package contains JS shims that resolve the correct platform-specific
/// binary at runtime and lists the platform packages as `optionalDependencies`.
pub fn generate_main_package(
    output_dir: &Path,
    job: &Job,
    platform_packages: &[PackageInfo],
) -> Result<PackageInfo> {
    let dir = output_dir.join(&job.name);
    if job.mode == Mode::Create && dir.exists() {
        fs::remove_dir_all(&dir).with_context(|| format!("failed to remove {}", dir.display()))?;
    }
    fs::create_dir_all(dir.join("bin"))?;

    for bin in &job.bins {
        let shim = generate_shim(&job.name, bin, platform_packages);
        let shim_path = dir.join("bin").join(format!("{bin}.js"));
        fs::write(&shim_path, shim)
            .with_context(|| format!("failed to write shim {}", shim_path.display()))?;
    }

    let pkg_json_path = dir.join("package.json");
    let mut map: Map<String, Value> = if job.mode == Mode::Merge && pkg_json_path.exists() {
        let content = fs::read_to_string(&pkg_json_path)
            .with_context(|| format!("failed to read {}", pkg_json_path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {}", pkg_json_path.display()))?
    } else {
        Map::new()
    };

    let platform_refs: BTreeMap<&str, &str> = platform_packages
        .iter()
        .map(|p| (p.name.as_str(), p.version.as_str()))
        .collect();
    if let Value::Object(built) = build_main_json(job, &platform_refs)? {
        for (k, v) in built {
            map.insert(k, v);
        }
    }
    if platform_refs.is_empty() {
        map.remove("optionalDependencies");
    }

    write_json(&pkg_json_path, &Value::Object(map))?;

    for name in list_extra_files(
        &job.crate_dir,
        job.meta.license_file.as_deref(),
        job.meta.readme_file.as_deref(),
        true,
    )? {
        let dest = name.file_name().unwrap();
        fs::copy(job.crate_dir.join(&name), dir.join(dest))
            .with_context(|| format!("failed to copy {}", name.display()))?;
    }

    Ok(PackageInfo {
        name: job.name.clone(),
        version: job.meta.version.clone(),
        platform: None,
    })
}

/// Builds the `package.json` map for the main package.
///
/// `platform_packages` maps platform package names to their versions, sorted alphabetically.
pub fn build_main_json(job: &Job, platform_packages: &BTreeMap<&str, &str>) -> Result<Value> {
    let mut pkg_json = json!({
        "name": job.name,
        "version": job.meta.version,
    });
    if let Some(v) = &job.meta.description {
        pkg_json["description"] = json!(v);
    }
    if !job.meta.keywords.is_empty() {
        pkg_json["keywords"] = json!(job.meta.keywords);
    }
    if let Some(v) = &job.meta.homepage {
        pkg_json["homepage"] = json!(v);
    }
    if let Some(v) = &job.meta.license {
        pkg_json["license"] = json!(v);
    }
    match job.meta.authors.as_slice() {
        [] => {}
        [one] => {
            pkg_json["author"] = json!(one);
        }
        many => {
            pkg_json["contributors"] = json!(many);
        }
    }
    if let Some(v) = &job.meta.repository {
        pkg_json["repository"] = json!({ "type": "git", "url": git_url::normalize(v) });
    }
    pkg_json["bin"] = Value::Object(
        job.bins
            .iter()
            .map(|b| (b.clone(), json!(format!("bin/{b}.js"))))
            .collect(),
    );
    pkg_json["engines"] = json!({"node": ">=14"});
    if !platform_packages.is_empty() {
        pkg_json["optionalDependencies"] = Value::Object(
            platform_packages
                .iter()
                .map(|(n, v)| (n.to_string(), json!(v)))
                .collect(),
        );
    }
    if let Some(custom) = &job.meta.custom {
        for (k, v) in custom {
            match k.as_str() {
                "name" => bail!("custom field \"name\" is not allowed"),
                k @ ("bin" | "optionalDependencies") => match (&mut pkg_json[k], v) {
                    (Value::Object(existing), Value::Object(new)) => {
                        for (key, val) in new {
                            if existing.insert(key.clone(), val.clone()).is_some() {
                                bail!(
                                    "custom field \"{k}.{key}\" would overwrite a generated value"
                                );
                            }
                        }
                    }
                    _ => pkg_json[k] = v.clone(),
                },
                _ => match (&mut pkg_json[k.as_str()], v) {
                    (Value::Object(existing), Value::Object(new)) => {
                        existing.extend(new.clone());
                    }
                    _ => {
                        pkg_json[k.as_str()] = v.clone();
                    }
                },
            }
        }
    }
    Ok(pkg_json)
}

/// Generates a JS shim that resolves the correct native binary at runtime.
///
/// When both glibc and musl Linux packages exist, the shim includes runtime
/// libc detection so the right variant is loaded.
fn generate_shim(name: &str, bin: &str, platform_pkgs: &[PackageInfo]) -> String {
    let mut platforms: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for pkg in platform_pkgs {
        let Some(pl) = &pkg.platform else { continue };
        let os_key = if pl.libc == Some(Libc::Musl) {
            "linux-musl".to_string()
        } else {
            pl.os.to_string()
        };
        let bin_file = if pl.os == Os::Win32 {
            format!("{bin}.exe")
        } else {
            bin.to_string()
        };
        platforms
            .entry(os_key)
            .or_default()
            .insert(pl.cpu.to_string(), format!("{}/{}", pkg.name, bin_file));
    }

    let mut platforms_js = String::from("{\n");
    for (os_key, cpus) in &platforms {
        if os_key.contains('-') {
            writeln!(platforms_js, "  '{os_key}': {{").unwrap();
        } else {
            writeln!(platforms_js, "  {os_key}: {{").unwrap();
        }
        for (cpu, path) in cpus {
            writeln!(platforms_js, "    {cpu}: '{path}',").unwrap();
        }
        platforms_js.push_str("  },\n");
    }
    platforms_js.push('}');

    let template = if platform_pkgs
        .iter()
        .filter_map(|p| p.platform.as_ref())
        .any(|p| p.libc == Some(Libc::Musl))
    {
        include_str!("shim_musl.js")
    } else {
        include_str!("shim.js")
    };

    template
        .replace("__PLATFORMS__", &platforms_js)
        .replace("__NAME__", name)
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let json_str = serde_json::to_string_pretty(value).context("failed to serialize JSON")?;
    fs::write(path, json_str).with_context(|| format!("failed to write {}", path.display()))
}

/// Returns the relative paths of licence/readme files that would be copied for a package.
/// Explicit manifest paths (license-file/readme) are preserved with their directory components.
/// Auto-discovered files are always at the crate root.
pub fn list_extra_files(
    src_dir: &Path,
    license_file: Option<&Path>,
    readme_file: Option<&Path>,
    include_readme: bool,
) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    if let Some(rel) = license_file {
        if rel.file_name().is_none() {
            bail!("invalid path in license-file: {}", rel.display());
        }
        files.push(rel.to_path_buf());
    }

    if include_readme && let Some(rel) = readme_file {
        if rel.file_name().is_none() {
            bail!("invalid path in readme: {}", rel.display());
        }
        files.push(rel.to_path_buf());
    }

    // Skip auto-discovery when all requested special files were explicitly provided.
    if license_file.is_some() && (!include_readme || readme_file.is_some()) {
        return Ok(files);
    }

    for entry in fs::read_dir(src_dir)
        .with_context(|| format!("failed to read directory {}", src_dir.display()))?
    {
        let entry = entry
            .with_context(|| format!("failed to read directory entry in {}", src_dir.display()))?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let stem = Path::new(name.as_ref())
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        let is_license = license_file.is_none()
            && (stem == "license"
                || stem == "licence"
                || stem == "copying"
                || stem.starts_with("license-")
                || stem.starts_with("licence-"));
        let is_readme = include_readme && readme_file.is_none() && stem == "readme";
        if is_license || is_readme {
            files.push(file_name.into());
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::platform_package_name;
    use crate::platform::{Cpu, Libc, Os, Platform};

    fn make_platform(os: Os, cpu: Cpu, libc: Option<Libc>) -> Platform {
        Platform {
            triple: format!("{cpu}-{os}"),
            os,
            cpu,
            libc,
        }
    }

    #[test]
    fn platform_package_name_no_libc_suffix() {
        // Single musl variant - normalise_libc strips the libc field, so no suffix.
        let platform = make_platform(Os::Linux, Cpu::X64, None);
        assert_eq!(
            platform_package_name("my-tool-", &platform),
            "my-tool-linux-x64"
        );
    }

    #[test]
    fn platform_package_name_with_libc_suffix() {
        // Dual-libc pair - musl keeps its libc field after normalisation.
        let glibc = make_platform(Os::Linux, Cpu::X64, Some(Libc::Glibc));
        let musl = make_platform(Os::Linux, Cpu::X64, Some(Libc::Musl));
        assert_eq!(
            platform_package_name("my-tool-", &glibc),
            "my-tool-linux-x64"
        );
        assert_eq!(
            platform_package_name("my-tool-", &musl),
            "my-tool-linux-x64-musl"
        );
    }
}
