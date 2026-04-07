use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, bail};
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Deserialize, Default, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Create,
    Merge,
}

use crate::template::{render, render_json};

/// Full build plan resolved from `cargo metadata` and npm config.
pub struct Build {
    pub output_dir: PathBuf,
    pub target_dir: PathBuf,
    pub jobs: Vec<Job>,
}

/// Configuration for one npm package group: a main package and its platform-specific packages.
pub struct Job {
    pub name: String,
    pub prefix: String,
    pub bins: Vec<String>,
    pub targets: HashSet<String>,
    pub targets_explicit: bool,
    pub crate_dir: PathBuf,
    pub meta: PackageMeta,
    pub mode: Mode,
}

/// Cargo package metadata forwarded into generated `package.json` files.
pub struct PackageMeta {
    pub version: String,
    pub description: Option<String>,
    pub license: Option<String>,
    pub license_file: Option<PathBuf>,
    pub readme_file: Option<PathBuf>,
    pub repository: Option<String>,
    pub homepage: Option<String>,
    pub authors: Vec<String>,
    pub keywords: Vec<String>,
    pub custom: Option<Map<String, Value>>,
}

pub struct LoadOpts {
    pub manifest_path: Option<PathBuf>,
    pub package: Vec<String>,
    pub workspace: bool,
    pub exclude: Vec<String>,
    pub cli_targets: Vec<String>,
    pub use_cargo_config: bool,
    pub target_dir: Option<PathBuf>,
    pub out_dir: Option<String>,
}

/// Resolve a complete npm build plan by running `cargo metadata` and merging workspace-level
/// `[workspace.metadata.npm]` defaults with per-crate `[package.metadata.npm]` overrides.
///
/// This function determines workspace context, computes the output and target directories,
/// selects which Cargo packages to include (respecting CLI include/exclude patterns, manifest
/// path, and workspace flags), and produces a `Build` containing the resolved `jobs` for each
/// included package. It validates patterns, enforces workspace restrictions (e.g., disallowing
/// per-crate `out_dir` in workspace mode), and rejects duplicate resolved npm package names.
///
/// # Returns
///
/// A `Build` containing `output_dir`, `target_dir`, and the list of resolved `jobs`.
///
/// # Errors
///
/// Returns an error if `cargo metadata` or cargo config cannot be loaded, if workspace or
/// per-crate npm configuration is invalid, if include patterns match no package, or if duplicate
/// npm package names are produced.
pub fn load(opts: LoadOpts) -> anyhow::Result<Build> {
    let mut cmd = cargo_metadata::MetadataCommand::new();
    if let Some(path) = &opts.manifest_path {
        cmd.manifest_path(path);
    }
    let metadata = cmd
        .no_deps()
        .exec()
        .map_err(|e| anyhow::anyhow!("failed to run cargo metadata: {e}"))?;

    let workspace_root = metadata.workspace_root.as_std_path();
    let current_dir = std::env::current_dir()?;
    let is_workspace = metadata.root_package().is_none() || metadata.packages.len() > 1;

    let workspace_base: RawConfig = match metadata.workspace_metadata.get("npm") {
        None => RawConfig::default(),
        Some(v) => serde_json::from_value::<RawConfig>(v.clone())
            .context("invalid [workspace.metadata.npm] config")?,
    };

    let output_dir = path_clean::clean(opts.out_dir.as_deref().map_or_else(
        || workspace_root.join(workspace_base.out_dir.as_deref().unwrap_or("npm")),
        |o| current_dir.join(o),
    ));

    if !opts.exclude.is_empty() && !opts.workspace {
        bail!("--exclude can only be used together with --workspace");
    }

    let pkg_patterns = compile_glob_patterns(&opts.package)?;
    let exclude_patterns = compile_glob_patterns(&opts.exclude)?;

    let target_id = opts.manifest_path.as_deref().and_then(|mp| {
        let abs = path_clean::clean(current_dir.join(mp));
        metadata
            .packages
            .iter()
            .find(|p| p.manifest_path.as_std_path() == abs)
            .map(|p| &p.id)
    });

    let cargo_config_targets: Vec<String> = if opts.use_cargo_config && opts.cli_targets.is_empty()
    {
        cargo_config2::Config::load()
            .context("failed to load cargo config")?
            .build_target_for_cli(std::iter::empty::<&str>())
            .context("failed to resolve build targets")?
    } else {
        vec![]
    };

    let mut jobs = Vec::new();
    for package in &metadata.packages {
        let pkg_dir = package.manifest_path.parent().unwrap().as_std_path();
        let included = if opts.workspace {
            true
        } else if !opts.package.is_empty() {
            pkg_patterns.iter().any(|p| p.matches(&package.name))
        } else if let Some(id) = target_id {
            &package.id == id
        } else if opts.manifest_path.is_some() {
            true // manifest path given but matched no member - workspace manifest
        } else {
            current_dir == workspace_root || current_dir.starts_with(pkg_dir)
        };
        let excluded = exclude_patterns.iter().any(|p| p.matches(&package.name));
        if !included || excluded {
            continue;
        }
        for job in resolve_package(
            package,
            &workspace_base,
            pkg_dir.to_path_buf(),
            is_workspace,
            &opts.cli_targets,
            &cargo_config_targets,
        )? {
            jobs.push(job);
        }
    }

    let unmatched_pkg = unmatched_patterns(&pkg_patterns, &metadata);
    if !unmatched_pkg.is_empty() {
        bail!(
            "package pattern(s) `{unmatched_pkg}` not found in workspace `{}`",
            workspace_root.display()
        );
    }

    let unmatched_exclude = unmatched_patterns(&exclude_patterns, &metadata);
    if !unmatched_exclude.is_empty() {
        eprintln!(
            "warning: excluded package(s) `{unmatched_exclude}` not found in workspace `{}`",
            workspace_root.display()
        );
    }

    let mut seen = std::collections::HashSet::new();
    for job in &jobs {
        if !seen.insert(&job.name) {
            bail!("duplicate npm package name '{}'", job.name);
        }
    }

    Ok(Build {
        output_dir,
        target_dir: opts
            .target_dir
            .unwrap_or_else(|| metadata.target_directory.as_std_path().to_path_buf()),
        jobs,
    })
}

/// Returns a comma-separated list of glob patterns that do not match any package name in the given Cargo metadata.
///
/// The returned string contains the original pattern texts joined by ", " for every pattern that fails to match any package in `metadata.packages`.
fn unmatched_patterns(patterns: &[glob::Pattern], metadata: &cargo_metadata::Metadata) -> String {
    patterns
        .iter()
        .filter(|pat| !metadata.packages.iter().any(|p| pat.matches(&p.name)))
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Compiles a slice of glob pattern strings into `glob::Pattern` instances.
///
/// Returns a vector of compiled `glob::Pattern` on success. Errors if any input
/// string is not a valid glob pattern; the error includes the offending pattern.
fn compile_glob_patterns(specs: &[String]) -> anyhow::Result<Vec<glob::Pattern>> {
    specs
        .iter()
        .map(|p| glob::Pattern::new(p).with_context(|| format!("invalid pattern `{p}`")))
        .collect()
}

/// Resolve npm Job definitions for a single Cargo package by combining workspace defaults,
/// per-crate npm config, and CLI/cargo-config targets.
///
/// Returns a list of resolved `Job`s for each npm config entry (or a single job derived
/// from the workspace defaults when no per-crate config is present). If the crate has
/// no binary targets or no npm configuration, returns an empty `Vec`.
///
/// The function validates that per-crate `out_dir` is not used when operating in
/// workspace mode and propagates any resolution errors.
fn resolve_package(
    package: &cargo_metadata::Package,
    workspace_base: &RawConfig,
    crate_dir: PathBuf,
    is_workspace: bool,
    cli_targets: &[String],
    cargo_config_targets: &[String],
) -> anyhow::Result<Vec<Job>> {
    let pkg_bins: Vec<String> = package
        .targets
        .iter()
        .filter(|t| t.kind.iter().any(|k| k == &cargo_metadata::TargetKind::Bin))
        .map(|t| t.name.clone())
        .collect();

    if pkg_bins.is_empty() {
        return Ok(vec![]);
    }

    let pkg_raw: Vec<RawConfig> = match package.metadata.get("npm") {
        None => vec![],
        Some(v) => serde_json::from_value::<RawConfigList>(v.clone())
            .with_context(|| {
                format!(
                    "invalid [package.metadata.npm] config in '{}'",
                    package.name
                )
            })?
            .into_vec(),
    };

    if is_workspace {
        for raw in &pkg_raw {
            if raw.out_dir.is_some() {
                bail!(
                    "`out-dir` cannot be set in [package.metadata.npm] for '{}' - \
                     use [workspace.metadata.npm] instead",
                    package.name
                );
            }
        }
    }

    if pkg_raw.is_empty() {
        return Ok(vec![resolve(
            workspace_base.clone(),
            package,
            &pkg_bins,
            crate_dir,
            cli_targets,
            cargo_config_targets,
        )?]);
    }

    pkg_raw
        .into_iter()
        .map(|raw| {
            resolve(
                merge(workspace_base.clone(), raw),
                package,
                &pkg_bins,
                crate_dir.clone(),
                cli_targets,
                cargo_config_targets,
            )
        })
        .collect()
}

/// Resolve a crate-level `RawConfig` and Cargo package metadata into a concrete `Job`.
///
/// This produces the final job fields used for packaging: a rendered `name` and `prefix` (template
/// variables `{name}` and `{version}`), derived `license`/`license_file`/`readme_file`, rendered
/// `custom` JSON (must be an object), resolved `targets` with precedence (CLI targets → per-package
/// `raw.targets` → `cargo_config_targets`), and validated `bins` (errors if any requested bin does
/// not exist). The returned `Job` also contains `targets_explicit` indicating whether targets came
/// from an explicit source (CLI or per-package) and a populated `PackageMeta`.
///
/// Errors are returned if template rendering or JSON rendering fails, or if unknown bin names are
/// requested.
fn resolve(
    raw: RawConfig,
    pkg: &cargo_metadata::Package,
    pkg_bins: &[String],
    crate_dir: PathBuf,
    cli_targets: &[String],
    cargo_config_targets: &[String],
) -> anyhow::Result<Job> {
    let crate_name = pkg.name.to_string();
    let version = pkg.version.to_string();
    let vars = std::collections::HashMap::from([
        ("name", crate_name.as_str()),
        ("version", version.as_str()),
    ]);

    let name = raw
        .name
        .map(|s| render(&s, &vars))
        .transpose()?
        .unwrap_or_else(|| crate_name.clone());
    let prefix = raw
        .prefix
        .map(|s| render(&s, &vars))
        .transpose()?
        .unwrap_or_else(|| format!("{name}-"));
    let license_file = pkg
        .license_file
        .as_ref()
        .map(|p| crate_dir.join(p.as_std_path()));
    let readme_file = pkg.readme.as_ref().map(|p| crate_dir.join(p.as_std_path()));
    let license = pkg.license.clone().or_else(|| {
        license_file
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|f| format!("SEE LICENSE IN {}", f.to_string_lossy()))
    });

    let custom = raw
        .custom
        .map(|m| render_json(m, &vars))
        .transpose()?
        .map(|v| match v {
            Value::Object(m) => m,
            _ => unreachable!(),
        });

    // Target resolution: CLI → per-package → workspace → cargo_config2.
    // `raw.targets` already encodes per-package > workspace via merge().
    let (targets, targets_explicit) = if !cli_targets.is_empty() {
        (cli_targets.to_vec(), true)
    } else if let Some(t) = raw.targets.filter(|t| !t.is_empty()) {
        (t, true)
    } else {
        (cargo_config_targets.to_vec(), false)
    };
    let targets: HashSet<String> = targets.into_iter().collect();

    let bins = if let Some(requested) = raw.bins {
        let unknown: Vec<_> = requested.iter().filter(|b| !pkg_bins.contains(b)).collect();
        if !unknown.is_empty() {
            bail!("unknown bin(s) {unknown:?} for '{name}'; available: {pkg_bins:?}",);
        }
        requested
    } else {
        pkg_bins.to_vec()
    };

    Ok(Job {
        name,
        prefix,
        bins,
        targets,
        targets_explicit,
        crate_dir,
        mode: raw.mode.unwrap_or_default(),
        meta: PackageMeta {
            version,
            description: pkg.description.clone(),
            license,
            license_file,
            readme_file,
            repository: pkg.repository.clone().filter(|r| !r.is_empty()),
            homepage: pkg.homepage.clone(),
            authors: pkg.authors.clone(),
            keywords: pkg.keywords.clone(),
            custom,
        },
    })
}

/// Produce a `RawConfig` by overlaying `other` onto `base`, where each field from
/// `other` replaces the corresponding field from `base` when present.
fn merge(base: RawConfig, other: RawConfig) -> RawConfig {
    RawConfig {
        name: other.name.or(base.name),
        prefix: other.prefix.or(base.prefix),
        bins: other.bins.or(base.bins),
        targets: other.targets.or(base.targets),
        out_dir: other.out_dir.or(base.out_dir),
        mode: other.mode.or(base.mode),
        custom: other.custom.or(base.custom),
    }
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawConfig {
    name: Option<String>,
    prefix: Option<String>,
    bins: Option<Vec<String>>,
    targets: Option<Vec<String>>,
    out_dir: Option<String>,
    mode: Option<Mode>,
    custom: Option<Map<String, Value>>,
}

/// Supports both `[package.metadata.npm]` (object) and `[[package.metadata.npm]]` (array) forms.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawConfigList {
    Single(Box<RawConfig>),
    Multiple(Vec<RawConfig>),
}

impl RawConfigList {
    /// Flatten a RawConfigList into its contained entries.
    ///
    /// Converts a `Single` into a one-element vector and returns the inner vector for `Multiple`.
    fn into_vec(self) -> Vec<RawConfig> {
        match self {
            RawConfigList::Single(c) => vec![*c],
            RawConfigList::Multiple(v) => v,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RawConfig, RawConfigList, merge, resolve};
    use std::path::PathBuf;

    /// Create a synthetic `cargo_metadata::Package` for tests using the given crate name.
    ///
    /// The produced package has reasonable default fields (version "0.1.0", a path-based id,
    /// empty targets/dependencies/features, and null/empty optional metadata) suitable for unit tests
    /// that only need a basic package structure.
    fn make_fake_package(name: &str) -> cargo_metadata::Package {
        let json = serde_json::json!({
            "name": name,
            "version": "0.1.0",
            "id": format!("{name} 0.1.0 (path+file:///fake)"),
            "source": null,
            "dependencies": [],
            "targets": [],
            "features": {},
            "manifest_path": "/fake/Cargo.toml",
            "metadata": null,
            "publish": null,
            "authors": [],
            "categories": [],
            "default_run": null,
            "description": null,
            "edition": "2021",
            "keywords": [],
            "license": null,
            "license_file": null,
            "links": null,
            "readme": null,
            "repository": null,
            "rust_version": null,
            "homepage": null,
        });
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn resolve_uses_defaults() {
        let pkg = make_fake_package("my-crate");
        let job = resolve(
            RawConfig::default(),
            &pkg,
            &["my-crate".to_string()],
            PathBuf::from("/fake"),
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(job.name, "my-crate");
        assert_eq!(job.prefix, "my-crate-");
        assert_eq!(job.bins, ["my-crate"]);
        assert!(job.meta.custom.is_none());
    }

    #[test]
    fn resolve_uses_explicit_values() {
        let pkg = make_fake_package("my-crate");
        let raw = RawConfig {
            name: Some("my-tool".into()),
            prefix: Some("@org/cli-".into()),
            bins: Some(vec!["bin-a".into()]),
            ..Default::default()
        };
        let job = resolve(
            raw,
            &pkg,
            &["bin-a".to_string(), "bin-b".to_string()],
            PathBuf::from("/fake"),
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(job.name, "my-tool");
        assert_eq!(job.prefix, "@org/cli-");
        assert_eq!(job.bins, ["bin-a"]);
    }

    #[test]
    fn resolve_rejects_unknown_bins() {
        let pkg = make_fake_package("my-crate");
        let raw = RawConfig {
            bins: Some(vec!["bin-a".into(), "bin-b".into()]),
            ..Default::default()
        };
        let err = resolve(
            raw,
            &pkg,
            &["my-crate".to_string()],
            PathBuf::from("/fake"),
            &[],
            &[],
        )
        .err()
        .expect("expected error for unknown bins");
        assert!(err.to_string().contains("unknown bin(s)"), "{err}");
    }

    #[test]
    fn resolve_prefix_derived_from_name() {
        let pkg = make_fake_package("my-crate");
        let raw = RawConfig {
            name: Some("custom-name".into()),
            ..Default::default()
        };
        let job = resolve(raw, &pkg, &[], PathBuf::from("/fake"), &[], &[]).unwrap();
        assert_eq!(job.name, "custom-name");
        assert_eq!(job.prefix, "custom-name-");
    }

    #[test]
    fn merge_crate_overrides_workspace() {
        let workspace = RawConfig {
            name: Some("workspace-name".into()),
            prefix: Some("ws-".into()),
            bins: Some(vec!["ws-bin".into()]),
            targets: Some(vec!["x86_64-unknown-linux-gnu".into()]),
            ..Default::default()
        };
        let crate_cfg = RawConfig {
            name: Some("crate-name".into()),
            out_dir: Some("out".into()),
            ..Default::default()
        };
        let merged = merge(workspace, crate_cfg);
        assert_eq!(merged.name.as_deref(), Some("crate-name"));
        assert_eq!(merged.prefix.as_deref(), Some("ws-"));
        assert_eq!(merged.bins.as_deref(), Some(&["ws-bin".to_string()][..]));
        assert_eq!(
            merged.targets.as_deref(),
            Some(&["x86_64-unknown-linux-gnu".to_string()][..])
        );
        assert_eq!(merged.out_dir.as_deref(), Some("out"));
    }

    #[test]
    fn array_form_parses() {
        let json = r#"[{"name": "tool-a"}, {"name": "tool-b"}]"#;
        let list: RawConfigList = serde_json::from_str(json).unwrap();
        let vec = list.into_vec();
        assert_eq!(vec.len(), 2);
        assert_eq!(vec[0].name.as_deref(), Some("tool-a"));
        assert_eq!(vec[1].name.as_deref(), Some("tool-b"));
    }

    #[test]
    fn unknown_field_is_rejected() {
        let json = r#"{"name": "tool", "unknown-field": true}"#;
        let result = serde_json::from_str::<RawConfig>(json);
        assert!(result.is_err());
    }
}