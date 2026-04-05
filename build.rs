fn main() {
    // Embed the target triple so source/mod.rs can use it for target/release/ detection.
    let target = std::env::var("TARGET").unwrap();
    println!("cargo:rustc-env=TARGET_TRIPLE={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
