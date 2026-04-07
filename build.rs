/// Emit Cargo build-script directives derived from the `TARGET` environment variable.
///
/// Reads the `TARGET` environment variable, panics if it is not set, and prints
/// two Cargo directives: `cargo:rustc-env=TARGET_TRIPLE={target}` to expose the
/// target triple to compiled crates, and `cargo:rerun-if-changed=build.rs` to
/// force rebuilds when this build script changes.
fn main() {
    // Embed the target triple so source/mod.rs can use it for target/release/ detection.
    let target = std::env::var("TARGET").unwrap();
    println!("cargo:rustc-env=TARGET_TRIPLE={target}");
    println!("cargo:rerun-if-changed=build.rs");
}