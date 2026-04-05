# cargo-npm

[![Crates.io](https://img.shields.io/crates/v/cargo-npm)](https://crates.io/crates/cargo-npm)

Package and distribute Rust CLI binaries as npm packages.

**cargo-npm** generates platform-specific npm packages containing your compiled binaries and a main
package with Node.js shims. Users install the main package via npm - it pulls in only the binary for
their platform as an optional dependency and runs it transparently.

## Installation

```sh
cargo install --locked cargo-npm
```

Publishing requires `npm` to be available in `PATH`.

## How It Works

**cargo-npm** produces two kinds of npm packages:

**Platform packages** - one per target (e.g. `my-tool-linux-x64`, `my-tool-darwin-arm64`). Each
contains the compiled binary and a `package.json` with `os`/`cpu` constraints so npm only installs
the one that matches the user's system.

**Main package** - the package users actually install (e.g. `my-tool`). It lists all platform
packages as `optionalDependencies` and includes a small Node.js shim for each binary. The shim
locates the installed platform binary and runs it, forwarding all arguments, stdin/stdout/stderr,
and the exit code.

When you publish both glibc and musl Linux variants, the shim detects which libc the system uses at
runtime and selects the correct binary automatically.

## Getting Started

Build your crate for each target you want to distribute. The following example uses
[cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild) to cross-compile for multiple targets
from a single machine.

```sh
cargo zigbuild --release \
  --target x86_64-unknown-linux-gnu \
  --target aarch64-unknown-linux-gnu \
  --target x86_64-apple-darwin \
  --target aarch64-apple-darwin \
  --target x86_64-pc-windows-gnu \
  --target aarch64-pc-windows-gnullvm
```

Then generate and publish - `generate` picks up compiled binaries from cargo's `target/` directory
automatically.

```sh
cargo npm generate # generates packages into npm/
cargo npm publish  # publish to the npm registry
```

## Configuration

```toml
[package.metadata.npm]
# Main package name. Defaults to the crate name.
name = "my-tool"

# Platform package name prefix. Defaults to "{name}-". It is recommended to
# use a scoped prefix (e.g. "@myorg/my-tool-") as publishing many packages
# at once can trigger npm spam detection.
prefix = "@myorg/my-tool-"

# Binaries to include. Defaults to all [[bin]] targets in the crate.
bins = ["my-tool"]

# Target triples to generate packages for. When unset, falls back to
# build.target in .cargo/config.toml.
targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]

# Output directory. Defaults to "npm". Can only be set at the workspace
# level (or in a standalone crate).
out-dir = "npm"

# Generate mode. In "create" mode (default), package directories are
# recreated from scratch on every run. In "merge" mode, the main package
# is edited in-place, preserving any custom files or package.json fields.
mode = "create"

# Custom fields merged into the main package.json. Only "homepage",
# "license", "repository", and "publishConfig" are also copied to
# platform packages.
custom = {
  funding = {
    type = "github",
    url = "https://github.com/sponsors/myorg",
  }
}
```

### Cargo metadata forwarding

The following `[package]` fields are automatically written to the main package `package.json`:

| `Cargo.toml`                 | `package.json` |
| ---------------------------- | -------------- |
| `version`                    | `version`      |
| `description`                | `description`  |
| `keywords`                   | `keywords`     |
| `homepage`                   | `homepage`     |
| `license`                    | `license`      |
| `authors[0]` (single author) | `author`       |
| `authors` (multiple authors) | `contributors` |
| `repository`                 | `repository`   |

Fields in `custom` always win over forwarded Cargo metadata.

### Templates

The `name`, `prefix`, and `package` fields support template variables:

| Variable    | Value                               |
| ----------- | ----------------------------------- |
| `{name}`    | Crate name                          |
| `{version}` | Crate version                       |
| `{env.VAR}` | Value of environment variable `VAR` |

### Workspace config

Per-crate config in `[package.metadata.npm]` merges on top of workspace defaults in
`[workspace.metadata.npm]`, with crate values taking precedence.  
Templates are particularly useful here to avoid repeating yourself across crates:

```toml
[workspace.metadata.npm]
out-dir = "dist"
name    = "@myorg/{name}"
prefix  = "@myorg/{name}-cli-"
custom  = { homepage = "https://myorg.github.io/{name}" }
```

### Multiple packages per crate

Use the array form to produce multiple npm packages from a single crate with different binary
subsets:

```toml
[[package.metadata.npm]]
name = "@myorg/tool-a"
bins = ["tool-a"]

[[package.metadata.npm]]
name = "@myorg/tool-b"
bins = ["tool-b"]
```

## Generating

```sh
cargo npm generate [OPTIONS]
```

Generates (or regenerates) the npm directory structure, `package.json` files, JS shims, and copies
compiled binaries from the target directory. Safe to re-run at any time.

Targets must be explicitly configured - either via `--target`, `[package.metadata.npm] targets` in
`Cargo.toml`, or `build.target` in `.cargo/config.toml` - unless `--stub` or `--infer-targets` is
passed.

| Flag                     | Description                                                                     |
| ------------------------ | ------------------------------------------------------------------------------- |
| `--manifest-path <PATH>` | Path to `Cargo.toml` (defaults to the current directory)                        |
| `-p, --package <SPEC>`   | Process only the named package; supports glob patterns (repeatable)             |
| `--workspace`            | Process all packages in the workspace                                           |
| `--exclude <SPEC>`       | Exclude a package; requires `--workspace`; supports glob patterns (repeatable)  |
| `--target <TRIPLE>`      | Target triple to generate packages for (repeatable)                             |
| `--target-dir <DIR>`     | Directory for compiled artifacts (defaults to cargo's target directory)         |
| `--out-dir <DIR>`        | Output directory (default: `npm`)                                               |
| `--clean`                | Delete the entire output directory before generating                            |
| `--infer-targets`        | Infer targets from built binaries instead of requiring explicit configuration   |
| `--stub`                 | Generate only the main package; skip platform packages and optionalDependencies |

**Binary discovery** - for each configured target triple, **cargo-npm** looks for binaries in
`target/{triple}/release/`. It also checks `target/release/` and attributes those binaries to the
host platform. Missing binaries are silently skipped. With `--infer-targets`, the `target/`
directory is scanned automatically for any triples that have been compiled. The target directory
defaults to whatever cargo resolves (respecting `CARGO_TARGET_DIR` and `build.target-dir` in
`.cargo/config.toml`).

**Platform collision detection** - if two target triples would produce the same npm package name
(e.g. `x86_64-pc-windows-msvc` and `x86_64-pc-windows-gnu`), generation fails with an error
identifying the conflicting triples. Fix this by specifying only the intended triples via `--target`
or the `targets` config field in `Cargo.toml`.

**Automatic file copying** - the following files are copied from the crate root into each generated
package:

- Any files named `LICENSE`, `LICENCE`, `LICENSE-*`, `LICENCE-*`, or `COPYING` (case-insensitive,
  any extension) are copied into both main and platform packages.
- Any files named `README` (case-insensitive, any extension) are copied into the main package only.

If `license-file` or `readme` are set in `[package]`, those files are copied instead of using
auto-detection.

**Output structure:**

```text
npm/
  my-tool/                       # main package (what users install)
    bin/
      my-tool.js                 # Node.js shim
    package.json
    README.md
    LICENSE
  my-tool-linux-x64/             # platform packages
    my-tool
    package.json
    LICENSE
  my-tool-linux-x64-musl/        # separate musl package (only when both libc variants present)
    my-tool
    package.json
    LICENSE
  my-tool-darwin-arm64/
    my-tool
    package.json
    LICENSE
  my-tool-win32-x64/
    my-tool.exe
    package.json
    LICENSE
  ...
```

## Publishing

```sh
cargo npm publish [OPTIONS] [-- <npm args>]
```

Publishes all packages in the npm directory to the registry. Does not build or copy artifacts - run
`cargo npm generate` first, or download pre-built artifacts from CI.

Before publishing, each package is verified: the binaries must be present and `package.json` must be
up to date (run `cargo npm generate` if not). Platform packages are published first (in parallel),
then the main package - this ensures the optional dependencies are resolvable when the main package
is published. Already-published versions are skipped automatically.

If targets are configured, all of them must be present or publishing will fail.

Pass additional arguments to `npm publish` after `--`:

```sh
cargo npm publish -- --tag beta --access public
```

Set `NODE_AUTH_TOKEN` to authenticate with the registry:

```sh
NODE_AUTH_TOKEN=your-npm-token cargo npm publish
```

## Platform Support

### Supported targets

Any Rust target triple whose OS and CPU are recognised is supported. Unrecognised triples are
silently skipped during binary discovery.

#### OS

| Triple OS segment    | `os`      |
| -------------------- | --------- |
| `linux`              | `linux`   |
| `darwin`             | `darwin`  |
| `windows`            | `win32`   |
| `freebsd`            | `freebsd` |
| `openbsd`            | `openbsd` |
| `netbsd`             | `netbsd`  |
| `solaris`, `illumos` | `sunos`   |
| `aix`                | `aix`     |

#### CPU

| Triple arch segment | `cpu`     |
| ------------------- | --------- |
| `x86_64`            | `x64`     |
| `aarch64`           | `arm64`   |
| `i686`              | `ia32`    |
| `armv7`, `arm`      | `arm`     |
| `riscv64gc`         | `riscv64` |
| `s390x`             | `s390x`   |
| `powerpc64le`       | `ppc64`   |

#### libc (Linux only)

| Triple env segment | `libc`  |
| ------------------ | ------- |
| starts with `gnu`  | `glibc` |
| starts with `musl` | `musl`  |
| absent or other    | unset   |

### Dual libc (glibc + musl)

When you compile for **only glibc**, `libc: ["glibc"]` is set so the package only installs on glibc
systems (a glibc binary won't run on musl).

When you compile for **only musl**, no `libc` field is set and no suffix is added to the package
name. Because musl binaries are statically linked they run on both musl and glibc systems, so
restricting installation would be unnecessarily limiting.

When you compile for **both**, **cargo-npm** separates them:

- Musl packages get a `-musl` suffix (e.g. `my-tool-linux-x64-musl`)
- Both packages have a `libc` field set (`"glibc"` or `"musl"`)
- The shim detects the system's libc at runtime and selects the correct binary
