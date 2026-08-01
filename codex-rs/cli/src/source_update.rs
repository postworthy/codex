use std::path::Path;
#[cfg(not(windows))]
use std::process::Command;

#[cfg(not(windows))]
#[path = "source_update_install.rs"]
mod install;

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
        if let Err(initial_error) = try_source_git_update(build_dir, latest_version) {
            println!("\nAutomatic source update failed: {initial_error:#}");
            println!(
                "Launching Codex in {} to repair the update...",
                build_dir.display()
            );
            run_agent_assisted_update(build_dir, latest_version, &initial_error).map_err(
                |fallback_error| {
                    anyhow::anyhow!(
                        "agent-assisted source update failed: {fallback_error:#}. Initial automatic update failure: {initial_error:#}"
                    )
                },
            )?;
            println!("\nRetrying the source update after agent-assisted recovery...");
            try_source_git_update(build_dir, latest_version).map_err(|retry_error| {
                anyhow::anyhow!(
                    "source update still failed after agent-assisted recovery: {retry_error:#}. Initial failure: {initial_error:#}"
                )
            })?;
        }
        println!("\n🎉 Source update ran successfully! Please restart Codex.");
        Ok(())
    }
}

#[cfg(not(windows))]
fn try_source_git_update(build_dir: &Path, latest_version: &str) -> anyhow::Result<()> {
    ensure_source_checkout_clean(build_dir)?;
    sync_source_checkout(build_dir)?;
    let display_version = source_update_display_version(latest_version);
    let target = source_build_target()?;
    let layout = install::SourceInstallLayout::for_environment(&target)?;
    let _lock = install::SourceUpdateLock::acquire(&layout)?;
    install::prepare_staging_dir(&layout)?;
    let system_bwrap = target
        .contains("linux")
        .then(codex_sandboxing::find_system_bwrap_in_path)
        .flatten();
    let build_result = run_checked_command(
        source_package_build_command(
            build_dir,
            layout.staging_dir(),
            &target,
            system_bwrap.as_deref(),
        )
        .env("CODEX_CLI_DISPLAY_VERSION", display_version)
        .env("CODEX_CLI_BUILD_DIR", build_dir)
        .env("CODEX_CLI_UPDATE_BASE_VERSION", latest_version),
        "build Codex source package",
    );
    if let Err(error) = build_result {
        install::discard_staging_dir(&layout);
        return Err(error);
    }
    install::install_source_package(&layout, &target)
}

#[cfg(not(windows))]
fn source_package_build_command(
    build_dir: &Path,
    package_dir: &Path,
    target: &str,
    system_bwrap: Option<&Path>,
) -> Command {
    let mut command = Command::new("python3");
    command
        .arg(build_dir.join("scripts/build_codex_package.py"))
        .arg("--target")
        .arg(target)
        .arg("--cargo-profile")
        .arg("release")
        .arg("--package-dir")
        .arg(package_dir)
        .arg("--force")
        .current_dir(build_dir);
    if let Some(system_bwrap) = system_bwrap {
        command.arg("--bwrap-bin").arg(system_bwrap);
    }
    command
}

#[cfg(not(windows))]
fn source_build_target() -> anyhow::Result<String> {
    let output = Command::new("rustc").arg("-vV").output()?;
    if !output.status.success() {
        anyhow::bail!("`rustc -vV` failed with status {}", output.status);
    }
    let version = String::from_utf8(output.stdout)?;
    version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("`rustc -vV` output did not include a host target"))
}

#[cfg(not(windows))]
fn run_agent_assisted_update(
    build_dir: &Path,
    latest_version: &str,
    failure: &anyhow::Error,
) -> anyhow::Result<()> {
    let codex_bin = std::env::current_exe()?;
    let mut command =
        agent_assisted_update_command(codex_bin.as_path(), build_dir, latest_version, failure);
    let status = command.status()?;
    if !status.success() {
        anyhow::bail!("agent-assisted source update exited with status {status}");
    }
    Ok(())
}

#[cfg(not(windows))]
fn agent_assisted_update_command(
    codex_bin: &Path,
    build_dir: &Path,
    latest_version: &str,
    failure: &anyhow::Error,
) -> Command {
    let full_failure_detail = format!("{failure:#}");
    let failure_was_truncated = full_failure_detail.chars().count() > 4_000;
    let mut failure_detail: String = full_failure_detail.chars().take(4_000).collect();
    if failure_was_truncated {
        failure_detail.push('…');
    }
    let prompt = format!(
        "Automatic source update failed and needs agent-assisted recovery.\n\nRepository: {}\nTarget Codex release: {latest_version}\nFailure: {failure_detail}\n\nPerform the update repair now. Inspect the repository state, preserve fork-specific changes and unrelated user work, merge upstream/main, resolve any conflicts, and commit the merge if required. Run the repository-required formatting and relevant tests. Leave the checkout clean and ready for the updater to retry the release build and installation after this session exits. Do not merely explain the steps; perform the work. The failed automatic merge, if any, was aborted before this session started.",
        build_dir.display()
    );
    let mut command = Command::new(codex_bin);
    command
        .arg("exec")
        .arg("--cd")
        .arg(build_dir)
        .arg("-c")
        .arg("check_for_update_on_startup=false")
        .arg(prompt)
        .current_dir(build_dir);
    command
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

    let conflicted_paths = Command::new("git")
        .args(["diff", "--name-only", "--diff-filter=U"])
        .current_dir(build_dir)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|paths| paths.trim().chars().take(4_000).collect::<String>())
        .filter(|paths| !paths.is_empty());
    let _ = Command::new("git")
        .arg("merge")
        .arg("--abort")
        .current_dir(build_dir)
        .status();
    let conflict_detail = conflicted_paths
        .map(|paths| format!(" Conflicted paths before abort:\n{paths}"))
        .unwrap_or_default();
    anyhow::bail!("`{label}` failed with status {status}. The merge was aborted.{conflict_detail}");
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

#[cfg(all(test, not(windows)))]
#[path = "source_update_tests.rs"]
mod tests;
