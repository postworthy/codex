use std::path::PathBuf;

#[cfg(any(not(debug_assertions), test))]
use codex_install_context::InstallContext;
#[cfg(any(not(debug_assertions), test))]
use codex_install_context::InstallMethod;
#[cfg(any(not(debug_assertions), test))]
use codex_install_context::StandalonePlatform;

/// Update action the CLI should perform after the TUI exits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateAction {
    /// Update via `npm install -g @openai/codex@latest`.
    NpmGlobalLatest,
    /// Update via `bun install -g @openai/codex@latest`.
    BunGlobalLatest,
    /// Update via `brew upgrade codex`.
    BrewUpgrade,
    /// Update via `curl -fsSL https://chatgpt.com/codex/install.sh | CODEX_NON_INTERACTIVE=1 sh`.
    StandaloneUnix,
    /// Update via `$env:CODEX_NON_INTERACTIVE=1; irm https://chatgpt.com/codex/install.ps1 | iex`.
    StandaloneWindows,
    /// Update a local source checkout, rebuild it, and reinstall this binary.
    SourceGit {
        build_dir: PathBuf,
        latest_version: String,
    },
}

impl UpdateAction {
    #[cfg(any(not(debug_assertions), test))]
    pub(crate) fn from_install_context(context: &InstallContext) -> Option<Self> {
        match &context.method {
            InstallMethod::Npm => Some(UpdateAction::NpmGlobalLatest),
            InstallMethod::Bun => Some(UpdateAction::BunGlobalLatest),
            InstallMethod::Brew => Some(UpdateAction::BrewUpgrade),
            InstallMethod::Standalone { platform, .. } => Some(match platform {
                StandalonePlatform::Unix => UpdateAction::StandaloneUnix,
                StandalonePlatform::Windows => UpdateAction::StandaloneWindows,
            }),
            InstallMethod::Other => None,
        }
    }

    /// Returns the list of command-line arguments for invoking the update.
    pub fn command_args(&self) -> Option<(&'static str, &'static [&'static str])> {
        match self {
            UpdateAction::NpmGlobalLatest => Some(("npm", &["install", "-g", "@openai/codex"])),
            UpdateAction::BunGlobalLatest => Some(("bun", &["install", "-g", "@openai/codex"])),
            UpdateAction::BrewUpgrade => Some(("brew", &["upgrade", "--cask", "codex"])),
            UpdateAction::StandaloneUnix => Some((
                "sh",
                &[
                    "-c",
                    "curl -fsSL https://chatgpt.com/codex/install.sh | CODEX_NON_INTERACTIVE=1 sh",
                ],
            )),
            UpdateAction::StandaloneWindows => Some((
                "powershell",
                &[
                    "-ExecutionPolicy",
                    "Bypass",
                    "-c",
                    "$env:CODEX_NON_INTERACTIVE=1; irm https://chatgpt.com/codex/install.ps1 | iex",
                ],
            )),
            UpdateAction::SourceGit { .. } => None,
        }
    }

    /// Returns string representation of the command-line arguments for invoking the update.
    pub fn command_str(&self) -> String {
        if let Some((command, args)) = self.command_args() {
            return shlex::try_join(std::iter::once(command).chain(args.iter().copied()))
                .unwrap_or_else(|_| format!("{command} {}", args.join(" ")));
        }

        match self {
            UpdateAction::SourceGit {
                build_dir,
                latest_version,
            } => {
                let build_dir = build_dir.display();
                format!(
                    "cd {build_dir} && git pull --ff-only && CODEX_CLI_UPDATE_BASE_VERSION={latest_version} cargo build --release --bin codex"
                )
            }
            _ => unreachable!("non-source update actions have static command args"),
        }
    }

    pub fn is_standalone_windows(&self) -> bool {
        matches!(self, UpdateAction::StandaloneWindows)
    }
}

#[cfg(not(debug_assertions))]
pub fn get_update_action() -> Option<UpdateAction> {
    if let Some(source_action) = get_source_git_update_action(/*latest_version*/ None) {
        return Some(source_action);
    }
    UpdateAction::from_install_context(InstallContext::current())
}

#[cfg(not(debug_assertions))]
pub(crate) fn get_update_action_for_version(latest_version: &str) -> Option<UpdateAction> {
    if let Some(source_action) = get_source_git_update_action(Some(latest_version)) {
        return Some(source_action);
    }
    get_update_action()
}

#[cfg(not(debug_assertions))]
fn get_source_git_update_action(latest_version: Option<&str>) -> Option<UpdateAction> {
    let build_dir = crate::version::CODEX_CLI_BUILD_DIR?;
    Some(UpdateAction::SourceGit {
        build_dir: PathBuf::from(build_dir),
        latest_version: latest_version
            .or(crate::version::CODEX_CLI_UPDATE_BASE_VERSION)
            .unwrap_or("unknown")
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;

    #[test]
    fn maps_install_context_to_update_action() {
        let native_release_dir =
            AbsolutePathBuf::from_absolute_path(std::env::temp_dir().join("native-release"))
                .expect("temp dir path should be absolute");

        assert_eq!(
            UpdateAction::from_install_context(&InstallContext {
                method: InstallMethod::Other,
                package_layout: None,
            }),
            None
        );
        assert_eq!(
            UpdateAction::from_install_context(&InstallContext {
                method: InstallMethod::Npm,
                package_layout: None,
            }),
            Some(UpdateAction::NpmGlobalLatest)
        );
        assert_eq!(
            UpdateAction::from_install_context(&InstallContext {
                method: InstallMethod::Bun,
                package_layout: None,
            }),
            Some(UpdateAction::BunGlobalLatest)
        );
        assert_eq!(
            UpdateAction::from_install_context(&InstallContext {
                method: InstallMethod::Brew,
                package_layout: None,
            }),
            Some(UpdateAction::BrewUpgrade)
        );
        assert_eq!(
            UpdateAction::from_install_context(&InstallContext {
                method: InstallMethod::Standalone {
                    platform: StandalonePlatform::Unix,
                    release_dir: native_release_dir.clone(),
                    resources_dir: Some(native_release_dir.join("codex-resources")),
                },
                package_layout: None,
            }),
            Some(UpdateAction::StandaloneUnix)
        );
        assert_eq!(
            UpdateAction::from_install_context(&InstallContext {
                method: InstallMethod::Standalone {
                    platform: StandalonePlatform::Windows,
                    release_dir: native_release_dir.clone(),
                    resources_dir: Some(native_release_dir.join("codex-resources")),
                },
                package_layout: None,
            }),
            Some(UpdateAction::StandaloneWindows)
        );
    }

    #[test]
    fn standalone_update_commands_rerun_latest_installer() {
        assert_eq!(
            UpdateAction::StandaloneUnix.command_args(),
            Some((
                "sh",
                &[
                    "-c",
                    "curl -fsSL https://chatgpt.com/codex/install.sh | CODEX_NON_INTERACTIVE=1 sh"
                ][..],
            ))
        );
        assert_eq!(
            UpdateAction::StandaloneWindows.command_args(),
            Some((
                "powershell",
                &[
                    "-ExecutionPolicy",
                    "Bypass",
                    "-c",
                    "$env:CODEX_NON_INTERACTIVE=1; irm https://chatgpt.com/codex/install.ps1 | iex"
                ][..],
            ))
        );
    }

    #[test]
    fn source_git_update_has_local_rebuild_command_text() {
        let action = UpdateAction::SourceGit {
            build_dir: PathBuf::from("/tmp/codex"),
            latest_version: "0.136.0".to_string(),
        };

        assert_eq!(action.command_args(), None);
        assert_eq!(
            action.command_str(),
            "cd /tmp/codex && git pull --ff-only && CODEX_CLI_UPDATE_BASE_VERSION=0.136.0 cargo build --release --bin codex"
        );
    }
}
