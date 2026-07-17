use std::path::Path;
#[cfg(not(windows))]
use std::process::Command;
#[cfg(not(windows))]
use std::time::SystemTime;
#[cfg(not(windows))]
use std::time::UNIX_EPOCH;

pub(crate) fn run_source_git_update(build_dir: &Path, latest_version: &str) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let _ = (build_dir, latest_version);
        anyhow::bail!(
            "Source-build auto-update is not supported on Windows yet. Fetch upstream changes and rebuild Codex manually."
        );
    }

    #[cfg(not(windows))]
    {
        println!();
        println!(
            "Updating Codex from local source checkout at {}...",
            build_dir.display()
        );
        ensure_source_checkout_clean(build_dir)?;
        sync_source_checkout(build_dir)?;
        let display_version = source_update_display_version(latest_version);
        run_checked_command(
            Command::new("cargo")
                .arg("build")
                .arg("--release")
                .arg("--bin")
                .arg("codex")
                .current_dir(build_dir.join("codex-rs"))
                .env("CODEX_CLI_DISPLAY_VERSION", display_version)
                .env("CODEX_CLI_BUILD_DIR", build_dir)
                .env("CODEX_CLI_UPDATE_BASE_VERSION", latest_version),
            "cargo build --release --bin codex",
        )?;
        install_source_built_binary(build_dir)?;
        println!("\n🎉 Source update ran successfully! Please restart Codex.");
        Ok(())
    }
}

#[cfg(not(windows))]
fn ensure_source_checkout_clean(build_dir: &Path) -> anyhow::Result<()> {
    let output = Command::new("git")
        .arg("status")
        .arg("--porcelain")
        .current_dir(build_dir)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "`git status --porcelain` failed with status {}",
            output.status
        );
    }
    if !output.stdout.is_empty() {
        anyhow::bail!(
            "Source checkout at {} has uncommitted changes. Commit, stash, or discard them before updating.",
            build_dir.display()
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn sync_source_checkout(build_dir: &Path) -> anyhow::Result<()> {
    if has_upstream_remote(build_dir)? {
        run_checked_command(
            Command::new("git")
                .arg("fetch")
                .arg("upstream")
                .arg("main")
                .current_dir(build_dir),
            "git fetch upstream main",
        )?;
        merge_upstream_main(build_dir)
    } else {
        run_checked_command(
            Command::new("git")
                .arg("pull")
                .arg("--ff-only")
                .current_dir(build_dir),
            "git pull --ff-only",
        )
    }
}

#[cfg(not(windows))]
fn has_upstream_remote(build_dir: &Path) -> anyhow::Result<bool> {
    let output = Command::new("git")
        .arg("remote")
        .arg("get-url")
        .arg("upstream")
        .current_dir(build_dir)
        .output()?;
    Ok(output.status.success())
}

#[cfg(not(windows))]
fn merge_upstream_main(build_dir: &Path) -> anyhow::Result<()> {
    let label = "git merge --no-edit upstream/main";
    println!("Running `{label}`...");
    let status = Command::new("git")
        .arg("merge")
        .arg("--no-edit")
        .arg("upstream/main")
        .current_dir(build_dir)
        .status()?;
    if status.success() {
        return Ok(());
    }

    let _ = Command::new("git")
        .arg("merge")
        .arg("--abort")
        .current_dir(build_dir)
        .status();
    anyhow::bail!(
        "`{label}` failed with status {status}. The merge was aborted. Resolve upstream changes manually before updating Codex."
    );
}

#[cfg(not(windows))]
fn run_checked_command(command: &mut Command, label: &str) -> anyhow::Result<()> {
    println!("Running `{label}`...");
    let status = command.status()?;
    if !status.success() {
        anyhow::bail!("`{label}` failed with status {status}");
    }
    Ok(())
}

#[cfg(not(windows))]
fn source_update_display_version(latest_version: &str) -> String {
    Command::new("date")
        .arg("+%Y.%m.%d")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|date| format!("{} local", date.trim()))
        .filter(|version| version != " local")
        .unwrap_or_else(|| format!("{latest_version} local"))
}

#[cfg(not(windows))]
fn install_source_built_binary(build_dir: &Path) -> anyhow::Result<()> {
    let source_bin = build_dir.join("codex-rs").join("target/release/codex");
    let installed_bin = std::env::current_exe()?;
    let installed_dir = installed_bin
        .parent()
        .ok_or_else(|| anyhow::anyhow!("current executable has no parent directory"))?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let backup = installed_bin.with_file_name(format!(
        "{}.bak.{timestamp}",
        installed_bin
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("codex")
    ));
    let temp = installed_dir.join(format!(".codex-update-{timestamp}"));

    std::fs::copy(&installed_bin, &backup)?;
    std::fs::copy(&source_bin, &temp)?;
    let mut permissions = std::fs::metadata(&temp)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
    }
    std::fs::set_permissions(&temp, permissions)?;
    std::fs::rename(&temp, &installed_bin)?;
    println!("Installed updated binary to {}", installed_bin.display());
    println!("Previous binary backed up to {}", backup.display());
    Ok(())
}
