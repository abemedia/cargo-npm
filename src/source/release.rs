//! Binaries fetched from a release published by another tool.
//!
//! Assets are located and unpacked by cargo-binstall's conventions (see `conventions`), so any
//! release `cargo binstall` can install resolves identically.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use cargo_platform::Platform;
use reqwest::{Client, header};
use serde::Deserialize;
use tokio::task::{JoinHandle, JoinSet};

use strum::IntoEnumIterator;

use self::conventions::{BIN_FILE, FILENAMES, Repo, TAGS, Vars, bin_path, cfgs};
use super::Binaries;
use crate::config::{Job, PkgFmt, TargetOverrides};

mod conventions;

const GITHUB_API: &str = "https://api.github.com";
const MAX_PROBES: usize = 16;

/// Release assets for every job, downloaded and extracted concurrently.
///
/// Archives are unpacked under `{output_dir}/.tmp/{package}/{triple}`, removed on drop.
pub struct Release {
    scratch: PathBuf,
    /// One task per npm package.
    fetches: HashMap<String, JoinHandle<Result<Fetches>>>,
}

/// The extracted asset of each triple of one job.
type Fetches = Vec<(String, Fetched)>;

/// An extracted asset for one target.
#[derive(Debug)]
struct Fetched {
    dir: PathBuf,
    bin_dir: Option<String>,
    /// Template values with `{ name }` as the crate name.
    vars: Vars,
    /// The name the asset was found under: the crate name or one of its bins.
    found: String,
    /// The downloaded file itself when the asset is a bare binary (`pkg-fmt = "bin"`).
    bin: Option<PathBuf>,
}

struct Asset {
    url: String,
    fmt: PkgFmt,
}

impl Release {
    /// Starts fetching every job's targets into `{output_dir}/.tmp`.
    pub fn new(output_dir: &Path, jobs: &[Job]) -> Result<Release> {
        let scratch = output_dir.join(".tmp");
        if scratch.exists() {
            fs::remove_dir_all(&scratch)
                .with_context(|| format!("failed to remove {}", scratch.display()))?;
        }
        fs::create_dir_all(&scratch)?;
        // Keep a crashed run's leftovers out of `git status`, wherever the output dir lives.
        fs::write(scratch.join(".gitignore"), "*\n")?;

        let client = client()?;
        let mut tasks = HashMap::new();
        for job in jobs {
            let Some(repository) = &job.meta.repository else {
                bail!(
                    "--from-release needs `repository` in the Cargo.toml of '{}'",
                    job.crate_name
                );
            };
            let repo = Repo::parse(repository)?;
            // Only github.com has the API; GitHub Enterprise is probed like any other host.
            let api = (repo.domain == "github.com").then(|| GITHUB_API.to_owned());
            let token = api
                .as_ref()
                .and_then(|_| std::env::var("GITHUB_TOKEN").ok());
            let fetcher = Arc::new(Fetcher {
                client: client.clone(),
                api,
                token,
                repo,
                name: job.crate_name.clone(),
                version: job.meta.version.clone(),
                bins: job.bins.clone(),
                assets: TargetOverrides {
                    pkg_url: job.pkg_url.clone(),
                    pkg_fmt: job.pkg_fmt,
                    bin_dir: job.bin_dir.clone(),
                },
                overrides: job.overrides.clone(),
            });
            let targets: Vec<String> = job.targets.iter().cloned().collect();
            let dir = scratch.join(&job.name);
            let task = tokio::spawn(fetcher.fetch_all(targets, dir));
            tasks.insert(job.name.clone(), task);
        }
        Ok(Release {
            scratch,
            fetches: tasks,
        })
    }

    /// Waits for the job's assets and resolves the path of each of its binaries inside them.
    pub async fn locate(&mut self, job: &Job) -> Result<Binaries> {
        let fetched = match self.fetches.remove(&job.name) {
            Some(task) => task.await??,
            None => Vec::new(),
        };

        let mut found = Binaries::new();
        for (triple, asset) in fetched {
            let mut bins = BTreeMap::new();
            for bin in &job.bins {
                let path = match &asset.bin {
                    Some(file) => file.clone(),
                    None => bin_path(
                        &asset.dir,
                        asset.bin_dir.as_deref(),
                        &asset.vars,
                        &asset.found,
                        bin,
                    )?,
                };
                let rel = path.strip_prefix(&asset.dir).unwrap_or(&path).display();
                if bins.values().any(|p| *p == path) {
                    bail!("binaries resolve to the same file {rel} in the {triple} asset");
                }
                if !path.is_file() {
                    bail!(
                        "binary '{bin}' not found at {rel} in the {triple} asset - set bin-dir \
                         if the archive uses a different layout, or remove it from `bins`"
                    );
                }
                bins.insert(bin.clone(), path);
            }
            found.insert(triple, bins);
        }
        Ok(found)
    }
}

impl Drop for Release {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.scratch);
    }
}

/// The HTTP client shared by every request.
fn client() -> Result<Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::builder()
        .user_agent(concat!("cargo-npm/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to build HTTP client")
}

/// Locates, downloads and extracts assets; shared by all per-target tasks of a job.
struct Fetcher {
    client: Client,
    /// GitHub API base URL, for repositories on github.com.
    api: Option<String>,
    /// Sent only to the API.
    token: Option<String>,
    repo: Repo,
    name: String,
    version: String,
    bins: Vec<String>,
    /// Asset keys for targets without an override.
    assets: TargetOverrides,
    overrides: BTreeMap<String, TargetOverrides>,
}

impl Fetcher {
    /// Asset keys for `triple`: the job's defaults, overlaid by every matching override.
    ///
    /// Like binstall, the override named `triple` applies first, then `cfg(...)` overrides
    /// matching it in key order; later ones win per key.
    fn assets(&self, triple: &str) -> TargetOverrides {
        let cfgs = cfgs(triple);
        let by_cfg = self.overrides.iter().filter_map(|(key, o)| {
            let matches = key.starts_with("cfg(")
                && key
                    .parse::<Platform>()
                    .is_ok_and(|p| p.matches(triple, &cfgs));
            matches.then_some(o)
        });
        self.overrides
            .get(triple)
            .into_iter()
            .chain(by_cfg)
            .fold(self.assets.clone(), |base, o| TargetOverrides {
                pkg_url: o.pkg_url.clone().or(base.pkg_url),
                pkg_fmt: o.pkg_fmt.or(base.pkg_fmt),
                bin_dir: o.bin_dir.clone().or(base.bin_dir),
            })
    }

    /// Template values for `triple`, with `{ name }` set to `name`.
    fn vars(&self, name: &str, triple: &str) -> Vars {
        Vars {
            name: name.to_owned(),
            repo: self.repo.url.clone(),
            subcrate: self.repo.subcrate.clone(),
            version: self.version.clone(),
            target: triple.to_owned(),
        }
    }

    /// Names assets may be published under: the crate's, then each differently named bin's.
    ///
    /// binstall retries with the binary name when the crate has a single, differently named
    /// binary; this retries with every one.
    fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str()).chain(
            self.bins
                .iter()
                .map(String::as_str)
                .filter(|b| *b != self.name),
        )
    }

    /// Fetches every target of a job into `dir/{triple}`.
    ///
    /// The release listing is fetched once and shared; every target is awaited even after one
    /// fails, so no extraction on the blocking pool outlives the scratch dir.
    async fn fetch_all(self: Arc<Self>, targets: Vec<String>, dir: PathBuf) -> Result<Fetches> {
        let listing = match &self.api {
            Some(_) => Some(Arc::new(self.github_assets().await?)),
            None => None,
        };
        let mut set = JoinSet::new();
        for triple in targets {
            let fetcher = Arc::clone(&self);
            let listing = listing.clone();
            let dir = dir.join(&triple);
            set.spawn(async move {
                let result = fetcher.fetch(&triple, dir, listing.as_deref()).await;
                (triple, result)
            });
        }
        let mut fetched = Vec::new();
        let mut error = None;
        while let Some(joined) = set.join_next().await {
            match joined
                .map_err(anyhow::Error::from)
                .and_then(|(t, r)| r.map(|f| (t, f)))
            {
                Ok(pair) => fetched.push(pair),
                Err(e) => {
                    error.get_or_insert(e);
                }
            }
        }
        match error {
            Some(e) => Err(e),
            None => Ok(fetched),
        }
    }

    async fn fetch(
        &self,
        triple: &str,
        dir: PathBuf,
        listing: Option<&Vec<GhAsset>>,
    ) -> Result<Fetched> {
        let meta = self.assets(triple);
        for name in self.names() {
            let vars = self.vars(name, triple);
            let Some(asset) = self.locate(&meta, &vars, listing).await? else {
                continue;
            };
            let bytes = self.download(&asset.url).await?;
            let bin_file = vars.render_bin(BIN_FILE, name)?;
            let out = dir.clone();
            let file = bin_file.clone();
            tokio::task::spawn_blocking(move || extract(asset.fmt, &bytes, &out, &file)).await??;
            return Ok(Fetched {
                bin: (asset.fmt == PkgFmt::Bin).then(|| dir.join(&bin_file)),
                dir,
                bin_dir: meta.bin_dir,
                vars: Vars {
                    name: self.name.clone(),
                    ..vars
                },
                found: name.to_owned(),
            });
        }
        bail!(
            "no release asset found for {triple} in {} {}",
            self.repo.url,
            self.version
        )
    }

    /// Finds the asset for one target, or `None` if the release has none for it.
    ///
    /// With a release listing and no `pkg-url`, default filenames are matched against it;
    /// otherwise candidate URLs are probed.
    async fn locate(
        &self,
        meta: &TargetOverrides,
        vars: &Vars,
        listing: Option<&Vec<GhAsset>>,
    ) -> Result<Option<Asset>> {
        match (listing, &meta.pkg_url) {
            (Some(assets), None) => locate_in_listing(meta, vars, assets),
            _ => self.locate_by_probing(meta, vars).await,
        }
    }

    /// Lists the assets of the release whose tag matches one of [`TAGS`] for any of the
    /// crate's names.
    async fn github_assets(&self) -> Result<Vec<GhAsset>> {
        let mut tags = Vec::new();
        for name in self.names() {
            let vars = self.vars(name, "");
            for template in TAGS {
                if template.contains("subcrate") && vars.subcrate.is_none() {
                    continue;
                }
                let tag = vars.render(template, &[])?.replace("%2F", "/");
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
        }
        let api = self.api.as_deref().unwrap_or(GITHUB_API);
        let url = format!(
            "{api}/repos/{}/{}/releases",
            self.repo.owner, self.repo.name
        );
        for page in 1.. {
            let releases: Vec<GhRelease> = self
                .request(&format!("{url}?per_page=100&page={page}"))
                .header(header::ACCEPT, "application/vnd.github+json")
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .with_context(|| format!("failed to list releases of {}", self.repo.url))?
                .json()
                .await
                .context("failed to parse release list")?;
            if releases.is_empty() {
                break;
            }
            if let Some(release) = releases.into_iter().find(|r| tags.contains(&r.tag_name)) {
                return Ok(release.assets);
            }
        }
        bail!(
            "no release tagged {} found in {}",
            tags.join(", "),
            self.repo.url
        )
    }

    /// Probes candidate URLs, [`MAX_PROBES`] at a time in order, and returns the first that
    /// exists.
    async fn locate_by_probing(
        &self,
        meta: &TargetOverrides,
        vars: &Vars,
    ) -> Result<Option<Asset>> {
        let candidates = self.candidates(meta, vars)?;
        for chunk in candidates.chunks(MAX_PROBES) {
            let mut set = JoinSet::new();
            for (i, (url, fmt)) in chunk.iter().cloned().enumerate() {
                let client = self.client.clone();
                set.spawn(async move {
                    let ok = client
                        .head(&url)
                        .send()
                        .await
                        .is_ok_and(|r| r.status().is_success());
                    (i, ok.then_some(Asset { url, fmt }))
                });
            }
            let mut best: Option<(usize, Asset)> = None;
            while let Some(res) = set.join_next().await {
                if let (i, Some(asset)) = res?
                    && best.as_ref().is_none_or(|(j, _)| i < *j)
                {
                    best = Some((i, asset));
                }
            }
            if let Some((_, asset)) = best {
                return Ok(Some(asset));
            }
        }
        Ok(None)
    }

    /// Candidate asset URLs in probe order: the configured `pkg-url` or the host's default
    /// layout, expanded over formats and suffixes.
    fn candidates(&self, meta: &TargetOverrides, vars: &Vars) -> Result<Vec<(String, PkgFmt)>> {
        let windows = vars.windows();
        let mut out = Vec::new();
        if let Some(pkg_url) = &meta.pkg_url {
            if !has_var(pkg_url, &["archive-suffix", "archive-format", "format"]) {
                let url = vars.render(pkg_url, &[])?;
                let Some(fmt) = meta.pkg_fmt.or_else(|| guess_format(&url)) else {
                    bail!("cannot infer the archive format of {url}; set pkg-fmt");
                };
                return Ok(vec![(url, fmt)]);
            }
            for fmt in formats(meta.pkg_fmt) {
                for suffix in suffixes(fmt, windows) {
                    out.push((vars.render_asset(pkg_url, fmt, suffix)?, fmt));
                }
            }
            return Ok(out);
        }

        let Some(path) = self.repo.host.release_path() else {
            bail!(
                "no default release layout for {}; set pkg-url in [package.metadata.npm]",
                self.repo.url
            );
        };
        for tag in TAGS {
            if tag.contains("subcrate") && vars.subcrate.is_none() {
                continue;
            }
            let tag = vars.render(tag, &[])?;
            let dir = vars.render(path, &[("tag", &tag)])?;
            for fmt in formats(meta.pkg_fmt) {
                for suffix in suffixes(fmt, windows) {
                    for template in FILENAMES {
                        let name = vars.render_asset(template, fmt, suffix)?;
                        out.push((format!("{dir}/{name}"), fmt));
                    }
                }
            }
        }
        Ok(out)
    }

    async fn download(&self, url: &str) -> Result<Vec<u8>> {
        let bytes = self
            .request(url)
            .header(header::ACCEPT, "application/octet-stream")
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .with_context(|| format!("failed to download {url}"))?
            .bytes()
            .await
            .with_context(|| format!("failed to download {url}"))?;
        Ok(bytes.to_vec())
    }

    /// A GET request, authenticated when it targets the API.
    fn request(&self, url: &str) -> reqwest::RequestBuilder {
        let req = self.client.get(url);
        match (&self.token, &self.api) {
            (Some(token), Some(api)) if url.starts_with(api.as_str()) => req.bearer_auth(token),
            _ => req,
        }
    }
}

/// Whether `template` refers to any of `vars` as a `{ var }` placeholder.
fn has_var(template: &str, vars: &[&str]) -> bool {
    template
        .split('{')
        .skip(1)
        .filter_map(|s| s.split('}').next())
        .any(|v| vars.contains(&v.trim()))
}

#[derive(Deserialize, Debug)]
struct GhRelease {
    tag_name: String,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize, Debug)]
struct GhAsset {
    name: String,
    url: String,
}

/// Matches the default filenames for one target against a release's assets.
fn locate_in_listing(
    meta: &TargetOverrides,
    vars: &Vars,
    assets: &[GhAsset],
) -> Result<Option<Asset>> {
    for fmt in formats(meta.pkg_fmt) {
        for suffix in suffixes(fmt, vars.windows()) {
            for template in FILENAMES {
                let name = vars.render_asset(template, fmt, suffix)?;
                if let Some(asset) = assets.iter().find(|a| a.name == name) {
                    return Ok(Some(Asset {
                        url: asset.url.clone(),
                        fmt,
                    }));
                }
            }
        }
    }
    Ok(None)
}

/// Formats to try: the configured one, or all of them.
fn formats(pkg_fmt: Option<PkgFmt>) -> Vec<PkgFmt> {
    pkg_fmt.map_or_else(|| PkgFmt::iter().collect(), |fmt| vec![fmt])
}

/// Filename suffixes for `fmt`, in probe order.
///
/// From `PkgFmt::extensions` in `binstalk-types`.
fn suffixes(fmt: PkgFmt, windows: bool) -> &'static [&'static str] {
    match fmt {
        PkgFmt::Tar => &[".tar"],
        PkgFmt::Tbz2 => &[".tbz2", ".tar.bz2", ".tbz", ".tar.bz"],
        PkgFmt::Tgz => &[".tgz", ".tar.gz"],
        PkgFmt::Txz => &[".txz", ".tar.xz"],
        PkgFmt::Tzstd => &[".tzstd", ".tzst", ".tar.zst"],
        PkgFmt::Zip => &[".zip"],
        PkgFmt::Bin if windows => &[".bin", "", ".exe"],
        PkgFmt::Bin => &[".bin", ""],
    }
}

/// Guesses the archive format from the extension of a rendered `pkg-url`.
///
/// From `PkgFmt::guess_pkg_format` in `binstalk-types`.
fn guess_format(url: &str) -> Option<PkgFmt> {
    let mut it = url.rsplitn(3, '.');
    let last = it.next()?;
    let mut tar = || it.next() == Some("tar");
    match last {
        "tar" => Some(PkgFmt::Tar),
        "tbz2" | "tbz" => Some(PkgFmt::Tbz2),
        "bz2" | "bz" if tar() => Some(PkgFmt::Tbz2),
        "tgz" => Some(PkgFmt::Tgz),
        "gz" if tar() => Some(PkgFmt::Tgz),
        "txz" => Some(PkgFmt::Txz),
        "xz" if tar() => Some(PkgFmt::Txz),
        "tzstd" | "tzst" => Some(PkgFmt::Tzstd),
        "zst" if tar() => Some(PkgFmt::Tzstd),
        "exe" | "bin" => Some(PkgFmt::Bin),
        "zip" => Some(PkgFmt::Zip),
        _ => None,
    }
}

/// Unpacks an asset into `dir`. A bare binary is written as `bin_file`.
fn extract(fmt: PkgFmt, bytes: &[u8], dir: &Path, bin_file: &str) -> Result<()> {
    fs::create_dir_all(dir)?;
    let unpack = |reader: &mut dyn Read| tar::Archive::new(reader).unpack(dir);
    match fmt {
        PkgFmt::Tar => unpack(&mut Cursor::new(bytes))?,
        PkgFmt::Tgz => unpack(&mut flate2::read::GzDecoder::new(bytes))?,
        PkgFmt::Tbz2 => unpack(&mut bzip2::read::BzDecoder::new(bytes))?,
        PkgFmt::Txz => {
            let mut tar = Vec::new();
            lzma_rs::xz_decompress(&mut Cursor::new(bytes), &mut tar)?;
            unpack(&mut Cursor::new(tar))?;
        }
        PkgFmt::Tzstd => unpack(&mut ruzstd::decoding::StreamingDecoder::new(bytes)?)?,
        PkgFmt::Zip => zip::ZipArchive::new(Cursor::new(bytes))?.extract(dir)?,
        PkgFmt::Bin => fs::write(dir.join(bin_file), bytes)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn tgz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut ar = tar::Builder::new(gz);
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            ar.append_data(&mut header, name, *data).unwrap();
        }
        ar.into_inner().unwrap().finish().unwrap()
    }

    /// A fetcher for `repository`, using `api` as the GitHub API when the host is github.com.
    fn fetcher(api: &str, repository: &str, assets: TargetOverrides) -> Fetcher {
        let repo = Repo::parse(repository).unwrap();
        Fetcher {
            client: client().unwrap(),
            api: (repo.domain == "github.com").then(|| api.to_owned()),
            token: None,
            repo,
            name: "my-tool".into(),
            version: "1.2.3".into(),
            bins: vec!["my-tool".into()],
            assets,
            overrides: BTreeMap::new(),
        }
    }

    #[test]
    fn assets_apply_target_override_per_key() {
        let mut f = fetcher(
            "",
            "https://github.com/owner/my-tool",
            TargetOverrides {
                pkg_url: Some("url".into()),
                pkg_fmt: Some(PkgFmt::Tgz),
                bin_dir: None,
            },
        );
        f.overrides = BTreeMap::from([(
            "x86_64-pc-windows-msvc".to_string(),
            TargetOverrides {
                pkg_fmt: Some(PkgFmt::Zip),
                bin_dir: Some("dir".into()),
                ..Default::default()
            },
        )]);
        let win = f.assets("x86_64-pc-windows-msvc");
        assert_eq!(win.pkg_url.as_deref(), Some("url"));
        assert_eq!(win.pkg_fmt, Some(PkgFmt::Zip));
        assert_eq!(win.bin_dir.as_deref(), Some("dir"));
        let other = f.assets("aarch64-apple-darwin");
        assert_eq!(other.pkg_fmt, Some(PkgFmt::Tgz));
        assert_eq!(other.bin_dir, None);
    }

    #[test]
    fn assets_apply_cfg_overrides_after_named() {
        let mut f = fetcher(
            "",
            "https://github.com/owner/my-tool",
            TargetOverrides::default(),
        );
        let with = |pkg_fmt, bin_dir: &str| TargetOverrides {
            pkg_fmt: Some(pkg_fmt),
            bin_dir: Some(bin_dir.into()),
            ..Default::default()
        };
        f.overrides = BTreeMap::from([
            (
                "x86_64-pc-windows-msvc".to_string(),
                with(PkgFmt::Tgz, "named"),
            ),
            ("cfg(windows)".to_string(), with(PkgFmt::Zip, "windows")),
            (
                r#"cfg(all(unix, target_env = "musl"))"#.to_string(),
                with(PkgFmt::Txz, "musl"),
            ),
        ]);
        let win = f.assets("x86_64-pc-windows-msvc");
        assert_eq!(win.pkg_fmt, Some(PkgFmt::Zip));
        assert_eq!(win.bin_dir.as_deref(), Some("windows"));
        let musl = f.assets("x86_64-unknown-linux-musl");
        assert_eq!(musl.pkg_fmt, Some(PkgFmt::Txz));
        let gnu = f.assets("x86_64-unknown-linux-gnu");
        assert_eq!(gnu.pkg_fmt, None);
        assert_eq!(gnu.bin_dir, None);
    }

    #[tokio::test]
    async fn github_listing_matches_default_names_and_downloads_api_url() {
        let server = MockServer::start().await;
        let asset_url = format!("{}/assets/42", server.uri());
        Mock::given(method("GET"))
            .and(path("/repos/owner/my-tool/releases"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "tag_name": "v9.9.9", "assets": [] },
                { "tag_name": "v1.2.3", "draft": true, "assets": [
                    { "name": "my-tool-x86_64-unknown-linux-gnu.tar.gz", "url": asset_url }
                ] }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/assets/42"))
            .and(header("accept", "application/octet-stream"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(tgz(&[("my-tool", b"bin")])))
            .mount(&server)
            .await;

        let tmp = TempDir::new().unwrap();
        let f = fetcher(
            &server.uri(),
            "https://github.com/owner/my-tool",
            TargetOverrides::default(),
        );
        let listing = f.github_assets().await.unwrap();
        let fetched = f
            .fetch(
                "x86_64-unknown-linux-gnu",
                tmp.path().join("t"),
                Some(&listing),
            )
            .await
            .unwrap();
        assert_eq!(fs::read(fetched.dir.join("my-tool")).unwrap(), b"bin");
    }

    #[tokio::test]
    async fn github_listing_errors_when_no_release_matches() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/my-tool/releases"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        let f = fetcher(
            &server.uri(),
            "https://github.com/owner/my-tool",
            TargetOverrides::default(),
        );
        let err = f.github_assets().await.unwrap_err();
        assert!(err.to_string().contains("no release tagged"), "{err}");
    }

    #[tokio::test]
    async fn probing_uses_pkg_url_and_retries_with_bin_name() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path(
                "/owner/my-tool/releases/download/v1.2.3/cli-x86_64-unknown-linux-gnu.tar.gz",
            ))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/owner/my-tool/releases/download/v1.2.3/cli-x86_64-unknown-linux-gnu.tar.gz",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(tgz(&[("cli-x86_64-unknown-linux-gnu/cli", b"bin")])),
            )
            .mount(&server)
            .await;

        let tmp = TempDir::new().unwrap();
        let mut f = fetcher(
            "",
            &format!("{}/owner/my-tool", server.uri()),
            TargetOverrides {
                pkg_url: Some(
                    "{ repo }/releases/download/v{ version }/{ name }-{ target }.tar.gz".into(),
                ),
                ..Default::default()
            },
        );
        f.bins = vec!["cli".into()];
        let fetched = f
            .fetch("x86_64-unknown-linux-gnu", tmp.path().join("t"), None)
            .await
            .unwrap();
        assert_eq!(fetched.vars.name, "my-tool");
        assert_eq!(fetched.found, "cli");
        let path = bin_path(&fetched.dir, None, &fetched.vars, &fetched.found, "cli").unwrap();
        assert_eq!(path, fetched.dir.join("cli-x86_64-unknown-linux-gnu/cli"));
        assert!(path.is_file());
    }

    #[tokio::test]
    async fn bare_binary_asset_is_the_binary() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/dl/my-tool-x86_64-unknown-linux-gnu"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/dl/my-tool-x86_64-unknown-linux-gnu"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"bin".to_vec()))
            .mount(&server)
            .await;

        let tmp = TempDir::new().unwrap();
        let f = fetcher(
            "",
            &format!("{}/owner/my-tool", server.uri()),
            TargetOverrides {
                pkg_url: Some(format!("{}/dl/{{ name }}-{{ target }}", server.uri())),
                pkg_fmt: Some(PkgFmt::Bin),
                bin_dir: None,
            },
        );
        let fetched = f
            .fetch("x86_64-unknown-linux-gnu", tmp.path().join("t"), None)
            .await
            .unwrap();
        let bin = fetched.bin.unwrap();
        assert_eq!(fs::read(&bin).unwrap(), b"bin");
    }

    #[tokio::test]
    async fn token_is_sent_to_the_api_only() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cdn/asset"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/cdn/asset"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(tgz(&[("my-tool", b"bin")])))
            .mount(&server)
            .await;

        let tmp = TempDir::new().unwrap();
        let mut f = fetcher(
            &format!("{}/api", server.uri()),
            "https://github.com/owner/my-tool",
            TargetOverrides::default(),
        );
        f.token = Some("secret".into());
        // The API lives under /api; the asset URL does not, so it must go out unauthenticated.
        Mock::given(method("GET"))
            .and(path("/api/repos/owner/my-tool/releases"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "tag_name": "v1.2.3", "assets": [
                    { "name": "my-tool-x86_64-unknown-linux-gnu.tar.gz",
                      "url": format!("{}/cdn/asset", server.uri()) }
                ] }
            ])))
            .mount(&server)
            .await;
        let listing = f.github_assets().await.unwrap();
        let fetched = f
            .fetch(
                "x86_64-unknown-linux-gnu",
                tmp.path().join("t"),
                Some(&listing),
            )
            .await
            .unwrap();
        assert!(fetched.dir.join("my-tool").is_file());
    }

    #[test]
    fn has_var_matches_placeholders_only() {
        assert!(has_var("{ repo }/x{ archive-suffix }", &["archive-suffix"]));
        assert!(has_var("{format}", &["format"]));
        assert!(!has_var(
            "https://dl.example.com/formats/{ name }.tar.gz",
            &["format"]
        ));
    }

    #[tokio::test]
    async fn unknown_host_without_pkg_url_is_an_error() {
        let tmp = TempDir::new().unwrap();
        let f = fetcher(
            "",
            "https://example.com/owner/my-tool",
            TargetOverrides::default(),
        );
        let err = f
            .fetch("x86_64-unknown-linux-gnu", tmp.path().join("t"), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("set pkg-url"), "{err}");
    }

    #[test]
    fn guess_format_from_extension() {
        assert_eq!(guess_format("x/foo.tar.gz"), Some(PkgFmt::Tgz));
        assert_eq!(guess_format("x/foo.tgz"), Some(PkgFmt::Tgz));
        assert_eq!(guess_format("x/foo.tar.xz"), Some(PkgFmt::Txz));
        assert_eq!(guess_format("x/foo.zip"), Some(PkgFmt::Zip));
        assert_eq!(guess_format("x/foo.exe"), Some(PkgFmt::Bin));
        assert_eq!(guess_format("x/foo.gz"), None);
        assert_eq!(guess_format("x/foo"), None);
    }

    #[test]
    fn candidates_follow_host_layout_and_formats() {
        let f = fetcher(
            "",
            "https://gitlab.com/owner/my-tool",
            TargetOverrides::default(),
        );
        let vars = Vars {
            name: "my-tool".into(),
            repo: f.repo.url.clone(),
            subcrate: None,
            version: "1.2.3".into(),
            target: "x86_64-unknown-linux-gnu".into(),
        };
        let meta = TargetOverrides {
            pkg_fmt: Some(PkgFmt::Tgz),
            ..Default::default()
        };
        let urls: Vec<String> = f
            .candidates(&meta, &vars)
            .unwrap()
            .into_iter()
            .map(|(u, _)| u)
            .collect();
        assert_eq!(
            urls[0],
            "https://gitlab.com/owner/my-tool/-/releases/1.2.3/downloads/binaries/my-tool-x86_64-unknown-linux-gnu-v1.2.3.tgz"
        );
        assert!(urls.iter().all(|u| !u.contains("subcrate")));
        assert_eq!(urls.len(), 3 * 2 * FILENAMES.len());
    }

    #[test]
    fn extract_handles_each_format() {
        let tmp = TempDir::new().unwrap();
        extract(
            PkgFmt::Tgz,
            &tgz(&[("a/x", b"1")]),
            &tmp.path().join("tgz"),
            "x",
        )
        .unwrap();
        assert_eq!(fs::read(tmp.path().join("tgz/a/x")).unwrap(), b"1");

        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file::<_, ()>("x", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut zip, b"2").unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        extract(PkgFmt::Zip, &bytes, &tmp.path().join("zip"), "x").unwrap();
        assert_eq!(fs::read(tmp.path().join("zip/x")).unwrap(), b"2");

        extract(PkgFmt::Bin, b"3", &tmp.path().join("bin"), "x.exe").unwrap();
        assert_eq!(fs::read(tmp.path().join("bin/x.exe")).unwrap(), b"3");
    }
}
