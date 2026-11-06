use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");

    // ggml's Metal backend guards newer APIs with `@available(...)`. When the deployment target is
    // older than the SDK (the Tauri CLI sets it from `bundle.macOS.minimumSystemVersion`), clang
    // compiles those guards into calls to `__isPlatformVersionAtLeast`, which lives in the
    // compiler runtime (`libclang_rt.osx.a`). rustc links with `-nodefaultlibs`, so that runtime is
    // never on the link line and the final binary fails with an undefined symbol. Link it here:
    // unlike `rustc-link-arg`, a `rustc-link-lib` reaches every binary that depends on this crate.
    if env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "macos") {
        link_clang_runtime();
    }
}

fn link_clang_runtime() {
    let compiler = env::var("CC").unwrap_or_else(|_| "cc".to_owned());
    let resource_dir = Command::new(&compiler)
        .arg("-print-resource-dir")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()));

    let Some(lib_dir) = resource_dir.map(|dir| dir.join("lib").join("darwin")) else {
        println!("cargo:warning=could not ask `{compiler}` for its resource directory; libclang_rt.osx is not linked");
        return;
    };
    if !lib_dir.join("libclang_rt.osx.a").is_file() {
        println!(
            "cargo:warning=libclang_rt.osx.a not found in {}; it is not linked",
            lib_dir.display()
        );
        return;
    }
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=clang_rt.osx");
}
