use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, bail};
use serde::Deserialize;
use serde_json::{Map, Value};
use strum::{Display, EnumIter};

use crate::template::{render, render_json};

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
    pub crate_name: String,
    pub crate_dir: PathBuf,
    pub meta: PackageMeta,
    pub mode: Mode,
    /// Release asset URL template; the host's default layout when unset.
    pub pkg_url: Option<String>,
    /// Release asset archive format; every format is tried when unset.
    pub pkg_fmt: Option<PkgFmt>,
    /// Binary path template inside a release asset; inferred when unset.
    pub bin_dir: Option<String>,
    /// Per-target values for the three keys above, keyed by triple.
    pub overrides: BTreeMap<String, TargetOverrides>,
}

#[cfg(test)]
impl Job {
    /// A job with defaults for everything but the crate name and its bins.
    pub fn fake(crate_name: &str, bins: &[&str]) -> Job {
        Job {
            name: crate_name.to_owned(),
            prefix: format!("{crate_name}-"),
            bins: bins.iter().map(|b| (*b).to_owned()).collect(),
            targets: HashSet::new(),
            targets_explicit: false,
            crate_name: crate_name.to_owned(),
            crate_dir: PathBuf::from("/fake"),
            meta: PackageMeta {
                version: "1.2.3".to_owned(),
                description: None,
                license: None,
                license_file: None,
                readme_file: None,
                repository: Some(format!("https://github.com/owner/{crate_name}")),
                homepage: None,
                authors: Vec::new(),
                keywords: Vec::new(),
                custom: None,
            },
            mode: Mode::Create,
            pkg_url: None,
            pkg_fmt: None,
            bin_dir: None,
            overrides: BTreeMap::new(),
        }
    }
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

#[derive(Deserialize, Default, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Create,
    Merge,
}

/// Archive format of a release asset.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Display, EnumIter)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum PkgFmt {
    Tar,
    Tbz2,
    Tgz,
    Txz,
    Tzstd,
    Zip,
    Bin,
}

/// Release asset keys that may be set per target.
#[derive(Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TargetOverrides {
    pub pkg_url: Option<String>,
    pub pkg_fmt: Option<PkgFmt>,
    pub bin_dir: Option<String>,
}

/// Reads `cargo metadata` and resolves the full build plan.
///
/// Workspace-level `[workspace.metadata.npm]` provides defaults;
/// per-crate `[package.metadata.npm]` can override them.
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

fn unmatched_patterns(patterns: &[glob::Pattern], metadata: &cargo_metadata::Metadata) -> String {
    patterns
        .iter()
        .filter(|pat| !metadata.packages.iter().any(|p| pat.matches(&p.name)))
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn compile_glob_patterns(specs: &[String]) -> anyhow::Result<Vec<glob::Pattern>> {
    specs
        .iter()
        .map(|p| glob::Pattern::new(p).with_context(|| format!("invalid pattern `{p}`")))
        .collect()
}

/// Resolves all npm jobs for a single cargo package, returning an empty vec if
/// the package has no binary targets or no npm config.
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

/// Converts a raw config + cargo package into a resolved [`Job`].
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
        .map(|p| p.as_std_path().to_path_buf());
    let readme_file = pkg.readme.as_ref().map(|p| p.as_std_path().to_path_buf());
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
        if requested.is_empty() {
            bail!("`bins` must not be empty for '{name}'; available: {pkg_bins:?}");
        }
        let unknown: Vec<_> = requested.iter().filter(|b| !pkg_bins.contains(b)).collect();
        if !unknown.is_empty() {
            bail!("unknown bin(s) {unknown:?} for '{name}'; available: {pkg_bins:?}");
        }
        requested
    } else {
        pkg_bins.to_vec()
    };

    let overrides = raw.overrides.unwrap_or_default();
    for key in overrides.keys() {
        key.parse::<cargo_platform::Platform>()
            .with_context(|| format!("invalid overrides key '{key}' for '{name}'"))?;
    }

    Ok(Job {
        name,
        prefix,
        bins,
        targets,
        targets_explicit,
        crate_name,
        crate_dir,
        mode: raw.mode.unwrap_or_default(),
        pkg_url: raw.pkg_url,
        pkg_fmt: raw.pkg_fmt,
        bin_dir: raw.bin_dir,
        overrides,
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

/// Merges two raw configs, with `other` taking precedence over `base`.
fn merge(base: RawConfig, other: RawConfig) -> RawConfig {
    RawConfig {
        name: other.name.or(base.name),
        prefix: other.prefix.or(base.prefix),
        bins: other.bins.or(base.bins),
        targets: other.targets.or(base.targets),
        out_dir: other.out_dir.or(base.out_dir),
        mode: other.mode.or(base.mode),
        custom: match (base.custom, other.custom) {
            (None, x) | (x, None) => x,
            (Some(mut base_map), Some(other_map)) => {
                base_map.extend(other_map);
                Some(base_map)
            }
        },
        pkg_url: other.pkg_url.or(base.pkg_url),
        pkg_fmt: other.pkg_fmt.or(base.pkg_fmt),
        bin_dir: other.bin_dir.or(base.bin_dir),
        overrides: match (base.overrides, other.overrides) {
            (None, x) | (x, None) => x,
            (Some(mut base_map), Some(other_map)) => {
                base_map.extend(other_map);
                Some(base_map)
            }
        },
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
    pkg_url: Option<String>,
    pkg_fmt: Option<PkgFmt>,
    bin_dir: Option<String>,
    overrides: Option<BTreeMap<String, TargetOverrides>>,
}

/// Supports both `[package.metadata.npm]` (object) and `[[package.metadata.npm]]` (array) forms.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawConfigList {
    Single(Box<RawConfig>),
    Multiple(Vec<RawConfig>),
}

impl RawConfigList {
    fn into_vec(self) -> Vec<RawConfig> {
        match self {
            RawConfigList::Single(c) => vec![*c],
            RawConfigList::Multiple(v) => v,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BTreeMap, PkgFmt, RawConfig, RawConfigList, TargetOverrides, merge, resolve};
    use std::path::PathBuf;

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
    fn merge_asset_keys_per_key() {
        let workspace = RawConfig {
            pkg_url: Some("ws-url".into()),
            pkg_fmt: Some(PkgFmt::Tgz),
            overrides: Some(BTreeMap::from([("t1".into(), TargetOverrides::default())])),
            ..Default::default()
        };
        let crate_cfg = RawConfig {
            pkg_fmt: Some(PkgFmt::Zip),
            overrides: Some(BTreeMap::from([("t2".into(), TargetOverrides::default())])),
            ..Default::default()
        };
        let merged = merge(workspace, crate_cfg);
        assert_eq!(merged.pkg_url.as_deref(), Some("ws-url"));
        assert_eq!(merged.pkg_fmt, Some(PkgFmt::Zip));
        assert_eq!(merged.overrides.unwrap().len(), 2);
    }

    #[test]
    fn resolve_keeps_asset_keys() {
        let pkg = make_fake_package("my-crate");
        let raw = RawConfig {
            pkg_url: Some("{ repo }/x".into()),
            bin_dir: Some("{ bin }".into()),
            overrides: Some(BTreeMap::from([(
                "x86_64-pc-windows-msvc".into(),
                TargetOverrides {
                    pkg_fmt: Some(PkgFmt::Zip),
                    ..Default::default()
                },
            )])),
            ..Default::default()
        };
        let job = resolve(
            raw,
            &pkg,
            &["my-crate".to_string()],
            PathBuf::from("/fake"),
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(job.crate_name, "my-crate");
        assert_eq!(job.pkg_url.as_deref(), Some("{ repo }/x"));
        assert_eq!(job.pkg_fmt, None);
        assert_eq!(job.bin_dir.as_deref(), Some("{ bin }"));
        assert_eq!(
            job.overrides["x86_64-pc-windows-msvc"].pkg_fmt,
            Some(PkgFmt::Zip)
        );
    }

    #[test]
    fn resolve_rejects_invalid_override_key() {
        let pkg = make_fake_package("my-crate");
        let raw = RawConfig {
            overrides: Some(BTreeMap::from([(
                "cfg(target_os = linux)".into(),
                TargetOverrides::default(),
            )])),
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
        .expect("expected error for invalid override key");
        assert!(err.to_string().contains("invalid overrides key"), "{err}");
    }

    #[test]
    fn override_unknown_field_is_rejected() {
        let json = r#"{"overrides": {"t": {"bins": ["x"]}}}"#;
        assert!(serde_json::from_str::<RawConfig>(json).is_err());
    }

    #[test]
    fn resolve_rejects_empty_bins() {
        let pkg = make_fake_package("my-crate");
        let raw = RawConfig {
            bins: Some(vec![]),
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
        .expect("expected error for empty bins");
        assert!(err.to_string().contains("must not be empty"), "{err}");
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
