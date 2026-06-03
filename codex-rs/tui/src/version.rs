/// The current Codex CLI version as embedded at compile time.
pub const CODEX_CLI_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Version label for user-visible UI surfaces.
pub const CODEX_CLI_DISPLAY_VERSION: &str = match option_env!("CODEX_CLI_DISPLAY_VERSION") {
    Some(version) => version,
    None => CODEX_CLI_VERSION,
};
/// Source checkout used to build this binary, when this is a local source build.
#[allow(dead_code)]
pub const CODEX_CLI_BUILD_DIR: Option<&str> = option_env!("CODEX_CLI_BUILD_DIR");
/// Public release version this local source build was based on, when known.
#[allow(dead_code)]
pub const CODEX_CLI_UPDATE_BASE_VERSION: Option<&str> =
    option_env!("CODEX_CLI_UPDATE_BASE_VERSION");
