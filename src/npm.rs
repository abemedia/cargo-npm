use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

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

/// Constructs the npm package name for a platform-specific package.
///
/// The returned name is formed as `<prefix><os>-<cpu>`. If the platform's `libc` is
/// `Musl`, the suffix `-musl` is appended (for example: `my-tool-linux-x64-musl`).
pub fn platform_package_name(prefix: &str, platform: &Platform) -> String {
    if platform.libc == Some(Libc::Musl) {
        return format!("{}{}-{}-musl", prefix, platform.os, platform.cpu);
    }
    format!("{}{}-{}", prefix, platform.os, platform.cpu)
}

/// Generate a platform-specific npm package in the given output directory.
///
/// This creates (or replaces) a directory named for `pkg_name` under `output_dir`,
/// writes a `package.json` that constrains the package to the provided `platform`,
/// and copies license/readme files from the crate directory as appropriate.
///
/// # Returns
///
/// `PackageInfo` containing the package `name`, `version`, and `Some(platform)`.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// // Assume `job` and `platform` are constructed appropriately for your crate.
/// let out = Path::new("dist");
/// let pkg = generate_platform_package(out, &platform, "my-tool-linux-x64", &job).unwrap();
/// assert_eq!(pkg.name, "my-tool-linux-x64");
/// ```
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
    copy_special_files(
        &job.crate_dir,
        &dir,
        job.meta.license_file.as_deref(),
        None,
        false,
    )?;

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

/// Generate the main npm package directory for `job` under `output_dir`, including JS shims and a merged `package.json` that references the provided platform packages as `optionalDependencies`.
///
/// The function creates a `bin/` directory, writes executable shim files for each declared bin, merges or builds the main `package.json` according to the job's mode, removes `optionalDependencies` when no platform packages are provided, writes the resulting JSON, and copies license/README files from the crate directory into the package directory.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
///
/// // Generate the main package into ./out for a prepared `job` and list of platform packages.
/// // `job` and `platform_packages` must be constructed according to the crate's types.
/// let out = Path::new("./out");
/// let _ = crate::npm::generate_main_package(out, &job, &platform_packages);
/// ```
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

    copy_special_files(
        &job.crate_dir,
        &dir,
        job.meta.license_file.as_deref(),
        job.meta.readme_file.as_deref(),
        true,
    )?;

    Ok(PackageInfo {
        name: job.name.clone(),
        version: job.meta.version.clone(),
        platform: None,
    })
}

/// Build the `package.json` object for the main npm package.
///
/// The returned JSON contains package metadata from `job.meta` (name, version, description,
/// keywords, homepage, license), author/contributors, a `bin` map pointing each bin to `bin/<name>.js`,
/// `engines.node` set to ">=14", and (when non-empty) `optionalDependencies` mapping platform package
/// names to versions from `platform_packages`.
///
/// Custom fields from `job.meta.custom` are merged into the generated object with these rules:
/// - `"name"` is forbidden and causes an error.
/// - For `"bin"` and `"optionalDependencies"`, if both the generated value and the custom value are
///   objects, keys from the custom object are merged into the generated object; any attempt to
///   overwrite an existing generated key causes an error. Otherwise the custom value replaces the
///   generated field.
/// - For other keys, when both existing and custom values are objects they are merged (custom keys
///   extend or overwrite existing keys); otherwise the custom value replaces the field.
///
/// # Errors
///
/// Returns an error when `job.meta.custom` contains a forbidden `"name"` field or when a custom
/// `"bin"`/`"optionalDependencies"` entry would overwrite a generated key.
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
/// // `example_job()` is a test helper that returns a `Job` populated with sensible defaults.
/// let job = crate::test_helpers::example_job();
/// let platform_packages: BTreeMap<&str, &str> = BTreeMap::new();
/// let pkg_json = crate::npm::build_main_json(&job, &platform_packages).unwrap();
/// assert_eq!(pkg_json["name"], job.name);
/// ```
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

/// Create a JavaScript shim that resolves the correct platform-specific native binary for a given executable.
///
/// The returned source chooses among platform packages to load the appropriate binary (Windows `.exe` names are used where applicable). When both glibc and musl Linux packages are present, the shim includes runtime libc detection so the matching variant is selected.
///
/// - `name`: package name inserted into the shim (used in generated require/load paths).
/// - `bin`: executable basename the shim should launch.
/// - `platform_pkgs`: list of generated platform packages (each may include a `Platform`) used to build the platform→cpu→path mapping.
///
/// # Returns
///
/// A JS source string containing the shim code that resolves and invokes the correct native binary path.
///
/// # Examples
///
/// ```
/// let shim = generate_shim("my-tool", "mybin", &[]);
/// assert!(shim.contains("my-tool"));
/// ```
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

/// Serialize `value` as pretty-printed JSON and write it to `path`.
///
/// On success returns `Ok(())`. Returns an error if JSON serialization fails or if writing to
/// the filesystem fails (errors are annotated with context indicating serialization or write failure).
///
/// # Examples
///
/// ```
/// use serde_json::json;
/// use std::path::Path;
///
/// let tmp = Path::new("example.json");
/// write_json(tmp, &json!({"hello": "world"})).unwrap();
/// ```
fn write_json(path: &Path, value: &Value) -> Result<()> {
    let json_str = serde_json::to_string_pretty(value).context("failed to serialize JSON")?;
    fs::write(path, json_str).with_context(|| format!("failed to write {}", path.display()))
}

/// Copy license and, optionally, README files from `src_dir` into `dest_dir`.
///
/// If `license_file` is `Some`, that specific path (relative to `src_dir`) is copied to
/// `dest_dir` using the file name component of `license_file`. If `include_readme` is
/// `true` and `readme_file` is `Some`, that specific README path (relative to `src_dir`)
/// is also copied.
///
/// When both the requested special files are explicitly provided (i.e. `license_file.is_some()`
/// and either `include_readme` is `false` or `readme_file.is_some()`), the function does no
/// further scanning and returns after copying the explicit files. Otherwise the function
/// performs auto-discovery: it iterates the entries of `src_dir` and copies any file whose
/// name matches the license detection (`is_license_file`) when no explicit license was given,
/// and any README candidate (`is_readme_file`) when `include_readme` is `true` and no explicit
/// readme was given.
///
/// # Errors
///
/// Returns an error if reading `src_dir`, reading directory entries, or copying any file fails.
/// Error contexts include the path that failed.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
///
/// // Copy an explicit LICENSE and auto-discover README candidates from `./pkg` into `./out`.
/// copy_special_files(
///     Path::new("./pkg"),
///     Path::new("./out"),
///     Some(Path::new("LICENSE")),
///     None,
///     true,
/// ).unwrap();
/// ```
fn copy_special_files(
    src_dir: &Path,
    dest_dir: &Path,
    license_file: Option<&Path>,
    readme_file: Option<&Path>,
    include_readme: bool,
) -> Result<()> {
    if let Some(rel) = license_file {
        let src = src_dir.join(rel);
        if let Some(name) = rel.file_name() {
            fs::copy(&src, dest_dir.join(name))
                .with_context(|| format!("failed to copy license file {}", src.display()))?;
        }
    }

    if include_readme && let Some(rel) = readme_file {
        let src = src_dir.join(rel);
        if let Some(name) = rel.file_name() {
            fs::copy(&src, dest_dir.join(name))
                .with_context(|| format!("failed to copy readme file {}", src.display()))?;
        }
    }

    // Skip auto-discovery when all requested special files were explicitly provided.
    if license_file.is_some() && (!include_readme || readme_file.is_some()) {
        return Ok(());
    }

    for entry in fs::read_dir(src_dir)
        .with_context(|| format!("failed to read directory {}", src_dir.display()))?
    {
        let entry = entry
            .with_context(|| format!("failed to read directory entry in {}", src_dir.display()))?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let is_license = license_file.is_none() && is_license_file(&name);
        let is_readme = include_readme && readme_file.is_none() && is_readme_file(&name);
        if is_license || is_readme {
            fs::copy(entry.path(), dest_dir.join(name.as_ref()))?;
        }
    }
    Ok(())
}

/// Detects whether a filename resembles a license file.
///
/// This checks the file stem (name without extension) in a case-insensitive way.
/// It returns `true` for stems equal to `license`, `licence`, or `copying`,
/// and for stems that start with `license-` or `licence-`.
///
/// # Examples
///
/// ```
/// assert!(is_license_file("LICENSE"));
/// assert!(is_license_file("license-MIT.txt"));
/// assert!(is_license_file("Copying"));
/// assert!(!is_license_file("README.md"));
/// assert!(!is_license_file("not-a-license.txt"));
/// ```
pub fn is_license_file(name: &str) -> bool {
    let stem = Path::new(name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    stem == "license"
        || stem == "licence"
        || stem == "copying"
        || stem.starts_with("license-")
        || stem.starts_with("licence-")
}

/// Checks whether a filename looks like a README file.
///
/// The check compares the file stem (filename without extension) case-insensitively
/// and returns `true` only if it equals `"readme"`.
///
/// # Examples
///
/// ```
/// assert!(is_readme_file("README.md"));
/// assert!(is_readme_file("readme"));
/// assert!(is_readme_file("ReadMe.txt"));
/// assert!(!is_readme_file("readme-old.md"));
/// assert!(!is_readme_file("not_a_readme.md"));
/// ```
fn is_readme_file(name: &str) -> bool {
    Path::new(name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
        == "readme"
}

#[cfg(test)]
mod tests {
    use super::platform_package_name;
    use crate::platform::{Cpu, Libc, Os, Platform};

    /// Constructs a Platform from the given OS, CPU, and optional libc.
    ///
    /// The returned Platform's `triple` field is formatted as `"{cpu}-{os}"`.
    ///
    /// # Examples
    ///
    /// ```
    /// let pl = make_platform(Os::Linux, Cpu::X64, None);
    /// assert_eq!(pl.os, Os::Linux);
    /// assert_eq!(pl.cpu, Cpu::X64);
    /// assert_eq!(pl.libc, None);
    /// assert_eq!(pl.triple, format!("{}-{}", Cpu::X64, Os::Linux));
    /// ```
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
