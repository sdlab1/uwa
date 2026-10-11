//! Bakes the target triple into the binary so `uwa update` doesn't have to
//! guess it at runtime (where `std::env::consts::ARCH` + `::OS` can't
//! distinguish `gnu` from `musl`, or `msvc` from `gnu`).

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    println!("cargo:rustc-env=UWA_TARGET={target}");
    println!("cargo:rustc-env=UWA_VERSION={version}");
    println!("cargo:rerun-if-changed=build.rs");
}
