use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "cargo-npm", bin_name = "cargo")]
pub enum Cargo {
    Npm(Npm),
}

/// Publish Rust binaries as npm packages
#[derive(Parser)]
pub struct Npm {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Generate npm packages from compiled Rust binaries
    Generate(GenerateArgs),
    /// Publish npm packages to the registry
    Publish(PublishArgs),
}

#[derive(Args)]
pub struct CommonArgs {
    /// Path to Cargo.toml
    #[arg(long, value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,
    /// Package to process; supports glob patterns (may be specified multiple times)
    #[arg(short, long, value_name = "SPEC")]
    pub package: Vec<String>,
    /// Process all packages in the workspace
    #[arg(long)]
    pub workspace: bool,
    /// Exclude packages from processing; supports glob patterns; requires --workspace (may be specified multiple times)
    #[arg(long, value_name = "SPEC")]
    pub exclude: Vec<String>,
    /// Output directory for generated packages [default: npm]
    #[arg(long)]
    pub out_dir: Option<String>,
}

#[derive(Args)]
pub struct GenerateArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Target triple (may be specified multiple times)
    #[arg(long, value_name = "TRIPLE")]
    pub target: Vec<String>,
    /// Directory for compiled artifacts [default: cargo's target directory]
    #[arg(long, value_name = "DIR")]
    pub target_dir: Option<PathBuf>,
    /// Remove the output directory before generating
    #[arg(long)]
    pub clean: bool,
    /// Infer targets from built binaries instead of requiring explicit configuration
    #[arg(long, conflicts_with = "target")]
    pub infer_targets: bool,
    /// Generate only the main package without platform packages or optionalDependencies
    #[arg(long, conflicts_with_all = ["target", "infer_targets"])]
    pub stub: bool,
}

#[derive(Args)]
pub struct PublishArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Arguments passed through to `npm publish`
    #[arg(last = true)]
    pub npm_args: Vec<String>,
}
