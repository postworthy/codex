use crate::LogEntry;
use crate::LogQuery;
use crate::LogRow;
use crate::SortKey;
use crate::SqliteConfig;
use crate::ThreadMetadata;
use crate::ThreadMetadataBuilder;
use crate::ThreadsPage;
use crate::apply_rollout_item;
use crate::migrations::runtime_goals_migrator;
use crate::migrations::runtime_logs_migrator;
use crate::migrations::runtime_memories_migrator;
use crate::migrations::runtime_state_migrator;
use crate::migrations::runtime_thread_history_migrator;
use crate::model::ThreadRow;
use crate::model::anchor_from_item;
use crate::model::datetime_to_epoch_millis;
use crate::model::datetime_to_epoch_seconds;
use crate::model::epoch_millis_to_datetime;
use crate::paths::file_modified_time_utc;
use crate::telemetry::DbKind;
use crate::telemetry::DbTelemetry;
use anyhow::Context;
use chrono::DateTime;
use chrono::Utc;
use codex_protocol::ThreadId;
use codex_protocol::protocol::RolloutItem;
use serde_json::Value;
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::SqliteConnection;
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::time::Instant;
use tracing::warn;

mod backfill;
mod external_agent_config_imports;
mod goals;
mod logs;
mod memories;
mod recovery;
mod remote_control;
#[cfg(test)]
pub(crate) mod test_support;
mod thread_section_order;
mod thread_sections;
mod threads;

pub use external_agent_config_imports::ExternalAgentConfigImportDetailsRecord;
pub use external_agent_config_imports::ExternalAgentConfigImportFailureRecord;
pub use external_agent_config_imports::ExternalAgentConfigImportHistoryRecord;
pub use external_agent_config_imports::ExternalAgentConfigImportSuccessRecord;
pub use goals::GoalAccountingMode;
pub use goals::GoalAccountingOutcome;
pub use goals::GoalStore;
pub use goals::GoalUpdate;
pub use memories::MemoryStore;
pub use recovery::RuntimeDbBackup;
pub(super) use recovery::RuntimeDbInitError;
pub use recovery::backup_runtime_db_for_fresh_start;
pub use recovery::is_sqlite_corruption_error;
pub use recovery::runtime_db_path_for_corruption_error;
pub use recovery::sqlite_error_detail_is_corruption;
pub use recovery::sqlite_error_detail_is_lock;
pub use remote_control::RemoteControlEnrollmentRecord;
pub use threads::ThreadFilterOptions;

// "Partition" is the retained-log-content bucket we cap at 10 MiB:
// - one bucket per non-null thread_id
// - one bucket per threadless (thread_id IS NULL) non-null process_uuid
// - one bucket for threadless rows with process_uuid IS NULL
// This budget tracks each row's persisted rendered log body plus non-body
// metadata, rather than the exact sum of all persisted SQLite column bytes.
const LOG_PARTITION_SIZE_LIMIT_BYTES: i64 = 10 * 1024 * 1024;
const LOG_PARTITION_ROW_LIMIT: i64 = 1_000;

#[derive(Clone)]
pub struct StateRuntime {
    sqlite: SqliteConfig,
    default_provider: String,
    pool: Arc<sqlx::SqlitePool>,
    logs_pool: Arc<sqlx::SqlitePool>,
    thread_goals: GoalStore,
    memories: MemoryStore,
    thread_updated_at_millis: Arc<AtomicI64>,
    thread_recency_at_millis: Arc<AtomicI64>,
}

impl StateRuntime {
    /// Initialize the state runtime using the provided SQLite configuration and default provider.
    ///
    /// This opens (and migrates) the SQLite databases under the configured
    /// `sqlite_home`.
    /// Logs and paginated thread history live in dedicated files to reduce
    /// lock contention with the rest of the state store.
    pub async fn init(sqlite: SqliteConfig, default_provider: String) -> anyhow::Result<Arc<Self>> {
        Self::init_inner(sqlite, default_provider, /*telemetry_override*/ None).await
    }

    #[cfg(test)]
    pub(crate) async fn init_with_telemetry_for_tests(
        sqlite: SqliteConfig,
        default_provider: String,
        telemetry_override: &dyn DbTelemetry,
    ) -> anyhow::Result<Arc<Self>> {
        Self::init_inner(sqlite, default_provider, Some(telemetry_override)).await
    }

    async fn init_inner(
        sqlite: SqliteConfig,
        default_provider: String,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<Arc<Self>> {
        tokio::fs::create_dir_all(sqlite.home()).await?;
        let state_migrator = runtime_state_migrator();
        let logs_migrator = runtime_logs_migrator();
        let goals_migrator = runtime_goals_migrator();
        let memories_migrator = runtime_memories_migrator();
        let state_path = sqlite.state_db_path();
        let logs_path = sqlite.logs_db_path();
        let goals_path = sqlite.goals_db_path();
        let memories_path = sqlite.memories_db_path();
        let pool = match sqlite
            .open_state_db(&state_migrator, telemetry_override)
            .await
        {
            Ok(db) => Arc::new(db),
            Err(err) => {
                warn!("failed to open state db at {}: {err}", state_path.display());
                return Err(err);
            }
        };
        let logs_pool = match open_logs_sqlite_with_recovery(
            &sqlite,
            &logs_path,
            &logs_migrator,
            telemetry_override,
        )
        .await
        {
            Ok(db) => Arc::new(db),
            Err(err) => {
                warn!("failed to open logs db at {}: {err}", logs_path.display());
                close_sqlite_pools(&[pool.as_ref()]).await;
                return Err(err);
            }
        };
        let goals_pool = match sqlite
            .open_goals_db(&goals_migrator, telemetry_override)
            .await
        {
            Ok(db) => Arc::new(db),
            Err(err) => {
                warn!("failed to open goals db at {}: {err}", goals_path.display());
                close_sqlite_pools(&[pool.as_ref(), logs_pool.as_ref()]).await;
                return Err(err);
            }
        };
        let memories_pool = match sqlite
            .open_memories_db(&memories_migrator, telemetry_override)
            .await
        {
            Ok(db) => Arc::new(db),
            Err(err) => {
                warn!(
                    "failed to open memories db at {}: {err}",
                    memories_path.display()
                );
                close_sqlite_pools(&[pool.as_ref(), logs_pool.as_ref(), goals_pool.as_ref()]).await;
                return Err(err);
            }
        };
        let started = Instant::now();
        let backfill_state_result = ensure_backfill_state_row_in_pool(pool.as_ref()).await;
        crate::telemetry::record_init_result(
            telemetry_override,
            DbKind::State,
            "ensure_backfill_state",
            started.elapsed(),
            &backfill_state_result,
        );
        if let Err(err) = backfill_state_result {
            close_sqlite_pools(&[
                pool.as_ref(),
                logs_pool.as_ref(),
                goals_pool.as_ref(),
                memories_pool.as_ref(),
            ])
            .await;
            return Err(err);
        }
        let started = Instant::now();
        let thread_timestamp_millis_result: anyhow::Result<(Option<i64>, Option<i64>)> =
            sqlx::query_as(
                "SELECT MAX(threads.updated_at_ms), MAX(threads.recency_at_ms) FROM threads",
            )
            .fetch_one(pool.as_ref())
            .await
            .map_err(anyhow::Error::from);
        crate::telemetry::record_init_result(
            telemetry_override,
            DbKind::State,
            "post_init_query",
            started.elapsed(),
            &thread_timestamp_millis_result,
        );
        let (thread_updated_at_millis, thread_recency_at_millis) =
            match thread_timestamp_millis_result {
                Ok(value) => value,
                Err(err) => {
                    close_sqlite_pools(&[
                        pool.as_ref(),
                        logs_pool.as_ref(),
                        goals_pool.as_ref(),
                        memories_pool.as_ref(),
                    ])
                    .await;
                    return Err(err);
                }
            };
        let thread_updated_at_millis = thread_updated_at_millis.unwrap_or(0);
        let thread_recency_at_millis = thread_recency_at_millis.unwrap_or(0);
        let runtime = Arc::new(Self {
            thread_goals: GoalStore::new(Arc::clone(&goals_pool)),
            memories: MemoryStore::new(Arc::clone(&memories_pool), Arc::clone(&pool)),
            pool,
            logs_pool,
            sqlite,
            default_provider,
            thread_updated_at_millis: Arc::new(AtomicI64::new(thread_updated_at_millis)),
            thread_recency_at_millis: Arc::new(AtomicI64::new(thread_recency_at_millis)),
        });
        if let Err(err) = runtime.run_logs_startup_maintenance().await {
            warn!(
                "failed to run startup maintenance for logs db at {}: {err}",
                logs_path.display(),
            );
        }
        Ok(runtime)
    }

    /// Return the SQLite configuration for this runtime.
    pub fn sqlite(&self) -> &SqliteConfig {
        &self.sqlite
    }

    pub fn thread_goals(&self) -> &GoalStore {
        &self.thread_goals
    }

    pub fn memories(&self) -> &MemoryStore {
        &self.memories
    }

    /// Close all SQLite pools and wait for outstanding pool workers to exit.
    pub async fn close(&self) {
        self.memories.close().await;
        self.thread_goals.close().await;
        self.logs_pool.close().await;
        self.pool.close().await;
    }

    pub async fn clear_memory_data_in_sqlite_home(sqlite: &SqliteConfig) -> anyhow::Result<bool> {
        let memories_path = sqlite.memories_db_path();
        if !tokio::fs::try_exists(&memories_path).await? {
            return Ok(false);
        }

        let memories_migrator = runtime_memories_migrator();
        let pool = sqlite
            .open_memories_db(&memories_migrator, /*telemetry_override*/ None)
            .await?;
        memories::clear_memory_data_in_pool(&pool).await?;
        pool.close().await;
        Ok(true)
    }
}

async fn close_sqlite_pools(pools: &[&SqlitePool]) {
    for pool in pools {
        pool.close().await;
    }
}

async fn open_logs_sqlite_with_recovery(
    sqlite: &SqliteConfig,
    path: &Path,
    migrator: &Migrator,
    telemetry_override: Option<&dyn DbTelemetry>,
) -> anyhow::Result<SqlitePool> {
    match sqlite.open_logs_db(migrator, telemetry_override).await {
        Ok(pool) => Ok(pool),
        Err(err) if is_recoverable_logs_db_error(&err) => {
            warn!(
                "logs db at {} is corrupt; moving it aside and recreating it",
                path.display()
            );
            quarantine_logs_sqlite_files(path)
                .await
                .with_context(|| format!("failed to quarantine logs db at {}", path.display()))?;
            sqlite
                .open_logs_db(migrator, telemetry_override)
                .await
                .with_context(|| format!("failed to recreate logs db at {}", path.display()))
        }
        Err(err) => Err(err),
    }
}

fn is_recoverable_logs_db_error(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(sqlx_err) = cause.downcast_ref::<sqlx::Error>()
            && is_recoverable_sqlite_error(sqlx_err)
        {
            return true;
        }
    }

    let message = err.to_string().to_ascii_lowercase();
    message.contains("file is not a database")
        || message.contains("database disk image is malformed")
}

fn is_recoverable_sqlite_error(err: &sqlx::Error) -> bool {
    let sqlx::Error::Database(database_error) = err else {
        return false;
    };
    let primary_code = database_error
        .code()
        .and_then(|code| code.parse::<i32>().ok())
        .map(|code| code & 0xff);
    let message = database_error.message().to_ascii_lowercase();
    matches!(primary_code, Some(11 | 26))
        || message.contains("file is not a database")
        || message.contains("database disk image is malformed")
}

async fn quarantine_logs_sqlite_files(path: &Path) -> std::io::Result<Vec<PathBuf>> {
    let suffix = format!(".corrupt-{}", Utc::now().format("%Y%m%d-%H%M%S"));
    let mut moved = Vec::new();
    for path in sqlite_path_triplet(path) {
        if tokio::fs::try_exists(path.as_path()).await? {
            moved.push(quarantine_path(path.as_path(), suffix.as_str()).await?);
        }
    }
    Ok(moved)
}

fn sqlite_path_triplet(path: &Path) -> Vec<PathBuf> {
    let mut wal_path = path.as_os_str().to_os_string();
    wal_path.push("-wal");
    let mut shm_path = path.as_os_str().to_os_string();
    shm_path.push("-shm");
    vec![
        path.to_path_buf(),
        PathBuf::from(wal_path),
        PathBuf::from(shm_path),
    ]
}

async fn quarantine_path(path: &Path, suffix: &str) -> std::io::Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::other(format!(
            "cannot create a quarantine name for {}",
            path.display()
        ))
    })?;
    let mut sequence = 0;
    loop {
        let mut quarantine_name = file_name.to_os_string();
        quarantine_name.push(suffix);
        if sequence > 0 {
            quarantine_name.push(format!(".{sequence}"));
        }
        let quarantine_path = path.with_file_name(quarantine_name);
        if !tokio::fs::try_exists(quarantine_path.as_path()).await? {
            tokio::fs::rename(path, quarantine_path.as_path()).await?;
            return Ok(quarantine_path);
        }
        sequence += 1;
    }
}

/// Open and migrate the rebuildable paginated thread-history database.
pub async fn open_thread_history_db(sqlite: &SqliteConfig) -> anyhow::Result<SqlitePool> {
    let migrator = runtime_thread_history_migrator();
    sqlite
        .open_thread_history_db(&migrator, /*telemetry_override*/ None)
        .await
}

pub(super) async fn ensure_backfill_state_row_in_pool(
    pool: &sqlx::SqlitePool,
) -> anyhow::Result<()> {
    // Eagerly check if the operation would have no effect to avoid blocking waiting for a SQLite
    // writer for no reason in the hot startup path.
    if sqlx::query_scalar::<_, i64>("SELECT 1 FROM backfill_state WHERE id = 1")
        .fetch_optional(pool)
        .await?
        .is_some()
    {
        return Ok(());
    }

    sqlx::query(
        r#"
INSERT INTO backfill_state (id, status, last_watermark, last_success_at, updated_at)
VALUES (?, ?, NULL, NULL, ?)
ON CONFLICT(id) DO NOTHING
            "#,
    )
    .bind(1_i64)
    .bind(crate::BackfillStatus::Pending.as_str())
    .bind(Utc::now().timestamp())
    .execute(pool)
    .await?;
    Ok(())
}

/// Run SQLite's built-in integrity check against an existing database file.
pub async fn sqlite_integrity_check(
    sqlite: &SqliteConfig,
    path: &Path,
) -> anyhow::Result<Vec<String>> {
    let pool = sqlite.open_read_only_pool(path).await?;
    let rows = sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
        .fetch_all(&pool)
        .await?;
    pool.close().await;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::StateRuntime;
    use super::runtime_state_migrator;
    use super::sqlite_integrity_check;
    use super::test_support::unique_temp_dir;
    use crate::DB_INIT_METRIC;
    use crate::DbTelemetry;
    use crate::migrations::STATE_MIGRATOR;
    use codex_utils_absolute_path::test_support::PathExt;
    use pretty_assertions::assert_eq;
    use sqlx::SqlitePool;
    use sqlx::migrate::MigrateError;
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::sync::Mutex;

    #[derive(Default)]
    struct TestTelemetry {
        counters: Mutex<Vec<MetricEvent>>,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct MetricEvent {
        name: String,
        tags: BTreeMap<String, String>,
    }

    impl TestTelemetry {
        fn counters(&self) -> Vec<MetricEvent> {
            self.counters
                .lock()
                .expect("telemetry lock")
                .iter()
                .map(|event| MetricEvent {
                    name: event.name.clone(),
                    tags: event.tags.clone(),
                })
                .collect()
        }
    }

    impl DbTelemetry for TestTelemetry {
        fn counter(&self, name: &str, _inc: i64, tags: &[(&str, &str)]) {
            self.counters
                .lock()
                .expect("telemetry lock")
                .push(MetricEvent {
                    name: name.to_string(),
                    tags: tags_to_map(tags),
                });
        }

        fn record_duration(
            &self,
            _name: &str,
            _duration: std::time::Duration,
            _tags: &[(&str, &str)],
        ) {
        }
    }

    fn tags_to_map(tags: &[(&str, &str)]) -> BTreeMap<String, String> {
        tags.iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    async fn open_db_pool(path: &Path) -> SqlitePool {
        crate::SqliteConfig::new_for_testing(path.parent().unwrap_or(path).abs())
            .open_read_write_pool(path)
            .await
            .expect("open sqlite pool")
    }

    fn count_files_with_prefix(dir: &Path, prefix: &str) -> usize {
        std::fs::read_dir(dir)
            .expect("read temp dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
            .count()
    }

    #[tokio::test]
    async fn sqlite_integrity_check_reports_ok_for_valid_db() {
        let codex_home = unique_temp_dir();
        tokio::fs::create_dir_all(&codex_home)
            .await
            .expect("create codex home");
        let sqlite = crate::SqliteConfig::new_for_testing(codex_home.as_path().abs());
        let path = sqlite.state_db_path();
        let pool = sqlite
            .open_read_write_pool(&path)
            .await
            .expect("open sqlite db");
        sqlx::query("CREATE TABLE sample (id INTEGER PRIMARY KEY)")
            .execute(&pool)
            .await
            .expect("create sample table");
        pool.close().await;

        let result = sqlite_integrity_check(&sqlite, &path)
            .await
            .expect("integrity check should run");

        assert_eq!(result, vec!["ok".to_string()]);
        let _ = tokio::fs::remove_dir_all(codex_home).await;
    }

    #[tokio::test]
    async fn open_state_sqlite_tolerates_newer_applied_migrations() {
        let codex_home = unique_temp_dir();
        tokio::fs::create_dir_all(&codex_home)
            .await
            .expect("create codex home");
        let sqlite = crate::SqliteConfig::new_for_testing(codex_home.as_path().abs());
        let state_path = sqlite.state_db_path();
        let pool = sqlite
            .open_read_write_pool(&state_path)
            .await
            .expect("open state db");
        STATE_MIGRATOR
            .run(&pool)
            .await
            .expect("apply current state schema");
        sqlx::query(
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(9_999_i64)
        .bind("future migration")
        .bind(true)
        .bind(vec![1_u8, 2, 3, 4])
        .bind(1_i64)
        .execute(&pool)
        .await
        .expect("insert future migration record");
        pool.close().await;

        let strict_pool = open_db_pool(state_path.as_path()).await;
        let strict_err = STATE_MIGRATOR
            .run(&strict_pool)
            .await
            .expect_err("strict migrator should reject newer applied migrations");
        assert!(matches!(strict_err, MigrateError::VersionMissing(9_999)));
        strict_pool.close().await;

        let tolerant_migrator = runtime_state_migrator();
        let tolerant_pool = sqlite
            .open_state_db(&tolerant_migrator, /*telemetry_override*/ None)
            .await
            .expect("runtime migrator should tolerate newer applied migrations");
        tolerant_pool.close().await;

        let _ = tokio::fs::remove_dir_all(codex_home).await;
    }

    #[tokio::test]
    async fn init_quarantines_and_recreates_corrupt_logs_db() {
        let codex_home = unique_temp_dir();
        tokio::fs::create_dir_all(&codex_home)
            .await
            .expect("create codex home");
        let sqlite = crate::SqliteConfig::new_for_testing(codex_home.as_path().abs());
        let logs_path = sqlite.logs_db_path();
        let mut logs_wal_path = logs_path.as_os_str().to_os_string();
        logs_wal_path.push("-wal");
        let logs_wal_path = std::path::PathBuf::from(logs_wal_path);
        let mut logs_shm_path = logs_path.as_os_str().to_os_string();
        logs_shm_path.push("-shm");
        let logs_shm_path = std::path::PathBuf::from(logs_shm_path);
        tokio::fs::write(logs_path.as_path(), b"not sqlite")
            .await
            .expect("write corrupt logs db");
        tokio::fs::write(logs_wal_path.as_path(), b"not sqlite wal")
            .await
            .expect("write corrupt logs wal");
        tokio::fs::write(logs_shm_path.as_path(), b"not sqlite shm")
            .await
            .expect("write corrupt logs shm");

        let runtime = StateRuntime::init(sqlite.clone(), "test-provider".to_string())
            .await
            .expect("state runtime should recover corrupt logs db");

        assert!(tokio::fs::try_exists(logs_path.as_path()).await.unwrap());
        assert_eq!(
            sqlite_integrity_check(&sqlite, logs_path.as_path())
                .await
                .expect("integrity check should run"),
            vec!["ok".to_string()]
        );
        assert_eq!(
            count_files_with_prefix(codex_home.as_path(), "logs_2.sqlite.corrupt-"),
            1
        );

        runtime.close().await;
        let _ = tokio::fs::remove_dir_all(codex_home).await;
    }

    #[tokio::test]
    async fn init_does_not_quarantine_corrupt_state_db() {
        let codex_home = unique_temp_dir();
        tokio::fs::create_dir_all(&codex_home)
            .await
            .expect("create codex home");
        let sqlite = crate::SqliteConfig::new_for_testing(codex_home.as_path().abs());
        let state_path = sqlite.state_db_path();
        tokio::fs::write(state_path.as_path(), b"not sqlite")
            .await
            .expect("write corrupt state db");

        let result = StateRuntime::init(sqlite, "test-provider".to_string()).await;
        assert!(result.is_err(), "corrupt state db should remain fatal");

        assert!(tokio::fs::try_exists(state_path.as_path()).await.unwrap());
        assert_eq!(
            count_files_with_prefix(codex_home.as_path(), "state_5.sqlite.corrupt-"),
            0
        );
        assert_eq!(
            tokio::fs::read(state_path.as_path())
                .await
                .expect("read original state db"),
            b"not sqlite"
        );

        let _ = tokio::fs::remove_dir_all(codex_home).await;
    }

    #[tokio::test]
    async fn init_records_successful_sqlite_init_phases_to_explicit_telemetry() {
        let codex_home = unique_temp_dir();
        let telemetry = TestTelemetry::default();

        let runtime = StateRuntime::init_with_telemetry_for_tests(
            crate::SqliteConfig::new_for_testing(codex_home.as_path().abs()),
            "test-provider".to_string(),
            &telemetry,
        )
        .await
        .expect("state runtime should initialize");

        let phases = telemetry
            .counters()
            .into_iter()
            .filter(|event| event.name == DB_INIT_METRIC)
            .filter(|event| event.tags.get("status").map(String::as_str) == Some("success"))
            .filter_map(|event| event.tags.get("phase").cloned())
            .collect::<BTreeSet<_>>();
        let expected = [
            "open_state",
            "migrate_state",
            "open_logs",
            "migrate_logs",
            "open_goals",
            "migrate_goals",
            "open_memories",
            "migrate_memories",
            "ensure_backfill_state",
            "post_init_query",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
        assert_eq!(phases, expected);

        runtime.close().await;
        let _ = tokio::fs::remove_dir_all(codex_home).await;
    }
}
