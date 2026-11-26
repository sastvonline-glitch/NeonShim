//! The test executable exports the D3D12 Agility SDK symbols
//! (`src/test_agility.rs`). The library itself is never linked, so only test
//! builds see these.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg=/EXPORT:D3D12SDKVersion,DATA");
        println!("cargo:rustc-link-arg=/EXPORT:D3D12SDKPath,DATA");
    }
}
