use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::Parser;
use tokio::task::JoinSet;

mod artifacts;
mod cli;
mod config;
mod git;
mod git_url;
mod npm;
mod platform;
mod publish;
mod template;

/// Program entrypoint for the CLI that parses arguments and dispatches commands.
///
/// Parses command-line arguments into the Cargo/Npm subcommand and runs either the
/// generate or publish command. On error, prints a formatted message to stderr and
/// exits the process with status code 1.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli::Cargo::Npm(npm) = cli::Cargo::parse();
    let result = match npm.command {
        cli::Command::Generate(args) => cmd_generate(&args),
        cli::Command::Publish(args) => cmd_publish(&args).await,
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        process::exit(1);
    }
}

/// Generate npm packages for each binary crate described by the build configuration.
///
/// Loads Cargo metadata/configuration, optionally cleans and recreates the output directory,
/// produces platform-specific packages and a main package for each job, copies built binaries
/// into their platform package directories, and updates Git exclude entries for generated files.
///
/// Returns `Ok(())` on success, or an error if configuration is invalid or package generation fails.
fn cmd_generate(args: &cli::GenerateArgs) -> Result<()> {
    let build = config::load(config::LoadOpts {
        manifest_path: args.common.manifest_path.clone(),
        package: args.common.package.clone(),
        workspace: args.common.workspace,
        exclude: args.common.exclude.clone(),
        cli_targets: args.target.clone(),
        use_cargo_config: true,
        target_dir: args.target_dir.clone(),
        out_dir: args.common.out_dir.clone(),
    })?;

    if build.jobs.is_empty() {
        bail!("no binary crates found");
    }

    if args.clean && build.output_dir.exists() {
        check_safe_to_clean(&build.output_dir, &build.jobs)?;
        fs::remove_dir_all(&build.output_dir).with_context(|| {
            format!(
                "failed to clean output directory {}",
                build.output_dir.display()
            )
        })?;
    }
    fs::create_dir_all(&build.output_dir)?;

    let mut exclude_entries: Vec<PathBuf> = Vec::new();

    for job in build.jobs {
        if args.stub {
            npm::generate_main_package(&build.output_dir, &job, &[])?;
            continue;
        }

        let targets = if args.infer_targets {
            artifacts::infer_targets(&job.bins, &build.target_dir)?
        } else if job.targets.is_empty() {
            bail!(
                "no targets configured - add [package.metadata.npm] targets to Cargo.toml, \
                 pass --target, or use --infer-targets to discover built binaries"
            );
        } else {
            job.targets.clone()
        };

        let mut platforms: Vec<platform::Platform> = Vec::new();
        let mut unrecognised: Vec<&str> = Vec::new();
        for t in &targets {
            match platform::parse_triple(t) {
                Some(p) => platforms.push(p),
                None if job.targets_explicit => bail!(
                    "unrecognised target triple '{t}' - \
                     cargo-npm does not know how to map it to an npm platform"
                ),
                None => unrecognised.push(t),
            }
        }
        if !unrecognised.is_empty() {
            eprintln!(
                "warning: skipping unrecognised target triple(s) {} - \
                 cargo-npm does not know how to map them to npm platforms",
                unrecognised.join(", ")
            );
        }
        if platforms.is_empty() {
            bail!("none of the configured targets can be mapped to supported npm platforms");
        }

        platform::check_collisions(&platforms)?;
        platform::normalise_libc(&mut platforms);

        let mut platform_pkgs = Vec::new();
        for platform in &platforms {
            let pkg_name = npm::platform_package_name(&job.prefix, platform);

            for bin_name in &job.bins {
                let bin_file = if platform.os == platform::Os::Win32 {
                    format!("{bin_name}.exe")
                } else {
                    bin_name.clone()
                };
                exclude_entries.push(build.output_dir.join(&pkg_name).join(bin_file));
            }

            let pkg_dir = build.output_dir.join(&pkg_name);
            platform_pkgs.push(npm::generate_platform_package(
                &build.output_dir,
                platform,
                &pkg_name,
                &job,
            )?);
            artifacts::copy_bins(&job.bins, &build.target_dir, platform, &pkg_dir)?;
        }

        npm::generate_main_package(&build.output_dir, &job, &platform_pkgs)?;
    }

    git::update_git_exclude(&build.output_dir, &exclude_entries)?;

    println!("Generated npm packages in {}", build.output_dir.display());

    Ok(())
}

/// Publishes prepared npm packages for every binary crate in the build.
///
/// Checks that the `npm` tool is available, loads the project build configuration
/// (using the CLI publish options), prepares publishable packages for each job,
/// and publishes them concurrently. Returns an error if npm is not found, loading
/// or package preparation fails, or any publish task returns an error.
///
/// # Parameters
///
/// - `args`: CLI publish arguments (contains common build options and extra npm arguments).
///
/// # Errors
///
/// Returns an `Err` if npm is unavailable, configuration loading fails, no binary
/// crates are found, package preparation fails, or any concurrent publish task fails.
async fn cmd_publish(args: &cli::PublishArgs) -> Result<()> {
    publish::which_npm()?;

    let build = config::load(config::LoadOpts {
        manifest_path: args.common.manifest_path.clone(),
        package: args.common.package.clone(),
        workspace: args.common.workspace,
        exclude: args.common.exclude.clone(),
        cli_targets: Vec::new(),
        use_cargo_config: false,
        target_dir: None,
        out_dir: args.common.out_dir.clone(),
    })?;

    if build.jobs.is_empty() {
        bail!("no binary crates found");
    }

    let packages: Vec<publish::Package> = build
        .jobs
        .iter()
        .map(|job| publish::prepare(&build.output_dir, job))
        .collect::<Result<_>>()?;

    let extra_args = Arc::new(args.npm_args.clone());
    let mut job_set = JoinSet::new();
    for package in packages {
        let extra_args = Arc::clone(&extra_args);
        job_set.spawn(async move { publish::publish(package, extra_args).await });
    }
    while let Some(res) = job_set.join_next().await {
        res??;
    }

    Ok(())
}

/// Prevents deleting the output directory when it contains the current working directory or any package directory.
///
/// This function resolves the current working directory and returns an error if
/// the working directory is located inside `output_dir` or if any job's
/// `crate_dir` is located inside `output_dir`.
fn check_safe_to_clean(output_dir: &Path, jobs: &[config::Job]) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to resolve current directory")?;
    if cwd.starts_with(output_dir) {
        bail!(
            "refusing to clean output directory {} as it contains the current working directory",
            output_dir.display()
        );
    }
    for job in jobs {
        if job.crate_dir.starts_with(output_dir) {
            bail!(
                "refusing to clean output directory {} as it contains package directory {}",
                output_dir.display(),
                job.crate_dir.display()
            );
        }
    }
    Ok(())
}