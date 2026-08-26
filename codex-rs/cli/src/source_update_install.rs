use std::ffi::OsStr;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::net::TcpStream;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use serde_json::Value;

const MAX_RETAINED_RELEASES: usize = 2;
const STALE_LOCK_AGE: Duration = Duration::from_secs(10 * 60);
const HOST_START_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) struct SourceInstallLayout {
    source_root: PathBuf,
    releases_dir: PathBuf,
    staging_dir: PathBuf,
    release_dir: PathBuf,
    current_link: PathBuf,
    visible_bin: PathBuf,
}

impl SourceInstallLayout {
    pub(super) fn for_environment(target: &str) -> anyhow::Result<Self> {
        let codex_home = codex_core::config::find_codex_home()?;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set; cannot install a source update"))?;
        let bin_dir = std::env::var_os("CODEX_INSTALL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/bin"));
        Ok(Self::new(
            codex_home.join("packages/source").as_path(),
            &bin_dir,
            &source_release_name(target),
        ))
    }

    fn new(source_root: &Path, bin_dir: &Path, release_name: &str) -> Self {
        let releases_dir = source_root.join("releases");
        Self {
            source_root: source_root.to_path_buf(),
            releases_dir: releases_dir.clone(),
            staging_dir: source_root.join(format!(".staging-{release_name}")),
            release_dir: releases_dir.join(release_name),
            current_link: source_root.join("current"),
            visible_bin: bin_dir.join("codex"),
        }
    }

    pub(super) fn staging_dir(&self) -> &Path {
        &self.staging_dir
    }
}

#[derive(Debug)]
pub(super) struct SourceUpdateLock {
    path: PathBuf,
    _file: File,
}

impl SourceUpdateLock {
    pub(super) fn acquire(layout: &SourceInstallLayout) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&layout.source_root)?;
        let path = layout.source_root.join("update.lock");
        match create_lock_file(&path) {
            Ok(file) => Ok(Self { path, _file: file }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let is_stale = std::fs::metadata(&path)
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age >= STALE_LOCK_AGE)
                    && !lock_owner_is_running(&path);
                if !is_stale {
                    return Err(error).context("another Codex source update is already running");
                }
                std::fs::remove_file(&path).context("remove stale source-update lock")?;
                let file = create_lock_file(&path)?;
                Ok(Self { path, _file: file })
            }
            Err(error) => Err(error).context("create source-update lock"),
        }
    }
}

impl Drop for SourceUpdateLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(super) fn prepare_staging_dir(layout: &SourceInstallLayout) -> anyhow::Result<()> {
    std::fs::create_dir_all(&layout.releases_dir)?;
    for entry in std::fs::read_dir(&layout.source_root)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(".staging-") {
            let path = entry.path();
            if path.is_dir() {
                std::fs::remove_dir_all(path)?;
            } else {
                std::fs::remove_file(path)?;
            }
        }
    }
    Ok(())
}

pub(super) fn discard_staging_dir(layout: &SourceInstallLayout) {
    if layout.staging_dir.is_dir() {
        let _ = std::fs::remove_dir_all(&layout.staging_dir);
    } else if layout.staging_dir.exists() {
        let _ = std::fs::remove_file(&layout.staging_dir);
    }
}

pub(super) fn install_source_package(
    layout: &SourceInstallLayout,
    target: &str,
) -> anyhow::Result<()> {
    install_source_package_with(layout, target, smoke_test_package)
}

fn install_source_package_with<F>(
    layout: &SourceInstallLayout,
    target: &str,
    mut smoke_test: F,
) -> anyhow::Result<()>
where
    F: FnMut(&Path) -> anyhow::Result<()>,
{
    validate_source_package(&layout.staging_dir, target)?;
    smoke_test(&layout.staging_dir).context("staged source package smoke test failed")?;
    ensure_visible_link_available(layout)?;
    std::fs::create_dir_all(&layout.releases_dir)?;

    if layout.release_dir.exists() {
        anyhow::bail!(
            "source release already exists at {}",
            layout.release_dir.display()
        );
    }
    std::fs::rename(&layout.staging_dir, &layout.release_dir)
        .context("promote staged source package")?;

    let previous_release = std::fs::read_link(&layout.current_link).ok();
    switch_current_release(layout, &layout.release_dir)?;
    if let Err(error) = smoke_test(&layout.release_dir) {
        return rollback_failed_install(layout, previous_release.as_deref(), error);
    }
    if let Err(error) = ensure_visible_link(layout) {
        return rollback_failed_install(layout, previous_release.as_deref(), error);
    }

    if let Err(error) = prune_old_releases(layout) {
        eprintln!("Warning: could not prune old Codex source releases: {error:#}");
    }
    println!(
        "Installed complete source package to {}",
        layout.release_dir.display()
    );
    println!("Active source package: {}", layout.current_link.display());
    println!("Codex command: {}", layout.visible_bin.display());
    Ok(())
}

fn validate_source_package(package_dir: &Path, target: &str) -> anyhow::Result<()> {
    let metadata_path = package_dir.join("codex-package.json");
    let metadata: Value = serde_json::from_slice(
        &std::fs::read(&metadata_path)
            .with_context(|| format!("read {}", metadata_path.display()))?,
    )?;
    let expected_fields = [
        ("layoutVersion", Value::from(1)),
        ("target", Value::from(target)),
        ("variant", Value::from("codex")),
        ("entrypoint", Value::from("bin/codex")),
        ("resourcesDir", Value::from("codex-resources")),
        ("pathDir", Value::from("codex-path")),
    ];
    for (field, expected) in expected_fields {
        if metadata.get(field) != Some(&expected) {
            anyhow::bail!(
                "invalid source package metadata field `{field}`: expected {expected}, found {}",
                metadata.get(field).unwrap_or(&Value::Null)
            );
        }
    }

    let mut executables = vec![
        package_dir.join("bin/codex"),
        package_dir.join("bin/codex-code-mode-host"),
        package_dir.join("codex-path/rg"),
    ];
    if target.contains("linux") {
        executables.push(package_dir.join("codex-resources/bwrap"));
    }
    for executable in executables {
        require_executable(&executable)?;
    }
    Ok(())
}

fn require_executable(path: &Path) -> anyhow::Result<()> {
    if !path.is_file() {
        anyhow::bail!("source package is missing executable {}", path.display());
    }
    use std::os::unix::fs::PermissionsExt;
    if std::fs::metadata(path)?.permissions().mode() & 0o111 == 0 {
        anyhow::bail!("source package file is not executable: {}", path.display());
    }
    Ok(())
}

fn smoke_test_package(package_dir: &Path) -> anyhow::Result<()> {
    let codex = package_dir.join("bin/codex");
    let status = Command::new(&codex).arg("--version").status()?;
    if !status.success() {
        anyhow::bail!("{} --version exited with {status}", codex.display());
    }
    smoke_test_code_mode_host(&package_dir.join("bin/codex-code-mode-host"))
}

fn smoke_test_code_mode_host(host: &Path) -> anyhow::Result<()> {
    let mut child = Command::new(host)
        .args(["--listen", "grpc://127.0.0.1:0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("start {}", host.display()))?;
    let result = check_host_readiness(&mut child);
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn check_host_readiness(child: &mut Child) -> anyhow::Result<()> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("code-mode host stdout was not piped"))?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = sender.send(result);
    });
    let listen_url = receiver
        .recv_timeout(HOST_START_TIMEOUT)
        .context("timed out waiting for code-mode host readiness address")??;
    let address = listen_url.trim().strip_prefix("http://").ok_or_else(|| {
        anyhow::anyhow!("code-mode host returned invalid address: {listen_url:?}")
    })?;
    let mut stream = TcpStream::connect(address).context("connect to code-mode host")?;
    stream.set_read_timeout(Some(HOST_START_TIMEOUT))?;
    stream.set_write_timeout(Some(HOST_START_TIMEOUT))?;
    write!(
        stream,
        "GET /healthz HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )?;
    let mut status_line = String::new();
    BufReader::new(stream).read_line(&mut status_line)?;
    if !status_line.starts_with("HTTP/1.1 200") {
        anyhow::bail!("code-mode host readiness check failed: {status_line:?}");
    }
    Ok(())
}

fn ensure_visible_link(layout: &SourceInstallLayout) -> anyhow::Result<()> {
    let bin_dir = layout
        .visible_bin
        .parent()
        .ok_or_else(|| anyhow::anyhow!("visible Codex command has no parent directory"))?;
    std::fs::create_dir_all(bin_dir)?;
    if std::fs::symlink_metadata(&layout.visible_bin).is_ok() {
        let expected = layout.current_link.join("bin/codex");
        if std::fs::read_link(&layout.visible_bin).ok().as_deref() != Some(expected.as_path()) {
            anyhow::bail!(
                "refusing to replace unmanaged Codex command at {}",
                layout.visible_bin.display()
            );
        }
        return Ok(());
    }
    replace_symlink(&layout.visible_bin, &layout.current_link.join("bin/codex"))
}

fn ensure_visible_link_available(layout: &SourceInstallLayout) -> anyhow::Result<()> {
    if std::fs::symlink_metadata(&layout.visible_bin).is_err() {
        return Ok(());
    }
    let expected = layout.current_link.join("bin/codex");
    if std::fs::read_link(&layout.visible_bin).ok().as_deref() == Some(expected.as_path()) {
        Ok(())
    } else {
        anyhow::bail!(
            "refusing to replace unmanaged Codex command at {}",
            layout.visible_bin.display()
        )
    }
}

fn switch_current_release(layout: &SourceInstallLayout, release_dir: &Path) -> anyhow::Result<()> {
    replace_symlink(&layout.current_link, release_dir)
}

fn rollback_current_release(
    layout: &SourceInstallLayout,
    previous_release: Option<&Path>,
) -> anyhow::Result<()> {
    if let Some(previous_release) = previous_release {
        replace_symlink(&layout.current_link, previous_release)
    } else {
        match std::fs::remove_file(&layout.current_link) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("remove failed source release link"),
        }
    }
}

fn rollback_failed_install(
    layout: &SourceInstallLayout,
    previous_release: Option<&Path>,
    install_error: anyhow::Error,
) -> anyhow::Result<()> {
    rollback_current_release(layout, previous_release)
        .with_context(|| format!("rollback failed after installation error: {install_error:#}"))?;
    std::fs::remove_dir_all(&layout.release_dir).context("remove failed source release")?;
    Err(install_error.context("source package activation rolled back"))
}

fn replace_symlink(link: &Path, target: &Path) -> anyhow::Result<()> {
    let parent = link
        .parent()
        .ok_or_else(|| anyhow::anyhow!("symlink path has no parent: {}", link.display()))?;
    std::fs::create_dir_all(parent)?;
    let temp_link = parent.join(format!(
        ".{}.{}",
        link.file_name().and_then(OsStr::to_str).unwrap_or("link"),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&temp_link);
    std::os::unix::fs::symlink(target, &temp_link)?;
    if let Err(error) = std::fs::rename(&temp_link, link) {
        let _ = std::fs::remove_file(&temp_link);
        return Err(error).with_context(|| format!("replace symlink {}", link.display()));
    }
    Ok(())
}

fn prune_old_releases(layout: &SourceInstallLayout) -> anyhow::Result<()> {
    let mut releases = std::fs::read_dir(&layout.releases_dir)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_dir()))
        .collect::<Vec<_>>();
    releases.sort_by_key(std::fs::DirEntry::file_name);
    let remove_count = releases.len().saturating_sub(MAX_RETAINED_RELEASES);
    for release in releases
        .into_iter()
        .filter(|release| release.path() != layout.release_dir)
        .take(remove_count)
    {
        std::fs::remove_dir_all(release.path())?;
    }
    Ok(())
}

fn create_lock_file(path: &Path) -> std::io::Result<File> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    writeln!(file, "{}", std::process::id())?;
    Ok(file)
}

fn lock_owner_is_running(path: &Path) -> bool {
    let Ok(pid) = std::fs::read_to_string(path) else {
        return false;
    };
    Command::new("kill")
        .args(["-0", pid.trim()])
        .status()
        .is_ok_and(|status| status.success())
}

fn source_release_name(target: &str) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("source-{timestamp}-{}-{target}", std::process::id())
}

#[cfg(test)]
#[path = "source_update_install_tests.rs"]
mod tests;
