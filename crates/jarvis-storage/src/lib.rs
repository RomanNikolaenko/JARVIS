//! SQLite storage for JARVIS: one database file with versioned migrations,
//! used through a single connection on blocking threads.
//!
//! The schema lives here (`migrations/*.sql`) so all domains share one
//! version line; domain crates (`jarvis-memory`, later tasks and audit) own
//! their queries and run them through [`Database::call`].
//!
//! The file is not encrypted: it relies on the protection of the user's
//! profile directory. On Unix the database is created with mode 0600 inside
//! a 0700 directory.

mod error;
mod migrations;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rusqlite::Connection;

pub use error::{Result, StorageError};
pub use migrations::{MIGRATIONS, Migration, MigrationReport, SCHEMA_VERSION};
pub use rusqlite;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Handle to the database. Cheap to clone; all clones share the connection.
#[derive(Debug, Clone)]
pub struct Database {
    connection: Arc<Mutex<Connection>>,
    path: Option<PathBuf>,
    report: MigrationReport,
}

impl Database {
    /// Opens (or creates) the database at `path` and applies pending
    /// migrations.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let opened = path.clone();
        let (connection, report) = tokio::task::spawn_blocking(move || open_file(&opened))
            .await
            .map_err(|err| {
                StorageError::Internal(format!("opening the database failed: {err}"))
            })??;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            path: Some(path),
            report,
        })
    }

    /// A private in-memory database with the full schema (tests).
    pub fn open_in_memory() -> Result<Self> {
        let mut connection = Connection::open_in_memory()?;
        configure(&connection)?;
        let report = migrations::migrate(&mut connection, None, MIGRATIONS)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            path: None,
            report,
        })
    }

    /// Runs `f` with the connection on a blocking thread.
    pub async fn call<T, E, F>(&self, f: F) -> std::result::Result<T, E>
    where
        F: FnOnce(&mut Connection) -> std::result::Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<StorageError> + Send + 'static,
    {
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let mut guard = connection.lock().unwrap_or_else(PoisonError::into_inner);
            f(&mut guard)
        })
        .await
        .map_err(|err| {
            E::from(StorageError::Internal(format!(
                "database task failed: {err}"
            )))
        })?
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// What happened when the database was opened.
    pub fn report(&self) -> &MigrationReport {
        &self.report
    }

    /// Size on disk including the write-ahead log.
    pub async fn size_bytes(&self) -> u64 {
        let Some(path) = &self.path else { return 0 };
        let mut total = 0;
        for suffix in ["", "-wal"] {
            let mut name = path.as_os_str().to_owned();
            name.push(suffix);
            if let Ok(meta) = tokio::fs::metadata(PathBuf::from(name)).await {
                total += meta.len();
            }
        }
        total
    }

    /// Rewrites the file without free pages and empties the write-ahead log,
    /// so deleted content does not linger on disk.
    pub async fn compact(&self) -> Result<()> {
        self.call(|conn| {
            conn.execute_batch("VACUUM;")?;
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            Ok(())
        })
        .await
    }
}

fn open_file(path: &Path) -> Result<(Connection, MigrationReport)> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|err| StorageError::io(format!("creating {}", dir.display()), err))?;
        restrict_permissions(dir, 0o700);
    }
    let corrupt = |err: rusqlite::Error| {
        if StorageError::is_corruption(&err) {
            StorageError::Corrupt {
                path: path.display().to_string(),
            }
        } else {
            StorageError::Sqlite(err)
        }
    };
    let mut connection = Connection::open(path).map_err(corrupt)?;
    configure(&connection).map_err(|err| match err {
        StorageError::Sqlite(err) => corrupt(err),
        other => other,
    })?;
    restrict_permissions(path, 0o600);
    let report =
        migrations::migrate(&mut connection, Some(path), MIGRATIONS).map_err(|err| match err {
            StorageError::Sqlite(err) => corrupt(err),
            other => other,
        })?;
    Ok((connection, report))
}

fn configure(connection: &Connection) -> Result<()> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    // Overwrite deleted content instead of leaving it in free pages.
    connection.pragma_update(None, "secure_delete", true)?;
    let _mode: String =
        connection.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    let fts5: bool = connection.query_row(
        "SELECT sqlite_compileoption_used('ENABLE_FTS5')",
        [],
        |row| row.get(0),
    )?;
    if !fts5 {
        return Err(StorageError::MissingFts5);
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(err) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
        tracing::warn!("cannot restrict permissions of {}: {err}", path.display());
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path, _mode: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creates_the_schema_and_reopens_without_migrating() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory").join("jarvis.db");
        let db = Database::open(&path).await.expect("open");
        assert_eq!(db.report().schema_version, SCHEMA_VERSION);
        assert_eq!(db.report().applied.len(), MIGRATIONS.len());
        let (foreign_keys, journal): (bool, String) = db
            .call(|conn| {
                Ok::<_, StorageError>((
                    conn.pragma_query_value(None, "foreign_keys", |row| row.get(0))?,
                    conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?,
                ))
            })
            .await
            .expect("pragmas");
        assert!(foreign_keys);
        assert_eq!(journal.to_ascii_lowercase(), "wal");
        assert!(db.size_bytes().await > 0);
        drop(db);

        let reopened = Database::open(&path).await.expect("reopen");
        assert!(reopened.report().applied.is_empty());
        assert!(reopened.report().backup.is_none());
    }

    #[tokio::test]
    async fn deleting_a_conversation_cleans_up_messages_index_and_vectors() {
        let db = Database::open_in_memory().expect("open");
        let counts = db
            .call(|conn| {
                conn.execute_batch(
                    "INSERT INTO conversations (id, title, created_at, updated_at) VALUES ('c1', 't', 0, 0);
                     INSERT INTO messages (uid, conversation_id, role, content, status, tokens, created_at)
                         VALUES ('m1', 'c1', 'user', 'Привіт, як справи з проєктом Атлас?', 'complete', 10, 0);
                     INSERT INTO embeddings (item_kind, item_id, model_key, vector)
                         SELECT 'message', id, 'test', x'0000803f' FROM messages;",
                )?;
                let found: i64 = conn.query_row(
                    "SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'атлас'",
                    [],
                    |row| row.get(0),
                )?;
                conn.execute("DELETE FROM conversations WHERE id = 'c1'", [])?;
                let left: (i64, i64, i64) = conn.query_row(
                    "SELECT (SELECT count(*) FROM messages),
                            (SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'атлас'),
                            (SELECT count(*) FROM embeddings)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
                Ok::<_, StorageError>((found, left))
            })
            .await
            .expect("queries");
        assert_eq!(counts.0, 1, "FTS finds Cyrillic text case-insensitively");
        assert_eq!(counts.1, (0, 0, 0));
    }

    #[tokio::test]
    async fn a_file_that_is_not_a_database_is_reported_as_corrupt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        std::fs::write(&path, vec![0x42; 4096]).expect("write");
        let err = Database::open(&path).await.expect_err("not a database");
        assert!(matches!(err, StorageError::Corrupt { .. }), "{err:?}");
        assert!(path.exists(), "the file is left for the user to inspect");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_database_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory").join("jarvis.db");
        let _db = Database::open(&path).await.expect("open");
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = std::fs::metadata(path.parent().expect("dir"))
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
    }
}
