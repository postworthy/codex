use std::ffi::OsString;
use std::path::Path;

use pretty_assertions::assert_eq;

use super::agent_assisted_update_command;
use super::source_package_build_command;

#[test]
fn agent_assisted_update_runs_codex_in_checkout_with_failure_context() {
    let codex_bin = Path::new("/opt/codex/bin/codex");
    let build_dir = Path::new("/work/codex");
    let failure = anyhow::anyhow!(
        "git merge failed with status 1. Conflicted paths before abort:\ncli/src/main.rs"
    );

    let command = agent_assisted_update_command(codex_bin, build_dir, "0.145.0", &failure);
    let args: Vec<OsString> = command.get_args().map(OsString::from).collect();

    assert_eq!(command.get_program(), codex_bin.as_os_str());
    assert_eq!(command.get_current_dir(), Some(build_dir));
    assert_eq!(
        &args[..5],
        &[
            OsString::from("exec"),
            OsString::from("--cd"),
            build_dir.as_os_str().to_os_string(),
            OsString::from("-c"),
            OsString::from("check_for_update_on_startup=false"),
        ]
    );
    let prompt = args[5].to_string_lossy();
    assert!(prompt.contains("Repository: /work/codex"));
    assert!(prompt.contains("Target Codex release: 0.145.0"));
    assert!(prompt.contains(
        "Failure: git merge failed with status 1. Conflicted paths before abort:\ncli/src/main.rs"
    ));
    assert!(prompt.contains("merge upstream/main"));
    assert!(prompt.contains("Do not merely explain the steps; perform the work."));
}

#[test]
fn source_package_build_uses_supported_packaging_path() {
    let build_dir = Path::new("/work/codex");
    let package_dir = Path::new("/work/codex/codex-rs/target/source-update-package");

    let command = source_package_build_command(build_dir, package_dir, "x86_64-unknown-linux-gnu");
    let args: Vec<OsString> = command.get_args().map(OsString::from).collect();

    assert_eq!(command.get_program(), "python3");
    assert_eq!(command.get_current_dir(), Some(build_dir));
    assert_eq!(
        args,
        vec![
            OsString::from("/work/codex/scripts/build_codex_package.py"),
            OsString::from("--target"),
            OsString::from("x86_64-unknown-linux-gnu"),
            OsString::from("--cargo-profile"),
            OsString::from("release"),
            OsString::from("--package-dir"),
            package_dir.as_os_str().to_os_string(),
            OsString::from("--force"),
        ]
    );
}
