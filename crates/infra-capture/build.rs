use std::env;

/// `screencapturekit` links a Swift bridge that needs the Swift runtime
/// (`libswift_Concurrency.dylib`, ...). Link arguments emitted by a dependency's build script
/// do not propagate to this package's test and example binaries, so the system Swift runtime
/// directory is added to their rpath here. No-op on every other target.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
