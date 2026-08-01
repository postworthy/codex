use std::ffi::OsString;
use std::path::Path;

use pretty_assertions::assert_eq;

use super::agent_assisted_update_command;

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
