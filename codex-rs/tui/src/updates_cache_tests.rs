use super::*;
use crate::legacy_core::config::ConfigBuilder;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[tokio::test]
async fn dismiss_version_creates_cache_file_when_missing() {
    let codex_home = tempdir().expect("temp codex home");
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await
        .expect("load config");
    let version_file = version_filepath(&config);

    dismiss_version(&config, "999.0.0")
        .await
        .expect("dismiss version");

    let info = read_version_info(&version_file).expect("read version info");
    assert_eq!(info.last_checked_at, DateTime::<Utc>::UNIX_EPOCH);
    assert_eq!(
        (
            info.latest_version.as_str(),
            info.dismissed_version.as_deref()
        ),
        ("999.0.0", Some("999.0.0"))
    );
}

#[test]
fn reads_latest_version_from_codex_home() {
    let codex_home = tempdir().expect("temp codex home");
    std::fs::write(
        codex_home.path().join(VERSION_FILENAME),
        r#"{"latest_version":"0.150.0","last_checked_at":"2026-08-27T00:00:00Z","dismissed_version":null}"#,
    )
    .expect("write version cache");

    assert_eq!(
        read_latest_version(codex_home.path()),
        Some("0.150.0".to_string())
    );
}
