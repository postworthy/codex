fn main() {
    println!("cargo:rerun-if-env-changed=CODEX_CLI_DISPLAY_VERSION");
    if let Ok(version) = std::env::var("CODEX_CLI_DISPLAY_VERSION") {
        println!("cargo:rustc-env=CODEX_CLI_DISPLAY_VERSION={version}");
    }
    println!("cargo:rerun-if-env-changed=CODEX_CLI_BUILD_DIR");
    if let Ok(build_dir) = std::env::var("CODEX_CLI_BUILD_DIR") {
        println!("cargo:rustc-env=CODEX_CLI_BUILD_DIR={build_dir}");
    }

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-ObjC");
    }
}
