//! Where compiled binaries come from.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::config::Job;

mod release;
mod target_dir;

use release::Release;

/// Binaries a job's targets provide: triple -> bin name -> file.
pub type Binaries = BTreeMap<String, BTreeMap<String, PathBuf>>;

/// A source of compiled binaries for the platform packages.
pub enum Source {
    TargetDir(PathBuf),
    Release(Release),
}

impl Source {
    /// Binaries built locally under cargo's target directory.
    pub fn target_dir(dir: PathBuf) -> Source {
        Source::TargetDir(dir)
    }

    /// Binaries downloaded from each job's release; downloads start immediately, into
    /// `{output_dir}/.tmp`.
    pub fn release(output_dir: &Path, jobs: &[Job]) -> Result<Source> {
        Ok(Source::Release(Release::new(output_dir, jobs)?))
    }

    /// Binaries for `job`: those of its configured targets, or with `infer` of every target
    /// the source has.
    pub async fn locate(&mut self, job: &Job, infer: bool) -> Result<Binaries> {
        match self {
            Self::TargetDir(dir) => target_dir::locate(dir, job, infer),
            Self::Release(_) if infer => bail!(
                "--infer-targets is not supported with --from-release yet - \
                 add [package.metadata.npm] targets to Cargo.toml or pass --target"
            ),
            Self::Release(release) => release.locate(job).await,
        }
    }

    /// Whether located files are scratch that may be moved rather than copied.
    pub fn disposable(&self) -> bool {
        matches!(self, Self::Release(_))
    }
}
