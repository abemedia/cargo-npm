//! cargo-binstall's conventions for locating release assets and the binaries inside them.
//!
//! Copied from cargo-binstall so that any crate installable with `cargo binstall` resolves
//! identically. Each item notes its source.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};
use cargo_platform::Cfg;

use crate::config::PkgFmt;
use crate::template::render;

/// Default asset filename templates, in probe order.
///
/// From `binstalk-fetchers/src/gh_crate_meta/hosting.rs`.
pub(super) const FILENAMES: &[&str] = &[
    "{ name }-{ target }-v{ version }{ archive-suffix }",
    "{ name }-{ target }-{ version }{ archive-suffix }",
    "{ name }-{ version }-{ target }{ archive-suffix }",
    "{ name }-v{ version }-{ target }{ archive-suffix }",
    "{ name }_{ target }_v{ version }{ archive-suffix }",
    "{ name }_{ target }_{ version }{ archive-suffix }",
    "{ name }_{ version }_{ target }{ archive-suffix }",
    "{ name }_v{ version }_{ target }{ archive-suffix }",
    "{ name }-{ target }{ archive-suffix }",
    "{ name }_{ target }{ archive-suffix }",
];

/// Directory names an archive may nest its contents under, in priority order.
///
/// From `binstalk-bins/src/lib.rs`.
const BIN_DIRS: &[&str] = &[
    "{ name }-{ target }-v{ version }",
    "{ name }-{ target }-{ version }",
    "{ name }-{ version }-{ target }",
    "{ name }-v{ version }-{ target }",
    "{ name }-{ target }",
    "{ name }-{ version }",
    "{ name }-v{ version }",
    "{ name }",
];

/// Binary filename template inside its directory.
pub(super) const BIN_FILE: &str = "{ bin }{ binary-ext }";

/// Release tag templates, in probe order.
///
/// From the `*_RELEASE_PATHS` tables in `hosting.rs`, plus `{ name }-v{ version }`, the tag
/// scheme cargo-dist and release-plz use for independently versioned workspace members.
pub(super) const TAGS: &[&str] = &[
    "{ version }",
    "v{ version }",
    "{ subcrate }%2F{ version }",
    "{ subcrate }%2Fv{ version }",
    "{ name }-v{ version }",
];

/// Hosting service a repository URL points at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Host {
    GitHub,
    GitLab,
    BitBucket,
    SourceForge,
    Codeberg,
    Other,
}

impl Host {
    /// Classifies `domain` the way `RepositoryHost::guess_git_hosting_services` does.
    pub(super) fn detect(domain: &str) -> Host {
        match domain {
            d if d.starts_with("github") => Host::GitHub,
            d if d.starts_with("gitlab") => Host::GitLab,
            "bitbucket.org" => Host::BitBucket,
            "sourceforge.net" => Host::SourceForge,
            "codeberg.org" => Host::Codeberg,
            _ => Host::Other,
        }
    }

    /// Release download directory template for this host, with `{ tag }` for the release tag.
    ///
    /// From `hosting.rs`. `None` for hosts without a default layout.
    pub(super) fn release_path(self) -> Option<&'static str> {
        Some(match self {
            Host::GitHub | Host::Codeberg => "{ repo }/releases/download/{ tag }",
            Host::GitLab => "{ repo }/-/releases/{ tag }/downloads/binaries",
            Host::BitBucket => "{ repo }/downloads",
            Host::SourceForge => "{ repo }/files/binaries/{ tag }",
            Host::Other => return None,
        })
    }

    /// Path separators between `owner/repo` and a subcrate path in browse URLs.
    fn subcrate_seps(self) -> &'static [&'static str] {
        match self {
            Host::GitHub => &["tree"],
            Host::GitLab => &["-", "blob"],
            Host::Codeberg => &["src", "branch"],
            _ => &[],
        }
    }
}

/// A repository URL split into the parts templates refer to.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Repo {
    pub host: Host,
    pub domain: String,
    /// `scheme://domain/owner/name`, with any subcrate path removed.
    pub url: String,
    pub owner: String,
    pub name: String,
    /// Workspace member the URL points at, e.g. `crates/foo` -> `foo`.
    pub subcrate: Option<String>,
}

impl Repo {
    /// Parses a `repository` URL, following `RepoInfo::detect_subcrate`.
    pub(super) fn parse(url: &str) -> Result<Repo> {
        let url = url.split(['#', '?']).next().unwrap_or(url);
        let url = url.trim_end_matches('/').trim_end_matches(".git");
        let Some((scheme, rest)) = url.split_once("://") else {
            bail!("repository URL {url:?} has no scheme");
        };
        let (domain, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = Host::detect(domain);
        let mut segments = path.split('/').filter(|s| !s.is_empty());
        let (Some(owner), Some(name)) = (segments.next(), segments.next()) else {
            bail!("repository URL {url:?} has no owner/name path");
        };
        let rest: Vec<&str> = segments.collect();
        let subcrate = detect_subcrate(&rest, host.subcrate_seps()).map(str::to_owned);
        Ok(Repo {
            host,
            domain: domain.to_owned(),
            url: format!("{scheme}://{domain}/{owner}/{name}"),
            owner: owner.to_owned(),
            name: name.to_owned(),
            subcrate,
        })
    }
}

fn detect_subcrate<'a>(segments: &[&'a str], seps: &[&str]) -> Option<&'a str> {
    if seps.is_empty() {
        return None;
    }
    let mut it = segments.iter().copied();
    for sep in seps {
        if it.next()? != *sep {
            return None;
        }
    }
    let _branch = it.next()?;
    let subcrate = match it.next()? {
        "crates" => it.next()?,
        s => s,
    };
    if it.next().is_some() {
        return None;
    }
    Some(subcrate)
}

/// Values available to templates, following the contexts in `gh_crate_meta.rs` and
/// `binstalk-bins`.
/// The cfg values a `cfg(...)` override key is evaluated against for `triple`.
///
/// Derived from the triple like binstall's `TargetTriple::cfgs`, so `cfg(target_env = "musl")`
/// and `cfg(unix)` match the same targets they do there.
pub(super) fn cfgs(triple: &str) -> Vec<Cfg> {
    let parts: Vec<&str> = triple.splitn(4, '-').collect();
    let part = |i: usize| parts.get(i).copied().unwrap_or("");
    let family = if part(2) == "windows" {
        "windows"
    } else {
        "unix"
    };
    [
        family.to_owned(),
        format!("target_family = \"{family}\""),
        format!("target_arch = \"{}\"", part(0)),
        format!("target_vendor = \"{}\"", part(1)),
        format!("target_os = \"{}\"", part(2)),
        format!("target_env = \"{}\"", part(3)),
    ]
    .iter()
    .map(|s| s.parse().expect("valid cfg"))
    .collect()
}

#[derive(Clone, Debug)]
pub(super) struct Vars {
    pub name: String,
    pub repo: String,
    pub subcrate: Option<String>,
    pub version: String,
    pub target: String,
}

impl Vars {
    /// Whether `target` is a Windows triple.
    pub(super) fn windows(&self) -> bool {
        self.target.contains("windows")
    }

    /// Renders `template` with the variables binstall provides, plus `extra`.
    pub(super) fn render(&self, template: &str, extra: &[(&str, &str)]) -> Result<String> {
        let parts: Vec<&str> = self.target.splitn(4, '-').collect();
        let os = parts.get(2).copied().unwrap_or("");
        let family = if os == "windows" { "windows" } else { "unix" };
        let binary_ext = if self.windows() { ".exe" } else { "" };
        let mut vars = HashMap::from([
            ("name", self.name.as_str()),
            ("repo", self.repo.as_str()),
            ("version", self.version.as_str()),
            ("target", self.target.as_str()),
            ("binary-ext", binary_ext),
            ("target-arch", parts.first().copied().unwrap_or("")),
            ("target-vendor", parts.get(1).copied().unwrap_or("")),
            ("target-os", os),
            ("target-libc", parts.get(3).copied().unwrap_or("")),
            ("target-family", family),
        ]);
        if let Some(s) = &self.subcrate {
            vars.insert("subcrate", s);
        }
        vars.extend(extra.iter().copied());
        render(template, &vars)
    }

    /// Renders a filename or URL template for one archive format and suffix.
    pub(super) fn render_asset(&self, template: &str, fmt: PkgFmt, suffix: &str) -> Result<String> {
        let format = fmt.to_string();
        self.render(
            template,
            &[
                ("archive-suffix", suffix),
                ("archive-format", &format),
                ("format", &format),
            ],
        )
    }

    /// Renders a template that refers to a binary.
    pub(super) fn render_bin(&self, template: &str, bin: &str) -> Result<String> {
        self.render(template, &[("bin", bin)])
    }
}

/// Resolves where `bin` lives inside an extracted archive at `dir`.
///
/// Uses `bin_dir` when set, otherwise infers the layout by testing [`BIN_DIRS`] in order and
/// falling back to the archive root, as `infer_bin_dir_template` does. Inference uses `found`,
/// the name the asset was published under, since that is what its directory is named after;
/// an explicit `bin_dir` renders `{ name }` as the crate name, as binstall does. The rendered
/// path must be relative and non-empty; existence is left to the caller.
pub(super) fn bin_path(
    dir: &Path,
    bin_dir: Option<&str>,
    vars: &Vars,
    found: &str,
    bin: &str,
) -> Result<PathBuf> {
    let template = if let Some(t) = bin_dir {
        t.to_owned()
    } else {
        let found = Vars {
            name: found.to_owned(),
            ..vars.clone()
        };
        BIN_DIRS
            .iter()
            .map(|d| found.render_bin(d, bin))
            .find(|d| d.as_ref().is_ok_and(|d| dir.join(d).is_dir()))
            .transpose()?
            .map_or_else(|| BIN_FILE.to_owned(), |d| format!("{d}/{BIN_FILE}"))
    };
    let rendered = vars.render_bin(&template, bin)?;
    let rel = path_clean::clean(&rendered);
    match rel.components().next() {
        None => bail!("bin-dir {template:?} renders to an empty path"),
        Some(Component::CurDir) if rel.components().count() == 1 => {
            bail!("bin-dir {template:?} renders to an empty path")
        }
        Some(Component::Prefix(_) | Component::RootDir | Component::ParentDir) => {
            bail!("bin-dir {template:?} renders to a path outside the archive: {rendered}")
        }
        _ => Ok(dir.join(rel)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn cfgs_mirror_rustc_for_triple() {
        let cfg = cfgs("aarch64-unknown-linux-musl");
        let names: Vec<String> = cfg.iter().map(ToString::to_string).collect();
        assert_eq!(
            names,
            [
                "unix",
                r#"target_family = "unix""#,
                r#"target_arch = "aarch64""#,
                r#"target_vendor = "unknown""#,
                r#"target_os = "linux""#,
                r#"target_env = "musl""#,
            ]
        );
        let cfg = cfgs("aarch64-apple-darwin");
        assert!(cfg.iter().any(|c| c.to_string() == r#"target_env = """#));
        let cfg = cfgs("x86_64-pc-windows-msvc");
        assert_eq!(cfg[0].to_string(), "windows");
    }

    fn vars() -> Vars {
        Vars {
            name: "my-tool".into(),
            repo: "https://github.com/owner/my-tool".into(),
            subcrate: None,
            version: "1.2.3".into(),
            target: "x86_64-unknown-linux-gnu".into(),
        }
    }

    #[test]
    fn repo_parses_plain_url() {
        let repo = Repo::parse("https://github.com/owner/my-tool.git").unwrap();
        assert_eq!(repo.host, Host::GitHub);
        assert_eq!(repo.url, "https://github.com/owner/my-tool");
        assert_eq!(repo.owner, "owner");
        assert_eq!(repo.name, "my-tool");
        assert_eq!(repo.subcrate, None);
    }

    #[test]
    fn repo_detects_subcrate() {
        let repo = Repo::parse("https://github.com/owner/ws/tree/main/crates/foo").unwrap();
        assert_eq!(repo.url, "https://github.com/owner/ws");
        assert_eq!(repo.subcrate.as_deref(), Some("foo"));
        let repo = Repo::parse("https://gitlab.com/owner/ws/-/blob/main/foo").unwrap();
        assert_eq!(repo.subcrate.as_deref(), Some("foo"));
        let repo = Repo::parse("https://github.com/owner/ws/tree/main/crates/foo/src").unwrap();
        assert_eq!(repo.subcrate, None);
    }

    #[test]
    fn repo_ignores_fragment_and_query() {
        let repo = Repo::parse("https://github.com/owner/my-tool.git#main?x=1").unwrap();
        assert_eq!(repo.domain, "github.com");
        assert_eq!(repo.name, "my-tool");
    }

    #[test]
    fn bin_path_infers_from_the_name_the_asset_was_found_under() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("cli-x86_64-unknown-linux-gnu")).unwrap();
        let path = bin_path(tmp.path(), None, &vars(), "cli", "cli").unwrap();
        assert_eq!(path, tmp.path().join("cli-x86_64-unknown-linux-gnu/cli"));
        // An explicit template still renders `{ name }` as the crate name.
        let path = bin_path(tmp.path(), Some("{ name }/{ bin }"), &vars(), "cli", "cli").unwrap();
        assert_eq!(path, tmp.path().join("my-tool/cli"));
    }

    #[test]
    fn repo_rejects_incomplete_urls() {
        assert!(Repo::parse("github.com/owner/repo").is_err());
        assert!(Repo::parse("https://github.com/owner").is_err());
    }

    #[test]
    fn host_detection() {
        assert_eq!(Host::detect("github.com"), Host::GitHub);
        assert_eq!(Host::detect("gitlab.example.com"), Host::GitLab);
        assert_eq!(Host::detect("codeberg.org"), Host::Codeberg);
        assert_eq!(Host::detect("example.com"), Host::Other);
    }

    #[test]
    fn render_asset_provides_binstall_variables() {
        let v = vars();
        let s = v
            .render_asset(
                "{ repo }/{ name }-{ target }-v{ version }{ archive-suffix }.{ archive-format }",
                PkgFmt::Tgz,
                ".tar.gz",
            )
            .unwrap();
        assert_eq!(
            s,
            "https://github.com/owner/my-tool/my-tool-x86_64-unknown-linux-gnu-v1.2.3.tar.gz.tgz"
        );
        assert_eq!(
            v.render("{ target-arch }/{ target-family }/{ target-libc }", &[])
                .unwrap(),
            "x86_64/unix/gnu"
        );
        assert!(v.render("{ subcrate }", &[]).is_err());
    }

    #[test]
    fn render_bin_uses_binary_ext_on_windows() {
        let mut v = vars();
        v.target = "x86_64-pc-windows-msvc".into();
        assert_eq!(v.render_bin(BIN_FILE, "my-tool").unwrap(), "my-tool.exe");
        assert_eq!(v.render("{ target-family }", &[]).unwrap(), "windows");
        assert_eq!(vars().render_bin(BIN_FILE, "my-tool").unwrap(), "my-tool");
    }

    #[test]
    fn bin_path_infers_nested_dir_by_priority() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("my-tool")).unwrap();
        fs::create_dir_all(tmp.path().join("my-tool-x86_64-unknown-linux-gnu")).unwrap();
        let path = bin_path(tmp.path(), None, &vars(), "my-tool", "my-tool").unwrap();
        assert_eq!(
            path,
            tmp.path().join("my-tool-x86_64-unknown-linux-gnu/my-tool")
        );
    }

    #[test]
    fn bin_path_falls_back_to_root() {
        let tmp = TempDir::new().unwrap();
        let path = bin_path(tmp.path(), None, &vars(), "my-tool", "my-tool").unwrap();
        assert_eq!(path, tmp.path().join("my-tool"));
    }

    #[test]
    fn bin_path_uses_configured_bin_dir() {
        let tmp = TempDir::new().unwrap();
        let path = bin_path(
            tmp.path(),
            Some("bin/{ bin }{ binary-ext }"),
            &vars(),
            "my-tool",
            "x",
        )
        .unwrap();
        assert_eq!(path, tmp.path().join("bin/x"));
    }

    #[test]
    fn bin_path_rejects_escaping_paths() {
        let tmp = TempDir::new().unwrap();
        assert!(bin_path(tmp.path(), Some("../{ bin }"), &vars(), "my-tool", "x").is_err());
        assert!(bin_path(tmp.path(), Some("/{ bin }"), &vars(), "my-tool", "x").is_err());
        assert!(bin_path(tmp.path(), Some(""), &vars(), "my-tool", "x").is_err());
        assert!(bin_path(tmp.path(), Some("."), &vars(), "my-tool", "x").is_err());
        assert!(bin_path(tmp.path(), Some("a/../{ bin }"), &vars(), "my-tool", "x").is_ok());
    }
}
