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
    /// Creates a new TestEnv rooted at a fresh temporary directory with no project files.
    ///
    /// The returned environment is ready for test setup operations (writing files, creating
    /// manifests, running commands) and tracks its own temporary directory which is removed
    /// when the environment is dropped.
    ///
    /// # Examples
    ///
    /// ```
    /// let env = TestEnv::new();
    /// env.create_file("hello.txt", b"hello");
    /// assert_eq!(env.read_file("hello.txt"), "hello");
    /// ```
    pub fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().to_path_buf();
        let exe = PathBuf::from(CARGO_NPM);
        Self { dir, exe, cwd }
    }

    /// Create a test environment containing a single-package Cargo project with a binary target.
    
    ///
    
    /// The created environment is populated with a generated `Cargo.toml` (from `gen_manifest`)
    
    /// and any files needed for the package's binary target (e.g., `src/main.rs`).
    
    ///
    
    /// # Examples
    
    ///
    
    /// ```no_run
    
    /// // create an isolated temp project with a single binary package
    
    /// let _env = tests::testenv::TestEnv::package();
    
    /// ```
    pub fn package() -> Self {
        Self::package_with_config(Table::new())
    }

    /// Creates a temporary Cargo package project, merging the provided TOML `config` on top of a default
    /// manifest and writing the resulting `Cargo.toml` into the test environment.
    ///
    /// The `config` table may include:
    /// - `[package]` to override package metadata (if `license-file` is provided, the default `license`
    ///   field is removed),
    /// - `[[bin]]` to replace or augment binary entries (any `path` entries will cause placeholder
    ///   `src` files to be created),
    /// - `[package.metadata.npm]` for npm-related metadata, or any other top-level manifest entries
    ///   to be merged into the final manifest.
    ///
    /// The function returns a `TestEnv` rooted at a fresh temporary directory containing the generated
    /// project and written `Cargo.toml`.
    ///
    /// # Examples
    ///
    /// ```
    /// use toml::value::{Table, Value};
    ///
    /// let mut cfg = Table::new();
    /// // Override package name and add an extra bin with a custom path.
    /// let mut pkg = Table::new();
    /// pkg.insert("name".to_string(), Value::String("my-overridden-tool".into()));
    /// cfg.insert("package".to_string(), Value::Table(pkg));
    ///
    /// let env = TestEnv::package_with_config(cfg);
    /// // `Cargo.toml` has been written into the environment's cwd.
    /// assert!(env.path().join("Cargo.toml").exists());
    /// ```
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

        if let Some(Value::Array(bins)) = manifest.get("bin") {
            for bin in bins {
                if let Some(path) = bin
                    .as_table()
                    .and_then(|t| t.get("path"))
                    .and_then(|v| v.as_str())
                {
                    env.create_file(path, b"fn main() {}");
                }
            }
        }

        env.create_toml("Cargo.toml", &manifest);
        env
    }

    /// Creates a workspace test environment containing the given member crates, each with a single binary named after the crate.
    ///
    /// The workspace manifest is written to `Cargo.toml` in a temporary directory, and for each `member` a `crates/<member>/Cargo.toml` and `crates/<member>/src/main.rs` are created.
    ///
    /// # Arguments
    ///
    /// * `members` - Slice of crate names to create as workspace members.
    ///
    /// # Returns
    ///
    /// A `TestEnv` rooted at a temporary directory with the workspace and member crates created.
    ///
    /// # Examples
    ///
    /// ```
    /// let env = TestEnv::workspace(&["foo", "bar"]);
    /// env.assert_exists("crates/foo/Cargo.toml");
    /// env.assert_exists("crates/bar/src/main.rs");
    /// ```
    pub fn workspace(members: &[&str]) -> Self {
        Self::workspace_with_config(members, Table::new())
    }

    /// Creates a new temporary Cargo workspace and member crates, merging any `[workspace]` table
    /// from `config` into the generated manifest.
    ///
    /// If `config` contains a `workspace` table, its entries extend or overwrite the default
    /// workspace table (resolver = "3", members = ["crates/*"]). The function writes the merged
    /// `Cargo.toml` at the environment root and creates a `crates/{member}/Cargo.toml` and
    /// `crates/{member}/src/main.rs` stub for each provided member name.
    ///
    /// # Examples
    ///
    /// ```
    /// use toml::value::Table;
    ///
    /// // Create a workspace with two members using the default workspace manifest.
    /// let env = TestEnv::workspace_with_config(&["foo", "bar"], Table::new());
    /// assert!(env.path().join("crates/foo/Cargo.toml").exists());
    /// assert!(env.path().join("crates/bar/src/main.rs").exists());
    /// ```
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

        env.create_toml("Cargo.toml", &manifest);

        for &member in members {
            env.create_toml(
                &format!("crates/{member}/Cargo.toml"),
                &gen_manifest(member),
            );
            env.create_file(&format!("crates/{member}/src/main.rs"), b"fn main() {}");
        }

        env
    }

    /// Returns the current working directory path inside this temporary test environment.
    ///
    /// # Examples
    ///
    /// ```
    /// let env = TestEnv::new();
    /// let cwd = env.path();
    /// // `cwd` is the environment's current working directory (a `&Path`)
    /// ```
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.cwd
    }

    /// Updates the environment's current working directory by joining the provided relative `path` onto the existing working directory.
    ///
    /// # Examples
    ///
    /// ```
    /// let mut env = TestEnv::new();
    /// env.chdir("crates/my-crate");
    /// // The environment's working directory is now the previous cwd joined with "crates/my-crate".
    /// ```
    pub fn chdir(&mut self, path: &str) {
        self.cwd = self.cwd.join(path);
    }

    /// Create fake built binaries for every binary target in the temporary project for the given target triples.
    ///
    /// For each package binary target discovered in the workspace, this writes a placeholder file at
    /// `target/<triple>/release/<name>` for each provided `triple`. If a `triple` is the empty string,
    /// the file is written to `target/release/<name>`. On Unix platforms the created file is made executable.
    ///
    /// # Parameters
    ///
    /// - `triples`: A slice of target triple strings. Use `""` to write the binary into the default `target/release` directory.
    ///
    /// # Examples
    ///
    /// ```
    /// let env = TestEnv::package();
    /// // Create default release binaries and a specific triple
    /// env.create_binaries(&["", "x86_64-unknown-linux-gnu"]);
    /// env.assert_exists("target/release/my-tool");
    /// env.assert_exists("target/x86_64-unknown-linux-gnu/release/my-tool");
    /// ```
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

    /// Creates a fake pre-built binary file inside the test environment's target directory.
    ///
    /// The created file contains the bytes `"fake binary"`. On Unix platforms the file's
    /// permissions are set to be executable (0o755).
    ///
    /// # Arguments
    ///
    /// * `name` — the filename of the binary (not a path).
    /// * `triple` — target triple segment to place the binary under; pass an empty string
    ///   to place the file at `target/release/{name}`, otherwise the file is placed at
    ///   `target/{triple}/release/{name}`.
    ///
    /// # Examples
    ///
    /// ```
    /// let env = TestEnv::new();
    /// env.create_binary("my-tool", "");
    /// // The file contents are readable as text in tests:
    /// let contents = env.read_file("target/release/my-tool");
    /// assert!(contents.contains("fake binary"));
    /// ```
    fn create_binary(&self, name: &str, triple: &str) {
        let rel = match triple {
            "" => format!("target/release/{name}"),
            _ => format!("target/{triple}/release/{name}"),
        };
        self.create_file(&rel, b"fake binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = self.dir.path().join(&rel);
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Creates a file at the given path inside the test environment's current working directory,
    /// creating any missing parent directories.
    ///
    /// # Examples
    ///
    /// ```
    /// let env = TestEnv::new();
    /// env.create_file("src/main.rs", b"fn main() {}");
    /// let content = env.read_file("src/main.rs");
    /// assert!(content.contains("fn main"));
    /// ```
    pub fn create_file(&self, name: &str, content: &[u8]) {
        let path = self.cwd.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    /// Writes a TOML `Table` to a file at `name` relative to the environment's project root.
    ///
    /// # Examples
    ///
    /// ```
    /// use toml::value::Table;
    /// let env = TestEnv::new();
    /// let mut table = Table::new();
    /// let mut pkg = Table::new();
    /// pkg.insert("name".into(), toml::Value::String("my-tool".into()));
    /// table.insert("package".into(), toml::Value::Table(pkg));
    /// env.create_toml("Cargo.toml", &table);
    /// assert!(env.path().join("Cargo.toml").exists());
    /// ```
    pub fn create_toml(&self, name: &str, content: &Table) {
        self.create_file(name, content.to_string().as_bytes());
    }

    /// Removes the file at the given path relative to the environment's current working directory.
    ///
    /// # Panics
    ///
    /// Panics if the file cannot be removed.
    ///
    /// # Examples
    ///
    /// ```
    /// let env = TestEnv::new();
    /// env.create_file("a.txt", b"contents");
    /// env.remove_file("a.txt");
    /// env.assert_not_exists("a.txt");
    /// ```
    pub fn remove_file(&self, name: &str) {
        fs::remove_file(self.cwd.join(name)).unwrap();
    }

    /// Execute the `cargo-npm npm <command>` invocation inside this test environment and return its captured output.
    
    ///
    
    /// The child process is run with the environment's current working directory, `CARGO_TARGET_DIR` set to the environment's temporary `target` directory, `FAKE_NPM_LOG` set to the environment's npm log path, and the test `fake_npm` directory prepended to `PATH`.
    
    ///
    
    /// # Examples
    
    ///
    
    /// ```
    
    /// let env = TestEnv::new();
    
    /// let output = env.run("publish", &["--dry-run"]);
    
    /// // inspect status, stdout, stderr
    
    /// assert!(output.status.code().is_some());
    
    /// ```
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

    /// Runs `cargo-npm npm <command> ...` in the test environment and asserts the process exits successfully.
    ///
    /// Panics if the invoked process exits with a non-success status; the panic message includes the captured
    /// stdout and stderr for debugging.
    ///
    /// # Parameters
    ///
    /// - `command`: The npm subcommand to run (e.g., `"publish"`, `"pack"`).
    /// - `args`: Additional arguments passed to the command.
    ///
    /// # Examples
    ///
    /// ```
    /// let env = TestEnv::package();
    /// env.assert_ok("pack", &[]);
    /// ```
    pub fn assert_ok(&self, command: &str, args: &[&str]) {
        let out = self.run(command, args);
        assert!(
            out.status.success(),
            "cargo npm {command} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    /// Asserts that invoking `cargo npm <command>` in the test environment fails and that stderr contains `expected`.
    ///
    /// # Parameters
    ///
    /// - `command`: the `cargo npm` subcommand to run (e.g., `"publish"`).
    /// - `args`: arguments to pass to the subcommand.
    /// - `expected`: substring that must appear in the process's stderr output.
    ///
    /// # Examples
    ///
    /// ```
    /// let env = TestEnv::package();
    /// env.assert_err("publish", &["--dry-run"], "authentication required");
    /// ```
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

    /// Reads a file at the given path (relative to the environment's current working directory) and parses it as JSON.
    ///
    /// # Examples
    ///
    /// ```
    /// let env = TestEnv::package();
    /// env.create_file("data.json", br#"{"key": "value"}"#);
    /// let json = env.read_json("data.json");
    /// assert_eq!(json["key"], "value");
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if the file cannot be read or if its contents are not valid JSON.
    pub fn read_json(&self, path: &str) -> serde_json::Value {
        serde_json::from_str(&self.read_file(path)).unwrap()
    }

    /// Reads a UTF-8 file relative to the environment's current working directory and returns its contents.
    ///
    /// # Parameters
    ///
    /// - `path`: A path relative to the environment's current working directory.
    ///
    /// # Returns
    ///
    /// The file contents as a `String`.
    ///
    /// # Panics
    ///
    /// Panics if the file cannot be read or is not valid UTF-8. The panic message is `failed to read {path}: {e}`.
    ///
    /// # Examples
    ///
    /// ```
    /// let env = TestEnv::new();
    /// env.create_file("notes.txt", b"hello");
    /// let contents = env.read_file("notes.txt");
    /// assert_eq!(contents, "hello");
    /// ```
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

    /// Verify that no file or directory exists at the given path relative to the project root.
    ///
    /// Panics if the path exists; the panic message includes the offending path.
    ///
    /// # Examples
    ///
    /// ```
    /// let env = TestEnv::new();
    /// env.assert_not_exists("some/nonexistent/path.txt");
    /// ```
    pub fn assert_not_exists(&self, path: &str) {
        assert!(
            !self.cwd.join(path).exists(),
            "Did not expect file or directory to exist: {path}",
        );
    }

    /// Check that the package names declared in every `package.json` under the environment's `npm` directory
    /// match the provided list of expected names, ignoring order.
    ///
    /// This will read every `npm/**/package.json`, extract each `name` field, and assert the set of names
    /// equals the `expected` set.
    ///
    /// # Examples
    ///
    /// ```
    /// let env = TestEnv::package();
    /// // after running the code that generates npm packages...
    /// env.assert_generated(&["my-tool"]);
    /// ```
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

    /// Lists package names recorded in the npm publish log.
    ///
    /// Reads the environment's npm publish log and returns the first tab-delimited
    /// token from each line as a `Vec<String>`.
    ///
    /// # Examples
    ///
    /// ```
    /// let mut env = TestEnv::new();
    /// env.create_file("npm-publish.log", b"pkg-a\t1\npkg-b\t2\n");
    /// let pkgs = env.published_packages();
    /// assert_eq!(pkgs, vec!["pkg-a".to_string(), "pkg-b".to_string()]);
    /// ```
    pub fn published_packages(&self) -> Vec<String> {
        fs::read_to_string(self.npm_log_path())
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.split('\t').next().map(str::to_owned))
            .collect()
    }

    /// Path to the npm publish log file inside the test environment.
    ///
    /// The file is located at `<tempdir>/npm-publish.log` where `<tempdir>` is the
    /// temporary directory backing this `TestEnv`.
    ///
    /// # Examples
    ///
    /// ```
    /// // Construct a test environment and inspect the npm log path.
    /// let env = tests::testenv::TestEnv::new();
    /// let path = env.npm_log_path();
    /// assert!(path.ends_with("npm-publish.log"));
    /// ```
    fn npm_log_path(&self) -> PathBuf {
        self.dir.path().join("npm-publish.log")
    }
}

/// Create a default Cargo.toml manifest Table for a binary crate with the given package name.
///
/// The manifest contains a `[package]` table with sensible defaults (version "1.0.0", edition "2021",
/// description, repository, and `license = "MIT"`) and a single `[[bin]]` entry with `path = "src/main.rs"`.
///
/// # Examples
///
/// ```
/// let tbl = gen_manifest("my-tool");
/// let pkg = tbl.get("package").and_then(|v| v.as_table()).expect("package table");
/// assert_eq!(pkg.get("name").and_then(|v| v.as_str()), Some("my-tool"));
/// let bins = tbl.get("bin").and_then(|v| v.as_array()).expect("bin array");
/// assert!(bins.iter().any(|b| b.as_table().and_then(|t| t.get("path")).and_then(|p| p.as_str()) == Some("src/main.rs")));
/// ```
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
