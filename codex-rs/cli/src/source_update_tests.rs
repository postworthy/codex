use std::ffi::OsString;
use std::path::Path;

use pretty_assertions::assert_eq;

use super::agent_assisted_update_command;
use super::install_source_built_binaries_to;
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

#[test]
fn source_package_install_replaces_codex_and_code_mode_host() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let package_dir = temp_dir.path().join("package");
    let package_bin_dir = package_dir.join("bin");
    let installed_dir = temp_dir.path().join("installed");
    std::fs::create_dir_all(&package_bin_dir).expect("create package bin dir");
    std::fs::create_dir_all(&installed_dir).expect("create installed dir");
    std::fs::write(package_bin_dir.join("codex"), b"new codex").expect("write packaged codex");
    std::fs::write(package_bin_dir.join("codex-code-mode-host"), b"new host")
        .expect("write packaged host");
    let installed_bin = installed_dir.join("codex");
    let installed_host = installed_dir.join("codex-code-mode-host");
    std::fs::write(&installed_bin, b"old codex").expect("write installed codex");
    std::fs::write(&installed_host, b"old host").expect("write installed host");

    install_source_built_binaries_to(&package_dir, &installed_bin).expect("install package");

    assert_eq!(std::fs::read(&installed_bin).unwrap(), b"new codex");
    assert_eq!(std::fs::read(&installed_host).unwrap(), b"new host");
    let backups = std::fs::read_dir(&installed_dir)
        .expect("read installed dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".bak."))
        .count();
    assert_eq!(backups, 2);
}
