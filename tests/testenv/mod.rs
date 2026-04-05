use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use tempfile::TempDir;
use toml::{Table, Value};

const CARGO_NPM: &str = env!("CARGO_BIN_EXE_cargo-npm");

pub struct TestEnv {
    dir: TempDir,
    exe: PathBuf,
    cwd: PathBuf,
}

impl TestEnv {
    /// Create an empty test environment with no files written.
    pub fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().to_path_buf();
        let exe = PathBuf::from(CARGO_NPM);
        Self { dir, exe, cwd }
    }

    /// Create a test environment for a single-binary package.
    pub fn package() -> Self {
        Self::package_with_config(Table::new())
    }

    /// Create a test environment for a package with the given manifest config merged on top
    /// of the defaults. Pass `[package]` entries to override package fields, `[[bin]]` entries
    /// to replace the default single binary, or `[package.metadata.npm]` for npm config.
    pub fn package_with_config(mut config: Table) -> Self {
        let env = Self::new();

        let mut manifest = gen_manifest("my-tool");

        if let Some(Value::Table(pkg)) = config.remove("package")
            && let Some(Value::Table(base_pkg)) = manifest.get_mut("package")
        {
            if pkg.contains_key("license-file") {
                base_pkg.remove("license");
            }
            base_pkg.extend(pkg);
        }
        manifest.extend(config);
        inject_default_targets(manifest.get_mut("package"));

        if let Some(Value::Array(bins)) = manifest.get("bin") {
            for bin in bins {
                if let Some(path) = bin
                    .as_table()
                    .and_then(|t| t.get("path"))
                    .and_then(|v| v.as_str())
                {
                    env.create_file(path, "fn main() {}");
                }
            }
        }

        env.create_file("Cargo.toml", manifest);
        env
    }

    /// Create a workspace test environment with multiple member packages, each with a
    /// single binary of the same name.
    pub fn workspace(members: &[&str]) -> Self {
        Self::workspace_with_config(members, Table::new())
    }

    /// Create a workspace with `[workspace.metadata.npm]` config.
    pub fn workspace_with_config(members: &[&str], mut config: Table) -> Self {
        let env = Self::new();

        let mut manifest = toml::toml! {
           [workspace]
           resolver = "3"
           members = ["crates/*"]
        };

        if let Some(Value::Table(pkg)) = config.remove("workspace")
            && let Some(Value::Table(base_pkg)) = manifest.get_mut("workspace")
        {
            base_pkg.extend(pkg);
        }

        inject_default_targets(manifest.get_mut("workspace"));
        env.create_file("Cargo.toml", manifest);

        for &member in members {
            env.create_file(format!("crates/{member}/Cargo.toml"), gen_manifest(member));
            env.create_file(format!("crates/{member}/src/main.rs"), "fn main() {}");
        }

        env
    }

    pub fn path(&self) -> &Path {
        &self.cwd
    }

    pub fn chdir(&mut self, path: &str) {
        self.cwd = self.cwd.join(path);
    }

    /// Create fake pre-built binaries for all bin targets in the workspace for each triple.
    pub fn create_binaries(&self, triples: &[&str]) {
        cargo_metadata::MetadataCommand::new()
            .no_deps()
            .current_dir(self.dir.path())
            .exec()
            .unwrap()
            .packages
            .iter()
            .flat_map(|p| &p.targets)
            .filter(|t| t.kind.contains(&cargo_metadata::TargetKind::Bin))
            .for_each(|bin| {
                for triple in triples {
                    self.create_binary(&bin.name, triple);
                }
            });
    }

    /// Create a fake pre-built binary. Pass `triple = ""` for `target/release/`.
    fn create_binary(&self, name: &str, triple: &str) {
        let is_windows = match triple {
            "" => cfg!(windows),
            _ => triple.contains("-windows-"),
        };
        let ext = if is_windows { ".exe" } else { "" };
        let rel = match triple {
            "" => format!("target/release/{name}{ext}"),
            _ => format!("target/{triple}/release/{name}{ext}"),
        };
        self.create_file(&rel, "fake binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = self.dir.path().join(&rel);
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Create a file at the given path relative to the project root, creating parent directories as needed.
    #[allow(clippy::needless_pass_by_value)]
    pub fn create_file<P: AsRef<Path>>(&self, name: P, content: impl ToString) {
        let path = self.cwd.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content.to_string().as_bytes()).unwrap();
    }

    pub fn remove_file<P: AsRef<Path>>(&self, name: P) {
        fs::remove_file(self.cwd.join(name)).unwrap();
    }

    fn run(&self, command: &str, args: &[&str]) -> Output {
        let bin_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/testenv/fake_npm");
        let path_sep = if cfg!(windows) { ";" } else { ":" };
        let path = format!(
            "{}{}{}",
            bin_dir.display(),
            path_sep,
            std::env::var("PATH").unwrap_or_default()
        );
        std::process::Command::new(&self.exe)
            .arg("npm")
            .arg(command)
            .args(args)
            .current_dir(self.cwd.clone())
            .env("CARGO_TARGET_DIR", self.dir.path().join("target"))
            .env("FAKE_NPM_LOG", self.npm_log_path())
            .env("PATH", path)
            .output()
            .unwrap()
    }

    pub fn assert_ok(&self, command: &str, args: &[&str]) {
        let out = self.run(command, args);
        assert!(
            out.status.success(),
            "cargo npm {command} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    pub fn assert_err(&self, command: &str, args: &[&str], expected: &str) {
        let out = self.run(command, args);
        assert!(
            !out.status.success(),
            "expected `cargo npm {command}` to fail but it succeeded\nstdout: {}",
            String::from_utf8_lossy(&out.stdout),
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(expected),
            "expected stderr to contain {expected:?}\ngot: {stderr}",
        );
    }

    pub fn read_json(&self, path: &str) -> serde_json::Value {
        serde_json::from_str(&self.read_file(path)).unwrap()
    }

    pub fn read_file(&self, path: &str) -> String {
        fs::read_to_string(self.cwd.join(path))
            .unwrap_or_else(|e| panic!("failed to read {path}: {e}",))
    }

    /// Asserts that a file or directory exists at the given path relative to the project root.
    pub fn assert_exists(&self, path: &str) {
        assert!(
            self.cwd.join(path).exists(),
            "Expected file or directory to exist: {path}",
        );
    }

    /// Asserts that a file or directory does NOT exist at the given path relative to the project root.
    pub fn assert_not_exists(&self, path: &str) {
        assert!(
            !self.cwd.join(path).exists(),
            "Did not expect file or directory to exist: {path}",
        );
    }

    /// Asserts that the set of package names in all package.json files under the given npm folder matches the expected set, ignoring order.
    pub fn assert_generated(&self, expected: &[&str]) {
        let pattern = self
            .dir
            .path()
            .join("npm/**/package.json")
            .to_string_lossy()
            .replace('\\', "/");
        let found: HashSet<String> = glob::glob(&pattern)
            .expect("invalid glob pattern")
            .map(|entry| {
                let p = entry.expect("glob error");
                let content = fs::read_to_string(&p).expect("failed to read package.json");
                let json: serde_json::Value =
                    serde_json::from_str(&content).expect("invalid package.json");
                json["name"]
                    .as_str()
                    .expect("missing name field")
                    .to_owned()
            })
            .collect();
        assert_eq!(
            found,
            expected
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<HashSet<_>>(),
            "Generated package names do not match.\nFound: {found:?}\nExpected: {expected:?}"
        );
    }

    /// Asserts that the set of published package names matches the expected set, ignoring order.
    pub fn assert_published(&self, expected: &[&str]) {
        let mut found = self.published_packages();
        let mut expected: Vec<String> = expected
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        found.sort();
        expected.sort();
        assert_eq!(
            found, expected,
            "Published package names do not match.\nFound: {found:?}\nExpected: {expected:?}"
        );
    }

    /// Returns the names of packages passed to `npm publish`.
    pub fn published_packages(&self) -> Vec<String> {
        fs::read_to_string(self.npm_log_path())
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.split('\t').next().map(str::to_owned))
            .collect()
    }

    fn npm_log_path(&self) -> PathBuf {
        self.dir.path().join("npm-publish.log")
    }
}

/// Inject a default target into `[<section>.metadata.npm]` if not already configured.
/// Pass `manifest.get_mut("package")` or `manifest.get_mut("workspace")`.
fn inject_default_targets(section: Option<&mut Value>) {
    section
        .and_then(|v| v.as_table_mut())
        .map(|t| t.entry("metadata").or_insert(Value::Table(Table::new())))
        .and_then(|v| v.as_table_mut())
        .map(|m| m.entry("npm").or_insert(Value::Table(Table::new())))
        .and_then(|v| v.as_table_mut())
        .map(|n| {
            n.entry("targets")
                .or_insert(Value::Array(vec!["x86_64-unknown-linux-gnu".into()]))
        });
}

fn gen_manifest(name: &str) -> Table {
    toml::toml! {
        [package]
        name = name
        version = "1.0.0"
        edition = "2021"
        description = "A test tool"
        repository = "https://github.com/example/my-tool"
        license = "MIT"

        [[bin]]
        name = name
        path = "src/main.rs"
    }
}
