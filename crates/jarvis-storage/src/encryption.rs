//! Encrypting a plain database in place, safe against a stop at any step.
//!
//! 1. [`Database::export_encrypted`] writes `<db>.encrypting` with
//!    `sqlcipher_export` and compares the row counts of every table.
//! 2. The caller stores the key (the OS keychain); if that fails it calls
//!    [`Database::discard_encrypted_copy`] and nothing has changed.
//! 3. [`Database::switch_to_encrypted`] closes the plain file, renames it to
//!    `<db>.plain-old`, moves the copy into its place, reopens it with the key
//!    and wipes the plain files.
//!
//! [`tidy_leftovers`] finishes or undoes a stop between these steps when the
//! database is opened next.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::PoisonError;

use rusqlite::{Connection, params};

use crate::{Database, FileState, Result, StorageError, configure, file_state};

impl Database {
    /// Writes an encrypted copy of the database to `<db>.encrypting` and
    /// checks that every table has the same number of rows in it.
    pub async fn export_encrypted(&self, key: &str) -> Result<()> {
        let path = self.file_path()?;
        let key = key.to_owned();
        self.call(move |conn| {
            let copy = sibling(&path, ".encrypting");
            remove_file(&copy)?;
            let result = export(conn, &copy, &key);
            if result.is_err() {
                // A partial copy is useless; the plain database is untouched.
                let _ = remove_file(&copy);
            }
            result
        })
        .await
    }

    /// Deletes the encrypted copy (the key could not be stored).
    pub async fn discard_encrypted_copy(&self) -> Result<()> {
        let path = self.file_path()?;
        tokio::task::spawn_blocking(move || remove_file(&sibling(&path, ".encrypting")))
            .await
            .map_err(|err| StorageError::Internal(format!("discarding the copy failed: {err}")))?
    }

    /// Replaces the plain database with the encrypted copy, reopens it with
    /// `key` and wipes the plain file, its journal and the plain migration
    /// backups. Every clone of this handle uses the encrypted file afterwards.
    pub async fn switch_to_encrypted(&self, key: &str) -> Result<()> {
        let path = self.file_path()?;
        let key = key.to_owned();
        let connection = std::sync::Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let mut guard = connection.lock().unwrap_or_else(PoisonError::into_inner);
            let copy = sibling(&path, ".encrypting");
            if file_state(&copy).map_err(|err| io_error("reading", &copy, err))?
                != FileState::Encrypted
            {
                return Err(StorageError::Internal(
                    "there is no encrypted copy to switch to".to_owned(),
                ));
            }
            // Close the plain file: Windows cannot rename a file in use.
            let plain = std::mem::replace(&mut *guard, Connection::open_in_memory()?);
            if let Err((plain, err)) = plain.close() {
                *guard = plain;
                return Err(err.into());
            }
            let old = sibling(&path, ".plain-old");
            let reopened = swap_files(&path, &copy, &old).and_then(|()| open_keyed(&path, &key));
            match reopened {
                Ok(encrypted) => *guard = encrypted,
                Err(err) => {
                    // Put the plain database back so memory keeps working.
                    let restored = restore_plain(&path, &copy, &old)
                        .and_then(|()| Ok(Connection::open(&path)?))
                        .and_then(|conn| configure(&conn).map(|()| conn));
                    match restored {
                        Ok(plain) => *guard = plain,
                        Err(restore) => tracing::error!(
                            "the plain database could not be reopened after a failed switch: {restore}"
                        ),
                    }
                    return Err(err);
                }
            }
            drop(guard);
            wipe_plain_files(&path);
            Ok(())
        })
        .await
        .map_err(|err| StorageError::Internal(format!("switching to the encrypted database failed: {err}")))?
    }

    fn file_path(&self) -> Result<PathBuf> {
        self.path.clone().ok_or_else(|| {
            StorageError::Internal("an in-memory database has no file to encrypt".to_owned())
        })
    }
}

fn export(conn: &Connection, copy: &Path, key: &str) -> Result<()> {
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    let tables = row_counts(conn)?;
    conn.execute(
        "ATTACH DATABASE ?1 AS encrypting KEY ?2",
        params![copy.to_string_lossy(), key],
    )?;
    let exported = export_into(conn, "encrypting");
    conn.execute("DETACH DATABASE encrypting", [])?;
    exported?;
    // Read the copy back through a fresh connection: this also proves the key opens it.
    let check = open_keyed(copy, key)?;
    let copied = row_counts(&check)?;
    check.close().map_err(|(_, err)| err)?;
    if copied != tables {
        return Err(StorageError::Internal(
            "the encrypted copy is incomplete".to_owned(),
        ));
    }
    Ok(())
}

/// Copies the main database into the attached database `schema` with
/// `sqlcipher_export`, and its `user_version`, which the export leaves out.
pub(crate) fn export_into(conn: &Connection, schema: &str) -> Result<()> {
    conn.query_row("SELECT sqlcipher_export(?1)", [schema], |_| Ok(()))?;
    let version: i64 = conn.pragma_query_value(Some("main"), "user_version", |row| row.get(0))?;
    conn.pragma_update(Some(schema), "user_version", version)?;
    Ok(())
}

/// `(table, rows)` for every table with storage, in name order.
fn row_counts(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let names: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT name FROM main.sqlite_schema
             WHERE type = 'table' AND rootpage > 0 ORDER BY name",
        )?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    names
        .into_iter()
        .map(|name| {
            let quoted = name.replace('"', "\"\"");
            let rows: i64 = conn.query_row(
                &format!("SELECT count(*) FROM main.\"{quoted}\""),
                [],
                |row| row.get(0),
            )?;
            Ok((name, rows))
        })
        .collect()
}

/// Opens an existing encrypted file with `key`, configured like the database.
pub(crate) fn open_keyed(path: &Path, key: &str) -> Result<Connection> {
    let connection = Connection::open(path)?;
    connection.pragma_update(None, "key", key)?;
    crate::restrict_permissions(path, 0o600);
    configure(&connection).map_err(|err| match err {
        StorageError::Sqlite(err) if StorageError::is_corruption(&err) => StorageError::Locked,
        other => other,
    })?;
    Ok(connection)
}

/// `<db>` → `<db>.plain-old` (with a journal left beside it), then the copy → `<db>`.
fn swap_files(path: &Path, copy: &Path, old: &Path) -> Result<()> {
    rename(path, old)?;
    for suffix in ["-wal", "-shm"] {
        let journal = sibling(path, suffix);
        if journal.exists() {
            rename(&journal, &sibling(old, suffix))?;
        }
    }
    rename(copy, path)
}

fn restore_plain(path: &Path, copy: &Path, old: &Path) -> Result<()> {
    if old.exists() {
        if path.exists() {
            rename(path, copy)?;
        }
        rename(old, path)?;
        for suffix in ["-wal", "-shm"] {
            let journal = sibling(old, suffix);
            if journal.exists() {
                rename(&journal, &sibling(path, suffix))?;
            }
        }
    }
    remove_file(copy)
}

/// Finishes or undoes an encryption that stopped part-way. Runs before the
/// database is opened.
pub(crate) fn tidy_leftovers(path: &Path) -> Result<()> {
    let copy = sibling(path, ".encrypting");
    let old = sibling(path, ".plain-old");
    let state = |p: &Path| file_state(p).map_err(|err| io_error("reading", p, err));
    if state(path)? == FileState::Missing && state(&old)? == FileState::Plain {
        if state(&copy)? == FileState::Encrypted {
            // Stopped between the two renames: the copy was complete and its
            // key stored before the first one.
            tracing::warn!("finishing an interrupted memory encryption");
            rename(&copy, path)?;
        } else {
            tracing::warn!("restoring the plain memory database after an interrupted encryption");
            restore_plain(path, &copy, &old)?;
        }
    }
    match state(path)? {
        FileState::Encrypted => {
            if copy.exists() {
                remove_file(&copy)?;
            }
            // Only `.plain-old` proves an encryption ran: a damaged plain file
            // also has no header, and its plain backups may be all that is left.
            if old.exists() {
                tracing::warn!("wiping the plain copy left by an interrupted encryption");
                wipe_plain_files(path);
            }
        }
        FileState::Plain | FileState::Missing => {
            // Stopped before the swap: the plain database is still in use.
            if copy.exists() {
                tracing::warn!("removing the copy left by an interrupted encryption");
                remove_file(&copy)?;
            }
        }
    }
    Ok(())
}

/// Overwrites with zeros, then deletes: every plain migration backup
/// `<db>.v*-*.bak`, then `<db>.plain-old` with its journal. `.plain-old` goes
/// last because it marks an unfinished wipe for [`tidy_leftovers`]. Failures
/// are logged, as the encrypted database is already in use.
fn wipe_plain_files(path: &Path) {
    let old = sibling(path, ".plain-old");
    let mut files = plain_backups(path);
    files.extend([sibling(&old, "-wal"), sibling(&old, "-shm"), old]);
    for file in files {
        if file.exists()
            && let Err(err) = wipe(&file)
        {
            tracing::warn!("cannot wipe {}: {err}", file.display());
        }
    }
}

fn plain_backups(path: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Vec::new();
    };
    let prefix = format!("{}.v", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|file| {
            file.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".bak"))
        })
        .filter(|file| matches!(file_state(file), Ok(FileState::Plain)))
        .collect()
}

/// Zero-fills a file and deletes it. On SSDs and copy-on-write file systems
/// the old blocks may survive; this removes the easy way to read them.
fn wipe(file: &Path) -> std::io::Result<()> {
    let length = std::fs::metadata(file)?.len();
    {
        let mut handle = std::fs::OpenOptions::new().write(true).open(file)?;
        let zeros = vec![0u8; 64 * 1024];
        let mut left = length;
        while left > 0 {
            let chunk = left.min(zeros.len() as u64) as usize;
            handle.write_all(&zeros[..chunk])?;
            left -= chunk as u64;
        }
        handle.sync_all()?;
    }
    std::fs::remove_file(file)
}

pub(crate) fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn rename(from: &Path, to: &Path) -> Result<()> {
    std::fs::rename(from, to).map_err(|err| {
        StorageError::io(
            format!("renaming {} to {}", from.display(), to.display()),
            err,
        )
    })
}

fn remove_file(file: &Path) -> Result<()> {
    match std::fs::remove_file(file) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            Err(io_error("deleting", file, err))
        }
        _ => Ok(()),
    }
}

fn io_error(action: &str, file: &Path, err: std::io::Error) -> StorageError {
    StorageError::io(format!("{action} {}", file.display()), err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MIGRATIONS, migrations};

    const KEY: &str = "111111-222222-333333-444444-555555-666666-777777-888888";

    fn seed(conn: &Connection) -> rusqlite::Result<()> {
        conn.execute_batch(
            "INSERT INTO conversations (id, title, created_at, updated_at) VALUES ('c1', 'Atlas', 0, 0);
             INSERT INTO messages (uid, conversation_id, role, content, status, tokens, created_at)
                 VALUES ('m1', 'c1', 'user', 'The Atlas launch moves to Friday', 'complete', 8, 0);
             INSERT INTO facts (uid, content, source, created_at, updated_at)
                 VALUES ('f1', 'Prefers green tea', 'user', 0, 0);
             INSERT INTO agent_tasks VALUES ('t1', 'goal', 'completed', NULL, NULL,
                 '{}', '[]', '[]', 'answer', NULL, 1, 2, 3);",
        )
    }

    async fn plain_database(path: &Path) -> Database {
        let db = Database::open(path).await.expect("plain database");
        db.call(|conn| Ok::<_, StorageError>(seed(conn)?))
            .await
            .expect("seed");
        db
    }

    async fn counts(db: &Database) -> Vec<(String, i64)> {
        db.call(|conn| row_counts(conn)).await.expect("row counts")
    }

    async fn conversations(db: &Database) -> i64 {
        db.call(|conn| {
            Ok::<_, StorageError>(conn.query_row(
                "SELECT count(*) FROM conversations",
                [],
                |row| row.get(0),
            )?)
        })
        .await
        .expect("count")
    }

    /// Files an encryption may leave next to the database.
    fn leftovers(path: &Path) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = [
            ".encrypting",
            ".plain-old",
            ".plain-old-wal",
            ".plain-old-shm",
        ]
        .iter()
        .map(|suffix| sibling(path, suffix))
        .filter(|file| file.exists())
        .collect();
        found.extend(plain_backups(path));
        found
    }

    #[tokio::test]
    async fn encrypting_keeps_every_row_and_search() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        let db = plain_database(&path).await;
        // A plain migration backup, as an upgrade leaves next to the database.
        let backup = dir.path().join("jarvis.db.v4-1.bak");
        let target = backup.to_string_lossy().into_owned();
        db.call(move |conn| {
            conn.execute("VACUUM INTO ?1", [target])?;
            Ok::<_, StorageError>(())
        })
        .await
        .expect("backup");
        let before = counts(&db).await;
        assert!(
            before
                .iter()
                .any(|(table, rows)| table == "agent_tasks" && *rows == 1)
        );

        db.export_encrypted(KEY).await.expect("export");
        db.switch_to_encrypted(KEY).await.expect("switch");
        assert_eq!(
            counts(&db).await,
            before,
            "the same handle reads the encrypted file"
        );
        drop(db);

        assert_eq!(file_state(&path).expect("state"), FileState::Encrypted);
        assert!(leftovers(&path).is_empty(), "{:?}", leftovers(&path));
        assert!(!backup.exists(), "the plain backup is wiped");
        let reopened = Database::open_with_key(&path, Some(KEY.to_owned()))
            .await
            .expect("reopen with the key");
        assert!(reopened.report().applied.is_empty());
        assert_eq!(counts(&reopened).await, before);
        let (found, version): (i64, u32) = reopened
            .call(|conn| {
                Ok::<_, StorageError>((
                    conn.query_row(
                        "SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'friday'",
                        [],
                        |row| row.get(0),
                    )?,
                    conn.pragma_query_value(None, "user_version", |row| row.get(0))?,
                ))
            })
            .await
            .expect("search");
        assert_eq!(found, 1, "full-text search works on the copy");
        assert_eq!(version, crate::SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn a_stop_before_the_swap_keeps_the_plain_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        let db = plain_database(&path).await;
        db.export_encrypted(KEY).await.expect("export");
        drop(db);
        assert!(sibling(&path, ".encrypting").exists());

        let reopened = Database::open(&path)
            .await
            .expect("the plain database opens");
        assert_eq!(file_state(&path).expect("state"), FileState::Plain);
        assert_eq!(conversations(&reopened).await, 1);
        assert!(leftovers(&path).is_empty(), "{:?}", leftovers(&path));
    }

    #[tokio::test]
    async fn a_discarded_copy_changes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        let db = plain_database(&path).await;
        db.export_encrypted(KEY).await.expect("export");
        db.discard_encrypted_copy().await.expect("discard");
        assert!(leftovers(&path).is_empty(), "{:?}", leftovers(&path));
        assert_eq!(conversations(&db).await, 1);
        assert_eq!(file_state(&path).expect("state"), FileState::Plain);
        let err = db.switch_to_encrypted(KEY).await.expect_err("no copy");
        assert!(matches!(err, StorageError::Internal(_)), "{err:?}");
        assert_eq!(conversations(&db).await, 1, "the plain database stays open");
    }

    #[tokio::test]
    async fn a_stop_after_the_swap_wipes_the_plain_copy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        let old = sibling(&path, ".plain-old");
        // The encrypted database first: a plain-old file without one would be restored.
        drop(
            Database::open_with_key(&path, Some(KEY.to_owned()))
                .await
                .expect("encrypted database"),
        );
        drop(plain_database(&old).await);
        assert_eq!(file_state(&old).expect("state"), FileState::Plain);

        let reopened = Database::open_with_key(&path, Some(KEY.to_owned()))
            .await
            .expect("open with the key");
        assert_eq!(conversations(&reopened).await, 0);
        assert!(leftovers(&path).is_empty(), "{:?}", leftovers(&path));
    }

    #[tokio::test]
    async fn a_stop_between_the_renames_finishes_the_switch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        let db = plain_database(&path).await;
        db.export_encrypted(KEY).await.expect("export");
        drop(db);
        std::fs::rename(&path, sibling(&path, ".plain-old")).expect("first rename");

        let reopened = Database::open_with_key(&path, Some(KEY.to_owned()))
            .await
            .expect("the copy is moved into place");
        assert_eq!(file_state(&path).expect("state"), FileState::Encrypted);
        assert_eq!(conversations(&reopened).await, 1);
        assert!(leftovers(&path).is_empty(), "{:?}", leftovers(&path));
    }

    #[tokio::test]
    async fn a_lost_copy_restores_the_plain_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        drop(plain_database(&path).await);
        std::fs::rename(&path, sibling(&path, ".plain-old")).expect("first rename");

        let reopened = Database::open(&path).await.expect("restored");
        assert_eq!(file_state(&path).expect("state"), FileState::Plain);
        assert_eq!(conversations(&reopened).await, 1);
        assert!(leftovers(&path).is_empty(), "{:?}", leftovers(&path));
    }

    #[tokio::test]
    async fn a_file_without_a_header_keeps_its_plain_backups() {
        // A damaged plain database looks encrypted; its backups must survive.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        let backup = dir.path().join("jarvis.db.v4-1.bak");
        drop(plain_database(&backup).await);
        std::fs::write(&path, vec![0x42; 4096]).expect("damaged file");

        let err = Database::open(&path).await.expect_err("locked");
        assert!(matches!(err, StorageError::Locked), "{err:?}");
        assert_eq!(file_state(&backup).expect("state"), FileState::Plain);
    }

    #[test]
    fn backups_of_an_encrypted_database_are_encrypted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        let mut conn = Connection::open(&path).expect("open");
        conn.pragma_update(None, "key", KEY).expect("key");
        configure(&conn).expect("configure");
        migrations::migrate(&mut conn, Some(&path), &MIGRATIONS[..4]).expect("schema 4");
        let report = migrations::migrate(&mut conn, Some(&path), MIGRATIONS).expect("upgrade");

        let backup = report.backup.expect("backup before migrating data");
        assert_eq!(file_state(&backup).expect("state"), FileState::Encrypted);
        let copy = open_keyed(&backup, KEY).expect("the key opens the backup");
        let applied: i64 = copy
            .query_row("SELECT count(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("read");
        assert_eq!(applied, 4);
        let version: u32 = copy
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, 4);
    }
}
