mod testenv;
use testenv::TestEnv;

// Platform package structure

/// Ensures the CLI generates a main package plus distinct platform packages for multiple target triples.
///
/// # Examples
///
/// ```
/// let env = TestEnv::package();
/// env.create_binaries(&[
///     "x86_64-unknown-linux-gnu",
///     "aarch64-apple-darwin",
///     "x86_64-pc-windows-msvc",
/// ]);
/// env.assert_ok("generate", &["--infer-targets"]);
/// env.assert_generated(&[
///     "my-tool",
///     "my-tool-darwin-arm64",
///     "my-tool-linux-x64",
///     "my-tool-win32-x64",
/// ]);
/// ```
#[test]
fn multiple_platforms_multiple_packages() {
    let env = TestEnv::package();
    env.create_binaries(&[
        "x86_64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ]);
    env.assert_ok("generate", &["--infer-targets"]);
    env.assert_generated(&[
        "my-tool",
        "my-tool-darwin-arm64",
        "my-tool-linux-x64",
        "my-tool-win32-x64",
    ]);
}

/// Ensures running `generate` from a package subdirectory selects that package and produces the main and corresponding platform packages.
///
/// # Examples
///
/// ```
/// let mut env = TestEnv::package();
/// env.create_binaries(&["x86_64-unknown-linux-gnu"]);
/// env.chdir("src");
/// env.assert_ok("generate", &["--infer-targets"]);
/// env.assert_generated(&["my-tool", "my-tool-linux-x64"]);
/// ```
#[test]
fn package_selected_when_run_from_subdirectory() {
    let mut env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.chdir("src");
    env.assert_ok("generate", &["--infer-targets"]);
    env.assert_generated(&["my-tool", "my-tool-linux-x64"]);
}

#[test]
fn host_platform_detected_from_release_dir() {
    let env = TestEnv::package();
    env.create_binaries(&[""]); // empty triple means host e.g. target/release/my-tool
    env.assert_ok("generate", &["--infer-targets"]);

    // Should produce at least one platform package and one main package
    env.assert_exists("npm/my-tool/package.json");
}

#[test]
fn no_binaries_found_is_error() {
    let env = TestEnv::package();
    env.create_binaries(&["not-a-real-triple"]);
    env.assert_err("generate", &["--infer-targets"], "no binaries found");
}

/// Verifies that `generate` fails if the package has binaries but no npm targets are configured.
///
/// # Examples
///
/// ```
/// let env = TestEnv::package();
/// env.create_binaries(&["x86_64-unknown-linux-gnu"]);
/// env.assert_err("generate", &[], "no targets configured");
/// ```
#[test]
fn no_targets_configured_is_error() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_err("generate", &[], "no targets configured");
}

/// Asserts that invoking `generate` with an explicit, unrecognized target triple produces a clear error.
///
/// # Examples
///
/// ```
/// // Fails if the CLI is invoked with a target triple that cannot be mapped to an npm platform.
/// let env = TestEnv::package();
/// env.create_binaries(&["not-a-real-triple"]);
/// env.assert_err(
///     "generate",
///     &["--target", "not-a-real-triple"],
///     "unrecognised target triple 'not-a-real-triple'",
/// );
/// ```
#[test]
fn explicit_unrecognised_target_is_error() {
    let env = TestEnv::package();
    env.create_binaries(&["not-a-real-triple"]);
    env.assert_err(
        "generate",
        &["--target", "not-a-real-triple"],
        "unrecognised target triple 'not-a-real-triple'",
    );
}

#[test]
fn cargo_config_unrecognised_target_is_skipped() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu", "not-a-real-triple"]);
    env.create_toml(
        ".cargo/config.toml",
        &toml::toml! {
            [build]
            target = ["x86_64-unknown-linux-gnu", "not-a-real-triple"]
        },
    );
    env.assert_ok("generate", &[]);
    env.assert_generated(&["my-tool", "my-tool-linux-x64"]);
}

#[test]
fn cargo_config_all_unrecognised_targets_is_error() {
    let env = TestEnv::package();
    env.create_toml(
        ".cargo/config.toml",
        &toml::toml! {
            [build]
            target = ["not-a-real-triple"]
        },
    );
    env.assert_err(
        "generate",
        &[],
        "none of the configured targets can be mapped to supported npm platforms",
    );
}

// Main package

/// Verifies that generating packages produces correct `package.json` fields for the main package and a Linux platform package.
///
/// Asserts the platform package contains name, version, license, repository, and platform-specific fields (`os`, `cpu`, `libc`), that the platform binary file exists, and that the main package contains expected `name`, `version`, `description`, `license`, `repository`, `bin`, `engines`, and `optionalDependencies` wiring to the platform package.
///
/// # Examples
///
/// ```
/// let env = TestEnv::package();
/// env.create_binaries(&["x86_64-unknown-linux-gnu"]);
/// env.assert_ok("generate", &["--infer-targets"]);
///
/// let platform = env.read_json("npm/my-tool-linux-x64/package.json");
/// assert_eq!(platform["name"], "my-tool-linux-x64");
///
/// let main = env.read_json("npm/my-tool/package.json");
/// assert_eq!(main["name"], "my-tool");
/// ```
#[test]
fn package_json_fields() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    assert_eq!(
        env.read_json("npm/my-tool-linux-x64/package.json"),
        serde_json::json!({
            "name": "my-tool-linux-x64",
            "version": "1.0.0",
            "license": "MIT",
            "repository": { "type": "git", "url": "git+https://github.com/example/my-tool.git" },
            "os": ["linux"],
            "cpu": ["x64"],
            "libc": ["glibc"]
        })
    );
    env.assert_exists("npm/my-tool-linux-x64/my-tool");

    assert_eq!(
        env.read_json("npm/my-tool/package.json"),
        serde_json::json!({
            "name": "my-tool",
            "version": "1.0.0",
            "description": "A test tool",
            "license": "MIT",
            "repository": { "type": "git", "url": "git+https://github.com/example/my-tool.git" },
            "bin": { "my-tool": "bin/my-tool.js" },
            "engines": { "node": ">=14" },
            "optionalDependencies": { "my-tool-linux-x64": "1.0.0" }
        })
    );
}

// File copying

#[test]
fn files_copied_to_correct_packages() {
    let env = TestEnv::package();
    env.create_file("README.md", b"# My Tool");
    env.create_file("LICENSE", b"MIT license");
    env.create_file("LICENSE-APACHE", b"Apache license");
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    env.assert_exists("npm/my-tool/README.md");
    env.assert_not_exists("npm/my-tool-linux-x64/README.md");
    env.assert_exists("npm/my-tool/LICENSE");
    env.assert_exists("npm/my-tool/LICENSE-APACHE");
    env.assert_exists("npm/my-tool-linux-x64/LICENSE");
    env.assert_exists("npm/my-tool-linux-x64/LICENSE-APACHE");
}

/// Ensures a configured `license-file` is copied into both the main and platform packages and that the `license` field is set to `SEE LICENSE IN <file>`.
///
/// This test verifies that specifying `license-file = "COPYING"` in the package manifest causes the named file to be copied into generated npm package directories (main and platform) and that automatic license scanning (e.g., copying a default `LICENSE`) is suppressed.
///
/// # Examples
///
/// ```
/// let env = TestEnv::package_with_config(toml::toml! {
///     [package]
///     license-file = "COPYING"
/// });
/// env.create_file("COPYING", b"MIT license");
/// env.create_binaries(&["x86_64-unknown-linux-gnu"]);
/// env.run_and_assert_ok("generate", &["--infer-targets"]);
/// assert!(env.path_exists("npm/my-tool/COPYING"));
/// let main = env.read_json("npm/my-tool/package.json");
/// assert_eq!(main["license"], "SEE LICENSE IN COPYING");
/// ```
#[test]
fn license_file_field_copies_named_file_and_sets_see_license_in() {
    let env = TestEnv::package_with_config(toml::toml! {
        [package]
        license-file = "COPYING"
    });
    env.create_file("COPYING", b"MIT license");
    env.create_file("LICENSE", b"should not be copied");
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    env.assert_exists("npm/my-tool/COPYING");
    env.assert_exists("npm/my-tool-linux-x64/COPYING");
    env.assert_not_exists("npm/my-tool/LICENSE"); // license-file suppresses auto-scan
    let main = env.read_json("npm/my-tool/package.json");
    assert_eq!(main["license"], "SEE LICENSE IN COPYING");
    let platform = env.read_json("npm/my-tool-linux-x64/package.json");
    assert_eq!(platform["license"], "SEE LICENSE IN COPYING");
}

#[test]
fn license_file_with_license_field_uses_license_as_is() {
    let env = TestEnv::package_with_config(toml::toml! {
        [package]
        license = "MIT"
        license-file = "COPYING"
    });
    env.create_file("COPYING", b"MIT license");
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    env.assert_exists("npm/my-tool/COPYING");
    env.assert_exists("npm/my-tool-linux-x64/COPYING");
    let main = env.read_json("npm/my-tool/package.json");
    assert_eq!(main["license"], "MIT");
}

// Shim generation

#[test]
fn shim_created_for_each_binary() {
    let env = TestEnv::package_with_config(toml::toml! {
        [[bin]]
        name = "tool-a"
        path = "src/bin/tool-a.rs"

        [[bin]]
        name = "tool-b"
        path = "src/bin/tool-b.rs"
    });
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    env.assert_exists("npm/my-tool/bin/tool-a.js");
    env.assert_exists("npm/my-tool/bin/tool-b.js");
}

#[test]
fn shim_content() {
    let env = TestEnv::package();
    env.create_binaries(&[
        "x86_64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ]);
    env.assert_ok("generate", &["--infer-targets"]);

    let shim = env.read_file("npm/my-tool/bin/my-tool.js");
    assert!(!shim.contains("isMusl")); // no musl detection
    assert!(shim.contains("my-tool-linux-x64/my-tool"));
    assert!(shim.contains("my-tool-darwin-arm64/my-tool"));
    assert!(shim.contains("my-tool-win32-x64/my-tool.exe"));
}

// libc suffix

#[test]
fn single_linux_variant_has_no_libc_suffix_or_musl_detection() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-musl"]);
    env.assert_ok("generate", &["--infer-targets"]);
    env.assert_generated(&["my-tool", "my-tool-linux-x64"]);

    let shim = env.read_file("npm/my-tool/bin/my-tool.js");
    assert!(!shim.contains("isMusl")); // no musl detection
    assert!(shim.contains("my-tool-linux-x64/my-tool"));

    let pkg = env.read_json("npm/my-tool-linux-x64/package.json");
    assert!(pkg["libc"].is_null());
}

/// Verifies that when both glibc and musl Linux targets are present, the generator produces
/// a separate musl-suffixed platform package and emits musl-detection logic in the JS shim.
///
/// Asserts that both `my-tool-linux-x64` and `my-tool-linux-x64-musl` packages are generated
/// and that the shim references both platform binaries and contains `isMusl`.
///
/// # Examples
///
/// ```
/// let env = TestEnv::package();
/// env.create_binaries(&["x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl"]);
/// env.assert_ok("generate", &["--infer-targets"]);
/// env.assert_generated(&["my-tool", "my-tool-linux-x64", "my-tool-linux-x64-musl"]);
///
/// let shim = env.read_file("npm/my-tool/bin/my-tool.js");
/// assert!(shim.contains("isMusl"));
/// assert!(shim.contains("my-tool-linux-x64/my-tool"));
/// assert!(shim.contains("my-tool-linux-x64-musl/my-tool"));
/// ```
#[test]
fn dual_libc_variants_get_suffix_and_musl_detection() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl"]);
    env.assert_ok("generate", &["--infer-targets"]);
    env.assert_generated(&["my-tool", "my-tool-linux-x64", "my-tool-linux-x64-musl"]);

    let shim = env.read_file("npm/my-tool/bin/my-tool.js");
    assert!(shim.contains("isMusl")); // musl detection
    assert!(shim.contains("my-tool-linux-x64/my-tool"));
    assert!(shim.contains("my-tool-linux-x64-musl/my-tool"));
}

// Platform collision

#[test]
fn platform_collision_is_error() {
    let env = TestEnv::package();
    // Both windows triples map to win32-x64
    env.create_binaries(&["x86_64-pc-windows-msvc", "x86_64-pc-windows-gnu"]);
    env.assert_err("generate", &["--infer-targets"], "platform collision");
}

// Binary permissions (Unix only)

/// Verifies the generated platform binary is executable on Unix.
///
/// Checks that the platform package's binary file has the execute bits set for user, group, and others.
///
/// # Examples
///
/// ```
/// // Runs on Unix
/// let env = TestEnv::package();
/// env.create_binaries(&["x86_64-unknown-linux-gnu"]);
/// env.assert_ok("generate", &["--infer-targets"]);
/// let meta = std::fs::metadata(env.path().join("npm/my-tool-linux-x64/my-tool")).unwrap();
/// assert_eq!(meta.permissions().mode() & 0o111, 0o111);
/// ```
#[test]
#[cfg(unix)]
fn binary_is_executable() {
    use std::os::unix::fs::PermissionsExt;

    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    let meta = std::fs::metadata(env.path().join("npm/my-tool-linux-x64/my-tool")).unwrap();
    assert_eq!(meta.permissions().mode() & 0o111, 0o111);
}

// CLI flags

#[test]
fn output_flag_overrides_default_dir() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets", "--out-dir", "dist"]);

    env.assert_exists("dist/my-tool-linux-x64/package.json");
    env.assert_not_exists("npm/my-tool-linux-x64/package.json");
}

/// Verifies that running `generate` with `--clean` removes stale files from the previous output directory.
///
/// # Examples
///
/// ```
/// let env = TestEnv::package();
/// env.create_binaries(&["x86_64-unknown-linux-gnu"]);
/// env.assert_ok("generate", &["--infer-targets"]);
///
/// env.create_file("npm/stale.txt", b"old");
/// env.assert_exists("npm/stale.txt");
///
/// env.assert_ok("generate", &["--clean", "--infer-targets"]);
/// env.assert_not_exists("npm/stale.txt");
/// ```
#[test]
fn clean_flag_removes_previous_output() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    // Create a stale file in the output dir
    env.create_file("npm/stale.txt", b"old");
    env.assert_exists("npm/stale.txt");

    env.assert_ok("generate", &["--clean", "--infer-targets"]);
    env.assert_not_exists("npm/stale.txt");
}

#[test]
fn manifest_path_targets_specific_workspace_member() {
    let env = TestEnv::workspace(&["pkg-a", "pkg-b"]);
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok(
        "generate",
        &[
            "--infer-targets",
            "--manifest-path",
            "crates/pkg-a/Cargo.toml",
        ],
    );
    env.assert_generated(&["pkg-a", "pkg-a-linux-x64"]);
}

#[test]
fn manifest_path_overrides_cwd() {
    let mut env = TestEnv::workspace(&["pkg-a", "pkg-b"]);
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.chdir("crates/pkg-a");
    env.assert_ok(
        "generate",
        &["--infer-targets", "--manifest-path", "../pkg-b/Cargo.toml"],
    );
    env.assert_generated(&["pkg-b", "pkg-b-linux-x64"]);
}

#[test]
fn manifest_path_workspace_manifest_includes_all() {
    let mut env = TestEnv::workspace(&["pkg-a", "pkg-b"]);
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.chdir("crates/pkg-a");
    env.assert_ok(
        "generate",
        &["--infer-targets", "--manifest-path", "../../Cargo.toml"],
    );
    env.assert_generated(&["pkg-a", "pkg-a-linux-x64", "pkg-b", "pkg-b-linux-x64"]);
}

/// Verifies that a Cargo `[build] target` in `.cargo/config.toml` is respected when generating packages.
///
/// This test ensures the generator uses the configured build target to select platform packages to produce,
/// rather than inferring targets from all available binaries.
///
/// # Examples
///
/// ```
/// let env = TestEnv::package();
/// env.create_binaries(&["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]);
/// env.create_file(
///     ".cargo/config.toml",
///     b"[build]\ntarget = \"x86_64-unknown-linux-gnu\"\n",
/// );
/// env.assert_ok("generate", &[]);
/// env.assert_generated(&["my-tool", "my-tool-linux-x64"]);
/// ```
#[test]
fn cargo_config_build_target_is_respected() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]);
    env.create_file(
        ".cargo/config.toml",
        b"[build]\ntarget = \"x86_64-unknown-linux-gnu\"\n",
    );
    env.assert_ok("generate", &[]);

    env.assert_generated(&["my-tool", "my-tool-linux-x64"]);
}

#[test]
fn target_flag_filters_and_supports_multiple() {
    let env = TestEnv::package();
    env.create_binaries(&[
        "x86_64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ]);
    env.assert_ok("generate", &["--target", "x86_64-unknown-linux-gnu"]);
    env.assert_generated(&["my-tool", "my-tool-linux-x64"]);

    env.assert_ok(
        "generate",
        &[
            "--target",
            "x86_64-unknown-linux-gnu",
            "--target",
            "aarch64-apple-darwin",
        ],
    );
    env.assert_generated(&["my-tool", "my-tool-darwin-arm64", "my-tool-linux-x64"]);
}

/// Uses `package.metadata.npm.targets` to select which platform packages to generate when target
/// inference is not enabled.
///
/// # Examples
///
/// ```
/// let env = TestEnv::package_with_config(toml::toml! {
///     [package.metadata.npm]
///     targets = ["x86_64-unknown-linux-gnu"]
/// });
/// env.create_binaries(&["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]);
/// env.assert_ok("generate", &[]);
/// env.assert_generated(&["my-tool", "my-tool-linux-x64"]);
/// ```
#[test]
fn npm_config_targets_used_without_infer() {
    let env = TestEnv::package_with_config(toml::toml! {
        [package.metadata.npm]
        targets = ["x86_64-unknown-linux-gnu"]
    });
    env.create_binaries(&["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]);
    env.assert_ok("generate", &[]);
    env.assert_generated(&["my-tool", "my-tool-linux-x64"]);
}

/// Ensures workspace-level npm `targets` are applied to every workspace member during generation.
///
/// This test verifies that when `workspace.metadata.npm.targets` is set, running the
/// `generate` command with `--workspace` produces platform packages for each workspace
/// member according to those targets.
///
/// # Examples
///
/// ```
/// let env = TestEnv::workspace_with_config(
///     &["pkg-a", "pkg-b"],
///     toml::toml! {
///         [workspace.metadata.npm]
///         targets = ["x86_64-unknown-linux-gnu"]
///     },
/// );
/// env.create_binaries(&["x86_64-unknown-linux-gnu"]);
/// env.assert_ok("generate", &["--workspace"]);
/// env.assert_generated(&["pkg-a", "pkg-a-linux-x64", "pkg-b", "pkg-b-linux-x64"]);
/// ```
#[test]
fn workspace_npm_config_targets_used() {
    let env = TestEnv::workspace_with_config(
        &["pkg-a", "pkg-b"],
        toml::toml! {
            [workspace.metadata.npm]
            targets = ["x86_64-unknown-linux-gnu"]
        },
    );
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--workspace"]);
    env.assert_generated(&["pkg-a", "pkg-a-linux-x64", "pkg-b", "pkg-b-linux-x64"]);
}

/// Verifies that a package's npm `targets` setting overrides the workspace-level `targets` when generating packages.
///
/// This test creates a workspace with a workspace-wide npm target (Linux x86_64) and a package (`pkg-a`) that
/// specifies its own npm target (macOS aarch64). It then generates packages for the workspace and asserts that
/// `pkg-a` uses its package-level target while `pkg-b` uses the workspace default.
///
/// # Examples
///
/// ```
/// // Setup a workspace with a workspace-level npm target and a package-level override.
/// let env = TestEnv::workspace_with_config(&["pkg-a", "pkg-b"], toml::toml! {
///     [workspace.metadata.npm]
///     targets = ["x86_64-unknown-linux-gnu"]
/// });
/// env.create_toml("crates/pkg-a/Cargo.toml", &toml::toml! {
///     [package]
///     name = "pkg-a"
///     version = "1.0.0"
///     edition = "2021"
///     license = "MIT"
///     [[bin]]
///     name = "pkg-a"
///     path = "src/main.rs"
///     [package.metadata.npm]
///     targets = ["aarch64-apple-darwin"]
/// });
/// env.create_binaries(&["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]);
/// env.assert_ok("generate", &["--workspace"]);
/// env.assert_generated(&["pkg-a", "pkg-a-darwin-arm64", "pkg_b", "pkg-b-linux-x64"]);
/// ```
#[test]
fn package_targets_override_workspace_targets() {
    let env = TestEnv::workspace_with_config(
        &["pkg-a", "pkg-b"],
        toml::toml! {
            [workspace.metadata.npm]
            targets = ["x86_64-unknown-linux-gnu"]
        },
    );
    // Override pkg-a to target aarch64 instead of the workspace default.
    env.create_toml(
        "crates/pkg-a/Cargo.toml",
        &toml::toml! {
            [package]
            name = "pkg-a"
            version = "1.0.0"
            edition = "2021"
            license = "MIT"

            [[bin]]
            name = "pkg-a"
            path = "src/main.rs"

            [package.metadata.npm]
            targets = ["aarch64-apple-darwin"]
        },
    );
    env.create_binaries(&["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--workspace"]);
    env.assert_generated(&["pkg-a", "pkg-a-darwin-arm64", "pkg-b", "pkg-b-linux-x64"]);
}

#[test]
fn target_dir_flag_overrides_default() {
    let env = TestEnv::package();
    env.create_file(
        "custom-target/x86_64-unknown-linux-gnu/release/my-tool",
        b"fake binary",
    );
    env.assert_ok(
        "generate",
        &["--infer-targets", "--target-dir", "custom-target"],
    );
    env.assert_generated(&["my-tool", "my-tool-linux-x64"]);
}

// CLI flags - package selection

#[test]
fn package_flag_filters_and_supports_glob() {
    let env = TestEnv::workspace(&["pkg-a", "pkg-b", "other"]);
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);

    env.assert_ok("generate", &["--infer-targets", "-p", "pkg-a"]);
    env.assert_generated(&["pkg-a", "pkg-a-linux-x64"]);

    env.assert_ok("generate", &["--infer-targets", "-p", "pkg-*"]);
    env.assert_generated(&["pkg-a", "pkg-a-linux-x64", "pkg-b", "pkg-b-linux-x64"]);
}

#[test]
fn workspace_flag_processes_all_packages() {
    let env = TestEnv::workspace(&["pkg-a", "pkg-b"]);
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--workspace", "--infer-targets"]);
    env.assert_generated(&["pkg-a", "pkg-a-linux-x64", "pkg-b", "pkg-b-linux-x64"]);
}

/// Verifies that `--exclude` omits matching workspace packages and accepts glob patterns.
///
/// First it runs `generate --workspace --exclude pkg-b --infer-targets` and asserts that
/// packages for `pkg-a` and `other` (and their platform packages) are generated while `pkg-b` is omitted.
/// Then it runs `generate` with `--exclude pkg-* --clean --infer-targets` to ensure glob-based exclusion
/// removes `pkg-a` as well, leaving only `other` and its platform package.
///
/// # Examples
///
/// ```
/// let env = TestEnv::workspace(&["pkg-a", "pkg-b", "other"]);
/// env.create_binaries(&["x86_64-unknown-linux-gnu"]);
/// env.assert_ok("generate", &["--workspace", "--exclude", "pkg-b", "--infer-targets"]);
/// env.assert_generated(&["pkg-a", "pkg-a-linux-x64", "other", "other-linux-x64"]);
///
/// env.assert_ok(
///     "generate",
///     &["--workspace", "--exclude", "pkg-*", "--clean", "--infer-targets"],
/// );
/// env.assert_generated(&["other", "other-linux-x64"]);
/// ```
#[test]
fn exclude_flag_skips_package_and_supports_glob() {
    let env = TestEnv::workspace(&["pkg-a", "pkg-b", "other"]);
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok(
        "generate",
        &["--workspace", "--exclude", "pkg-b", "--infer-targets"],
    );
    env.assert_generated(&["pkg-a", "pkg-a-linux-x64", "other", "other-linux-x64"]);

    env.assert_ok(
        "generate",
        &[
            "--workspace",
            "--exclude",
            "pkg-*",
            "--clean",
            "--infer-targets",
        ],
    );
    env.assert_generated(&["other", "other-linux-x64"]);
}

#[test]
fn exclude_without_workspace_is_error() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_err(
        "generate",
        &["--exclude", "my-tool"],
        "--exclude can only be used together with --workspace",
    );
}

/// Fails if a package selection pattern does not match any workspace member.
///
/// This test runs the `generate` command with a package pattern that does not match
/// any package in the workspace and asserts the CLI errors with a message
/// indicating the pattern was not found.
///
/// # Examples
///
/// ```
/// let env = TestEnv::package();
/// env.create_binaries(&["x86_64-unknown-linux-gnu"]);
/// env.assert_err("generate", &["-p", "nonexistent"], "not found in workspace");
/// ```
#[test]
fn unmatched_package_pattern_is_error() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_err("generate", &["-p", "nonexistent"], "not found in workspace");
}

// npm config - template variables, bins, array form, workspace config

/// Ensures `{name}` template variables in npm `name` and `prefix` are substituted into the generated main and platform package names.
///
/// # Examples
///
/// ```
/// let env = TestEnv::package_with_config(toml::toml! {
///     [package.metadata.npm]
///     name = "{name}-cli"
///     prefix = "@org/{name}-"
/// });
/// env.create_binaries(&["x86_64-unknown-linux-gnu"]);
/// env.assert_ok("generate", &["--infer-targets"]);
/// env.assert_generated(&["my-tool-cli", "@org/my-tool-linux-x64"]);
/// ```
#[test]
fn template_variables_in_name_and_prefix() {
    let env = TestEnv::package_with_config(toml::toml! {
        [package.metadata.npm]
        name = "{name}-cli"
        prefix = "@org/{name}-"
    });
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);
    env.assert_generated(&["my-tool-cli", "@org/my-tool-linux-x64"]);
}

/// Verifies that `package.metadata.npm.bins` restricts generated shims to the listed binaries.
///
/// The test creates two Cargo binary targets, configures `package.metadata.npm.bins = ["tool-a"]`,
/// runs `generate --infer-targets`, and asserts that a shim is produced only for `tool-a` and not for `tool-b`.
///
/// # Examples
///
/// ```
/// // After setup and running `generate --infer-targets`:
/// // - npm/my-tool/bin/tool-a.js exists
/// // - npm/my-tool/bin/tool-b.js does not exist
/// ```
#[test]
fn bins_config_limits_included_binaries() {
    let env = TestEnv::package_with_config(toml::toml! {
        [[bin]]
        name = "tool-a"
        path = "src/bin/tool-a.rs"

        [[bin]]
        name = "tool-b"
        path = "src/bin/tool-b.rs"

        [package.metadata.npm]
        bins = ["tool-a"]
    });
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    env.assert_exists("npm/my-tool/bin/tool-a.js");
    env.assert_not_exists("npm/my-tool/bin/tool-b.js");
}

/// Produces separate npm packages for each entry when `package.metadata.npm` is provided in array form.
///
/// Verifies that multiple `[[package.metadata.npm]]` entries create distinct main packages and their corresponding platform packages, and that each package includes shims only for the bins it declares.
///
/// # Examples
///
/// ```
/// // Sets up two bins and two npm package entries:
/// // - `@myorg/tool-a` with bin "tool-a"
/// // - `@myorg/tool-b` with bin "tool-b"
/// // After generating for Linux x86_64, both main and platform packages and their shims exist.
/// ```
fn array_form_produces_multiple_packages() {
    let env = TestEnv::package_with_config(toml::toml! {
        [[bin]]
        name = "tool-a"
        path = "src/bin/tool-a.rs"

        [[bin]]
        name = "tool-b"
        path = "src/bin/tool-b.rs"

        [[package.metadata.npm]]
        name = "@myorg/tool-a"
        bins = ["tool-a"]

        [[package.metadata.npm]]
        name = "@myorg/tool-b"
        bins = ["tool-b"]
    });
    env.create_file("src/bin/tool-a.rs", b"fn main() {}");
    env.create_file("src/bin/tool-b.rs", b"fn main() {}");
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);
    env.assert_generated(&[
        "@myorg/tool-a",
        "@myorg/tool-a-linux-x64",
        "@myorg/tool-b",
        "@myorg/tool-b-linux-x64",
    ]);
    env.assert_exists("npm/@myorg/tool-a/bin/tool-a.js");
    env.assert_exists("npm/@myorg/tool-b/bin/tool-b.js");
}

/// Verifies workspace-level npm configuration is applied to each workspace member when generating packages.
///
/// This test ensures a `workspace.metadata.npm` prefix template is substituted per-package and
/// that both main and platform packages are produced for each workspace member.
///
/// # Examples
///
/// ```
/// let env = TestEnv::workspace_with_config(
///     &["pkg-a", "pkg-b"],
///     toml::toml! {
///         [workspace.metadata.npm]
///         prefix = "@myorg/{name}-"
///     },
/// );
/// env.create_binaries(&["x86_64-unknown-linux-gnu"]);
/// env.assert_ok("generate", &["--workspace", "--infer-targets"]);
/// env.assert_generated(&[
///     "pkg-a",
///     "@myorg/pkg-a-linux-x64",
///     "pkg-b",
///     "@myorg/pkg-b-linux-x64",
/// ]);
/// ```
#[test]
fn workspace_config_applies_to_packages() {
    let env = TestEnv::workspace_with_config(
        &["pkg-a", "pkg-b"],
        toml::toml! {
            [workspace.metadata.npm]
            prefix = "@myorg/{name}-"
        },
    );
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--workspace", "--infer-targets"]);
    env.assert_generated(&[
        "pkg-a",
        "@myorg/pkg-a-linux-x64",
        "pkg-b",
        "@myorg/pkg-b-linux-x64",
    ]);
}

/// Ensures custom npm fields from `package.metadata.npm` are applied to the generated main package.
///
/// # Examples
///
/// ```
/// let env = TestEnv::package_with_config(toml::toml! {
///     [package.metadata.npm]
///     custom = {
///         engines = { node = ">=18" },
///         description = "Custom desc",
///         funding = "https://example.com",
///     }
/// });
/// env.create_binaries(&["x86_64-unknown-linux-gnu"]);
/// env.assert_ok("generate", &["--infer-targets"]);
///
/// let pkg = env.read_json("npm/my-tool/package.json");
/// assert_eq!(pkg["description"], "Custom desc");
/// assert_eq!(pkg["funding"], "https://example.com");
/// assert_eq!(pkg["engines"]["node"], ">=18");
/// ```
#[test]
fn custom_fields_applied_to_main_package() {
    let env = TestEnv::package_with_config(toml::toml! {
        [package.metadata.npm]
        custom = {
            engines = { node = ">=18" },
            description = "Custom desc",
            funding = "https://example.com",
        }
    });
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    let pkg = env.read_json("npm/my-tool/package.json");
    assert_eq!(pkg["description"], "Custom desc");
    assert_eq!(pkg["funding"], "https://example.com");
    assert_eq!(pkg["engines"]["node"], ">=18");
}

#[test]
fn single_author_uses_author_field() {
    let env = TestEnv::package_with_config(toml::toml! {
        [package]
        authors = ["Alice <alice@example.com>"]
    });
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    let pkg = env.read_json("npm/my-tool/package.json");
    assert_eq!(pkg["author"], "Alice <alice@example.com>");
    assert!(pkg["contributors"].is_null());
}

#[test]
fn multiple_authors_uses_contributors_field() {
    let env = TestEnv::package_with_config(toml::toml! {
        [package]
        authors = ["Alice <alice@example.com>", "Bob <bob@example.com>"]
    });
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    let pkg = env.read_json("npm/my-tool/package.json");
    assert!(pkg["author"].is_null());
    assert_eq!(pkg["contributors"][0], "Alice <alice@example.com>");
    assert_eq!(pkg["contributors"][1], "Bob <bob@example.com>");
}

/// Ensures whitelisted custom npm fields are copied into platform packages and non-whitelisted fields are omitted.
///
/// This test sets package-level custom npm fields, generates packages, and asserts that `publishConfig` is present
/// in the platform package's `package.json` while `funding` (a non-whlisted field) is not.
///
/// # Examples
///
/// ```
/// // After generation, the platform package should include publishConfig but not funding:
/// // assert_eq!(platform_pkg["publishConfig"]["access"], "public");
/// // assert!(platform_pkg["funding"].is_null());
/// ```
#[test]
fn whitelisted_custom_fields_copied_to_platform_packages() {
    let env = TestEnv::package_with_config(toml::toml! {
        [package.metadata.npm]
        custom = {
            publishConfig = { access = "public" },
            funding = "https://example.com",
        }
    });
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    let platform_pkg = env.read_json("npm/my-tool-linux-x64/package.json");
    assert_eq!(platform_pkg["publishConfig"]["access"], "public");
    // Non-whitelisted field must not appear in platform package
    assert!(platform_pkg["funding"].is_null());
}

// Publish

/// Verifies publishing uses scoped npm names for both the main package and its platform package.
///
/// # Examples
///
/// ```
/// // create a package that uses a scoped npm name and a scoped platform prefix,
/// // generate platform packages from binaries, then publish and assert published names
/// let env = TestEnv::package_with_config(toml::toml! {
///     [package.metadata.npm]
///     name = "@scope/my-tool"
///     prefix = "@scope/my-tool-"
/// });
/// env.create_binaries(&["x86_64-unknown-linux-gnu"]);
/// env.assert_ok("generate", &["--infer-targets"]);
/// env.assert_ok("publish", &[]);
/// env.assert_published(&["@scope/my-tool", "@scope/my-tool-linux-x64"]);
/// ```
#[test]
fn publish_scoped_packages() {
    let env = TestEnv::package_with_config(toml::toml! {
        [package.metadata.npm]
        name = "@scope/my-tool"
        prefix = "@scope/my-tool-"
    });
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);
    env.assert_ok("publish", &[]);
    env.assert_published(&["@scope/my-tool", "@scope/my-tool-linux-x64"]);
}

#[test]
fn publish_multiple_platforms() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]);
    env.assert_ok("generate", &["--infer-targets"]);
    env.assert_ok("publish", &[]);
    env.assert_published(&["my-tool", "my-tool-darwin-arm64", "my-tool-linux-x64"]);

    // Main package must be published after all platform packages.
    let pkgs = env.published_packages();
    assert_eq!(
        pkgs.last().unwrap(),
        "my-tool",
        "main package should be last: {pkgs:?}"
    );
}

#[test]
fn publish_extra_args_passed_through() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);
    env.assert_ok("publish", &["--", "--access", "public"]);

    let log = env.read_file("npm-publish.log");
    assert!(log.contains("--access public"), "log: {log}");
}

/// Verifies that `publish` uploads every package listed in the main package's `optionalDependencies`.
///
/// The test generates platform packages, runs `publish`, and asserts that the main package and
/// all platform packages referenced as optional dependencies were published.
///
/// # Examples
///
/// ```
/// // create environment with linux and darwin-arm64 binaries
/// let env = TestEnv::package();
/// env.create_binaries(&["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]);
/// env.assert_ok("generate", &["--infer-targets"]);
/// env.assert_generated(&["my-tool", "my-tool-darwin-arm64", "my-tool-linux-x64"]);
///
/// env.assert_ok("publish", &[]);
/// env.assert_published(&["my-tool", "my-tool-darwin-arm64", "my-tool-linux-x64"]);
/// ```
#[test]
fn publish_publishes_all_optional_deps() {
    // Publish always publishes all packages listed in optionalDependencies.
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]);
    env.assert_ok("generate", &["--infer-targets"]);
    env.assert_generated(&["my-tool", "my-tool-darwin-arm64", "my-tool-linux-x64"]);

    env.assert_ok("publish", &[]);
    env.assert_published(&["my-tool", "my-tool-darwin-arm64", "my-tool-linux-x64"]);
}

/// Ensures publishing is idempotent by skipping packages that are already published.
///
/// Publishes generated packages, runs publish again, and asserts the second run succeeds
/// and each package appears only once in the publish log.
///
/// # Examples
///
/// ```
/// let env = TestEnv::package();
/// env.create_binaries(&["x86_64-unknown-linux-gnu"]);
/// env.assert_ok("generate", &["--infer-targets"]);
/// env.assert_ok("publish", &[]);
/// // Second publish should succeed and not duplicate published entries.
/// env.assert_ok("publish", &[]);
/// env.assert_published(&["my-tool", "my-tool-linux-x64"]);
/// ```
#[test]
fn publish_skips_already_published_packages() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);
    env.assert_ok("publish", &[]);

    // Second publish should succeed, skipping packages already on the registry.
    env.assert_ok("publish", &[]);

    // Each package should only appear once in the publish log.
    env.assert_published(&["my-tool", "my-tool-linux-x64"]);
}

/// Verifies that publishing fails when the package's shim file referenced in `package.json` is missing.
///
/// This test generates npm packages from built binaries, deletes the generated shim (`bin/my-tool.js`),
/// and asserts that `publish` errors with a message mentioning the missing shim path.
///
/// # Examples
///
/// ```
/// let env = TestEnv::package();
/// env.create_binaries(&["x86_64-unknown-linux-gnu"]);
/// env.assert_ok("generate", &["--infer-targets"]);
/// env.remove_file("npm/my-tool/bin/my-tool.js");
/// env.assert_err("publish", &[], "bin/my-tool.js");
/// ```
#[test]
fn publish_fails_if_shim_missing() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    // Remove the generated shim so package.json is correct but the file is absent.
    env.remove_file("npm/my-tool/bin/my-tool.js");
    env.assert_err("publish", &[], "bin/my-tool.js");
}

#[test]
fn publish_fails_if_platform_package_name_wrong() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    env.create_file(
        "npm/my-tool-linux-x64/package.json",
        br#"{"name":"wrong-name","version":"1.0.0","os":["linux"],"cpu":["x64"]}"#,
    );
    env.assert_err(
        "publish",
        &[],
        "expected \"my-tool-linux-x64\", got \"wrong-name\"",
    );
}

#[test]
fn publish_fails_if_main_package_shim_missing() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    env.remove_file("npm/my-tool/bin/my-tool.js");
    env.assert_err(
        "publish",
        &[],
        "bin file 'bin/my-tool.js' for 'my-tool' not found",
    );
}

#[test]
fn publish_fails_if_main_package_optional_dep_missing() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    let mut pkg = env.read_json("npm/my-tool/package.json");
    pkg["optionalDependencies"] = serde_json::json!({});
    env.create_file(
        "npm/my-tool/package.json",
        serde_json::to_string(&pkg).unwrap().as_bytes(),
    );
    env.assert_err("publish", &[], "no platform packages found");
}

#[test]
fn publish_fails_if_optional_dep_version_mismatch() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    let mut pkg = env.read_json("npm/my-tool/package.json");
    pkg["optionalDependencies"]["my-tool-linux-x64"] = serde_json::json!("0.0.0");
    env.create_file(
        "npm/my-tool/package.json",
        serde_json::to_string(&pkg).unwrap().as_bytes(),
    );
    env.assert_err("publish", &[], "version in optionalDependencies");
}

#[test]
fn publish_fails_if_platform_package_missing() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    // Remove the platform package directory entirely.
    std::fs::remove_dir_all(env.path().join("npm/my-tool-linux-x64")).unwrap();
    env.assert_err("publish", &[], "not found");
}

/// Ensures publishing fails when a platform package is missing its expected binary.
///
/// This test generates packages for a Linux target, deletes the platform binary file,
/// and asserts that `publish` reports a missing-binary error for the platform package.
///
/// # Examples
///
/// ```rust
/// // Arrange: generate packages for the Linux target
/// let env = TestEnv::package();
/// env.create_binaries(&["x86_64-unknown-linux-gnu"]);
/// env.assert_ok("generate", &["--infer-targets"]);
///
/// // Act: remove the platform binary and attempt to publish
/// env.remove_file("npm/my-tool-linux-x64/my-tool");
///
/// // Assert: publish fails with a missing binary error
/// env.assert_err("publish", &[], "binary 'my-tool' missing");
/// ```
#[test]
fn publish_fails_if_binary_missing_from_platform_package() {
    let env = TestEnv::package();
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    env.assert_ok("generate", &["--infer-targets"]);

    env.remove_file("npm/my-tool-linux-x64/my-tool");
    env.assert_err("publish", &[], "binary 'my-tool' missing");
}

#[test]
fn publish_fails_if_configured_target_package_missing() {
    let env = TestEnv::package_with_config(toml::toml! {
        [package.metadata.npm]
        targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]
    });
    env.create_binaries(&["x86_64-unknown-linux-gnu"]);
    // Only generate for one target, leaving aarch64-apple-darwin missing.
    env.assert_ok("generate", &["--target", "x86_64-unknown-linux-gnu"]);
    env.assert_err("publish", &[], "my-tool-darwin-arm64");
}
