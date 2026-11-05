use std::{env, fs, path::PathBuf};

/// Common-Controls v6 manifest. `tauri-build` embeds an application manifest into the
/// binary only; test executables need it too, otherwise they fail to start on Windows
/// with `STATUS_ENTRYPOINT_NOT_FOUND` (tao imports comctl32 v6 symbols).
const TEST_MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#;

fn main() {
    tauri_build::build();

    let is_windows_msvc = env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows")
        && env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc");
    if is_windows_msvc {
        let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo"));
        let manifest = out_dir.join("test.manifest");
        fs::write(&manifest, TEST_MANIFEST).expect("failed to write test manifest");
        println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-tests=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
