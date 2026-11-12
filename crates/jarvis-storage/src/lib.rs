//! SQLite storage for JARVIS: one database file with versioned migrations,
//! used through a single connection on blocking threads.
//!
//! The schema lives here (`migrations/*.sql`) so all domains share one
//! version line; domain crates (`jarvis-memory`, later tasks and audit) own
//! their queries and run them through [`Database::call`].
//!
//! The file is plain SQLite unless it is opened with a key, which makes it a
//! SQLCipher database (AES-256, the key stretched with PBKDF2-SHA512).
//! [`file_state`] tells the two apart by the header. On Unix the database is
//! created with mode 0600 inside a 0700 directory.

mod encryption;
mod error;
mod migrations;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rusqlite::Connection;

pub use error::{Result, StorageError};
pub use migrations::{MIGRATIONS, Migration, MigrationReport, SCHEMA_VERSION};
pub use rusqlite;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// The first bytes of every plain SQLite file. SQLCipher files start with a
/// random salt instead.
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

/// What a database file holds, judged by its first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// No file, or an empty one: opening creates a new database.
    Missing,
    /// A plain SQLite database.
    Plain,
    /// No SQLite header: an encrypted database (or a damaged file, which
    /// cannot be told apart from one).
    Encrypted,
}

/// Reads the header of the database file at `path`.
pub fn file_state(path: &Path) -> std::io::Result<FileState> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(FileState::Missing),
        Err(err) => return Err(err),
    };
    let mut header = [0u8; SQLITE_HEADER.len()];
    let mut filled = 0;
    while filled < header.len() {
        match file.read(&mut header[filled..])? {
            0 => break,
            read => filled += read,
        }
    }
    Ok(match filled {
        0 => FileState::Missing,
        _ if header == *SQLITE_HEADER => FileState::Plain,
        _ => FileState::Encrypted,
    })
}

/// Handle to the database. Cheap to clone; all clones share the connection.
#[derive(Debug, Clone)]
pub struct Database {
    connection: Arc<Mutex<Connection>>,
    /// Set when a failed encryption could not reopen any database file: every
    /// call fails until JARVIS restarts and tidies the leftovers.
    closed: Arc<AtomicBool>,
    path: Option<PathBuf>,
    report: MigrationReport,
}

impl Database {
    /// Opens (or creates) the plain database at `path` and applies pending
    /// migrations.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self> {
        Self::open_with_key(path, None).await
    }

    /// Opens (or creates) the database at `path` with an encryption key and
    /// applies pending migrations. The key is used only for an encrypted file:
    /// a new file is created plain and a plain file stays plain (encrypting is
    /// [`Database::export_encrypted`] and [`Database::switch_to_encrypted`]).
    /// An encrypted file without its key is [`StorageError::Locked`].
    pub async fn open_with_key(path: impl Into<PathBuf>, key: Option<String>) -> Result<Self> {
        let path = path.into();
        let opened = path.clone();
        let (connection, report) =
            tokio::task::spawn_blocking(move || open_file(&opened, key.as_deref()))
                .await
                .map_err(|err| {
                    StorageError::Internal(format!("opening the database failed: {err}"))
                })??;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            closed: Arc::default(),
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
            closed: Arc::default(),
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
        let closed = Arc::clone(&self.closed);
        tokio::task::spawn_blocking(move || {
            let mut guard = connection.lock().unwrap_or_else(PoisonError::into_inner);
            if closed.load(Ordering::Acquire) {
                return Err(E::from(StorageError::Internal(
                    "the memory database could not be reopened after a failed encryption; restart JARVIS"
                        .to_owned(),
                )));
            }
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

fn open_file(path: &Path, key: Option<&str>) -> Result<(Connection, MigrationReport)> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|err| StorageError::io(format!("creating {}", dir.display()), err))?;
        restrict_permissions(dir, 0o700);
    }
    encryption::tidy_leftovers(path, key)?;
    let state = file_state(path)
        .map_err(|err| StorageError::io(format!("reading {}", path.display()), err))?;
    let corrupt = |err: rusqlite::Error| {
        if !StorageError::is_corruption(&err) {
            StorageError::Sqlite(err)
        } else if state == FileState::Encrypted {
            StorageError::Locked
        } else {
            StorageError::Corrupt {
                path: path.display().to_string(),
            }
        }
    };
    let mut connection = Connection::open(path).map_err(corrupt)?;
    // The key must be the first statement. Leftovers are tidied by now, so
    // the header says whether the file is encrypted.
    if state == FileState::Encrypted {
        let key = key.ok_or(StorageError::Locked)?;
        connection
            .pragma_update(None, "key", key)
            .map_err(corrupt)?;
    }
    configure(&connection).map_err(|err| match err {
        StorageError::Sqlite(err) => corrupt(err),
        other => other,
    })?;
    restrict_permissions(path, 0o600);
    let finish_encryption = state == FileState::Encrypted
        && encryption::verify_interrupted_encryption(path, &connection)?;
    let report =
        migrations::migrate(&mut connection, Some(path), MIGRATIONS).map_err(|err| match err {
            StorageError::Sqlite(err) => corrupt(err),
            other => other,
        })?;
    if finish_encryption {
        encryption::finish_interrupted_encryption(path);
    }
    Ok((connection, report))
}

fn configure(connection: &Connection) -> Result<()> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    // Overwrite deleted content instead of leaving it in free pages.
    connection.pragma_update(None, "secure_delete", true)?;
    let _mode: String =
        connection.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    // Agent execution intents must reach durable storage before tools run,
    // including across a power loss, not just a process crash.
    connection.pragma_update(None, "synchronous", "FULL")?;
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
    async fn sqlcipher_is_compiled_in() {
        let db = Database::open_in_memory().expect("open");
        let version: String = db
            .call(|conn| {
                Ok::<_, StorageError>(
                    conn.query_row("PRAGMA cipher_version", [], |row| row.get(0))?,
                )
            })
            .await
            .expect("SQLCipher answers cipher_version");
        assert!(version.starts_with("4."), "{version}");
    }

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
        let synchronous: u32 = db
            .call(|conn| {
                Ok::<_, StorageError>(
                    conn.pragma_query_value(None, "synchronous", |row| row.get(0))?,
                )
            })
            .await
            .expect("durability");
        assert_eq!(
            synchronous, 2,
            "FULL sync protects execution intents across power loss"
        );
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

    const KEY: &str = "123456-234567-345678-456789-567890-678901-789012-890123";

    /// A database encrypted the way the app does it.
    async fn encrypted_database(path: &Path) -> Database {
        let db = Database::open(path).await.expect("plain");
        db.encrypt_in_place(KEY).await.expect("encrypt");
        db
    }

    async fn count_atlas(db: &Database) -> i64 {
        db.call(|conn| {
            Ok::<_, StorageError>(conn.query_row(
                "SELECT count(*) FROM conversations WHERE title = 'Atlas deadline'",
                [],
                |row| row.get(0),
            )?)
        })
        .await
        .expect("query")
    }

    #[tokio::test]
    async fn an_encrypted_database_hides_its_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        let db = encrypted_database(&path).await;
        db.call(|conn| {
            conn.execute(
                "INSERT INTO conversations (id, title, created_at, updated_at)
                 VALUES ('c1', 'Atlas deadline', 0, 0)",
                [],
            )?;
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            Ok::<_, StorageError>(())
        })
        .await
        .expect("insert");
        drop(db);

        assert_eq!(file_state(&path).expect("state"), FileState::Encrypted);
        let bytes = std::fs::read(&path).expect("read");
        assert!(!bytes.starts_with(SQLITE_HEADER));
        assert!(
            !bytes.windows(14).any(|w| w == b"Atlas deadline"),
            "the text is not stored in the clear"
        );

        let reopened = Database::open_with_key(&path, Some(KEY.to_owned()))
            .await
            .expect("reopen with the key");
        assert_eq!(count_atlas(&reopened).await, 1);
        let fts5: bool = reopened
            .call(|conn| {
                Ok::<_, StorageError>(conn.query_row(
                    "SELECT sqlite_compileoption_used('ENABLE_FTS5')",
                    [],
                    |row| row.get(0),
                )?)
            })
            .await
            .expect("compile options");
        assert!(fts5);
    }

    #[tokio::test]
    async fn a_missing_or_wrong_key_reports_locked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        drop(encrypted_database(&path).await);
        let without = Database::open(&path).await.expect_err("no key");
        assert!(matches!(without, StorageError::Locked), "{without:?}");
        let wrong = Database::open_with_key(&path, Some(KEY.replace('1', "9")))
            .await
            .expect_err("wrong key");
        assert!(matches!(wrong, StorageError::Locked), "{wrong:?}");
        assert_eq!(file_state(&path).expect("state"), FileState::Encrypted);
    }

    #[tokio::test]
    async fn a_plain_database_ignores_a_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        drop(Database::open(&path).await.expect("create plain"));
        assert_eq!(file_state(&path).expect("state"), FileState::Plain);
        let db = Database::open_with_key(&path, Some(KEY.to_owned()))
            .await
            .expect("opens plain");
        assert_eq!(count_atlas(&db).await, 0);
        drop(db);
        assert_eq!(file_state(&path).expect("state"), FileState::Plain);
    }

    #[tokio::test]
    async fn a_new_database_is_plain_even_with_a_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        drop(
            Database::open_with_key(&path, Some(KEY.to_owned()))
                .await
                .expect("create"),
        );
        assert_eq!(file_state(&path).expect("state"), FileState::Plain);
    }

    #[test]
    fn file_state_reads_the_header() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        assert_eq!(file_state(&path).expect("state"), FileState::Missing);
        std::fs::write(&path, b"").expect("write");
        assert_eq!(file_state(&path).expect("state"), FileState::Missing);
        std::fs::write(&path, b"SQLite").expect("write");
        assert_eq!(file_state(&path).expect("state"), FileState::Encrypted);
    }

    #[tokio::test]
    async fn a_damaged_plain_database_is_reported_as_corrupt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        let mut bytes = SQLITE_HEADER.to_vec();
        bytes.resize(4096, 0x42);
        std::fs::write(&path, bytes).expect("write");
        let err = Database::open(&path).await.expect_err("not a database");
        assert!(matches!(err, StorageError::Corrupt { .. }), "{err:?}");
        assert!(path.exists(), "the file is left for the user to inspect");
    }

    #[tokio::test]
    async fn a_file_without_a_header_is_locked() {
        // An encrypted database and a damaged header look the same.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        std::fs::write(&path, vec![0x42; 4096]).expect("write");
        let err = Database::open(&path)
            .await
            .expect_err("not a plain database");
        assert!(matches!(err, StorageError::Locked), "{err:?}");
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
