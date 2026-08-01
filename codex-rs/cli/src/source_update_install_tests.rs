use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use pretty_assertions::assert_eq;

use super::SourceInstallLayout;
use super::SourceUpdateLock;
use super::install_source_package_with;
use super::smoke_test_code_mode_host;
use super::validate_source_package;

const TARGET: &str = "x86_64-unknown-linux-gnu";

#[test]
fn installs_complete_packages_atomically_and_retains_two_releases() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let source_root = temp_dir.path().join("source");
    let bin_dir = temp_dir.path().join("bin");

    for release_number in 1..=3 {
        let layout = SourceInstallLayout::new(
            &source_root,
            &bin_dir,
            &format!("source-{release_number:02}-{TARGET}"),
        );
        write_package(
            layout.staging_dir(),
            TARGET,
            &format!("codex {release_number}"),
        );
        install_source_package_with(&layout, TARGET, |_| Ok(())).expect("install package");
    }

    let current = std::fs::read_link(source_root.join("current")).expect("current release");
    let expected_release_name = format!("source-03-{TARGET}");
    assert_eq!(
        current.file_name().and_then(|name| name.to_str()),
        Some(expected_release_name.as_str())
    );
    assert_eq!(
        std::fs::read_to_string(current.join("codex-resources/marker")).expect("packaged resource"),
        "codex 3"
    );
    assert_eq!(
        std::fs::read_link(bin_dir.join("codex")).expect("visible command"),
        source_root.join("current/bin/codex")
    );
    let releases = std::fs::read_dir(source_root.join("releases"))
        .expect("releases")
        .collect::<Result<Vec<_>, _>>()
        .expect("release entries");
    assert_eq!(releases.len(), 2);
}

#[test]
fn retention_stays_bounded_when_current_release_sorts_before_old_releases() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let source_root = temp_dir.path().join("source");
    let bin_dir = temp_dir.path().join("bin");

    for release_name in ["source-bootstrap-01", "source-bootstrap-02", "source-03"] {
        let layout = SourceInstallLayout::new(&source_root, &bin_dir, release_name);
        write_package(layout.staging_dir(), TARGET, release_name);
        install_source_package_with(&layout, TARGET, |_| Ok(())).expect("install package");
    }

    let mut releases = std::fs::read_dir(source_root.join("releases"))
        .expect("releases")
        .map(|entry| entry.expect("release entry").file_name())
        .collect::<Vec<_>>();
    releases.sort();
    assert_eq!(
        releases,
        ["source-03", "source-bootstrap-02"].map(OsString::from)
    );
}

#[test]
fn rolls_back_current_release_when_post_install_smoke_test_fails() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let source_root = temp_dir.path().join("source");
    let bin_dir = temp_dir.path().join("bin");
    let first = SourceInstallLayout::new(&source_root, &bin_dir, "source-01");
    write_package(first.staging_dir(), TARGET, "first");
    install_source_package_with(&first, TARGET, |_| Ok(())).expect("first install");
    let previous = std::fs::read_link(source_root.join("current")).expect("previous current");

    let second = SourceInstallLayout::new(&source_root, &bin_dir, "source-02");
    write_package(second.staging_dir(), TARGET, "second");
    let mut smoke_test_count = 0;
    let error = install_source_package_with(&second, TARGET, |_| {
        smoke_test_count += 1;
        if smoke_test_count == 1 {
            Ok(())
        } else {
            anyhow::bail!("synthetic post-install failure")
        }
    })
    .expect_err("second install should fail");

    assert!(error.to_string().contains("rolled back"));
    assert_eq!(
        std::fs::read_link(source_root.join("current")).expect("rolled back current"),
        previous
    );
    assert!(!source_root.join("releases/source-02").exists());
}

#[test]
fn rejects_incomplete_or_wrong_target_packages_before_switching() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let package_dir = temp_dir.path().join("package");
    write_package(&package_dir, "aarch64-unknown-linux-gnu", "wrong target");

    let error = validate_source_package(&package_dir, TARGET).expect_err("target mismatch");

    assert!(error.to_string().contains("metadata field `target`"));
}

#[test]
fn source_update_lock_excludes_concurrent_updates_and_releases_on_drop() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let layout = SourceInstallLayout::new(temp_dir.path(), &temp_dir.path().join("bin"), "one");
    let lock = SourceUpdateLock::acquire(&layout).expect("first lock");

    let error = SourceUpdateLock::acquire(&layout).expect_err("concurrent lock");
    assert!(error.to_string().contains("already running"));

    drop(lock);
    SourceUpdateLock::acquire(&layout).expect("lock after drop");
}

#[test]
fn code_mode_host_smoke_test_checks_readiness_endpoint() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let host = temp_dir.path().join("codex-code-mode-host");
    std::fs::write(
        &host,
        r#"#!/bin/sh
exec python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
s.listen(1)
print("ws://%s:%d" % s.getsockname(), flush=True)
c, _ = s.accept()
c.recv(4096)
c.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
c.close()
s.close()'
"#,
    )
    .expect("write fake host");
    let mut permissions = std::fs::metadata(&host).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&host, permissions).expect("host permissions");

    smoke_test_code_mode_host(&host).expect("host readiness");
}

fn write_package(package_dir: &Path, target: &str, marker: &str) {
    std::fs::create_dir_all(package_dir.join("bin")).expect("package bin");
    std::fs::create_dir_all(package_dir.join("codex-path")).expect("package path");
    std::fs::create_dir_all(package_dir.join("codex-resources")).expect("package resources");
    std::fs::write(
        package_dir.join("codex-package.json"),
        serde_json::to_vec(&serde_json::json!({
            "layoutVersion": 1,
            "version": "0.0.0",
            "target": target,
            "variant": "codex",
            "entrypoint": "bin/codex",
            "resourcesDir": "codex-resources",
            "pathDir": "codex-path",
        }))
        .expect("package metadata"),
    )
    .expect("write package metadata");
    for relative_path in [
        "bin/codex",
        "bin/codex-code-mode-host",
        "codex-path/rg",
        "codex-resources/bwrap",
    ] {
        let path = package_dir.join(relative_path);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write executable");
        let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("executable permissions");
    }
    std::fs::write(package_dir.join("codex-resources/marker"), marker).expect("write marker");
}
