//! Encrypting a plain database in place, safe against a stop at any step.
//!
//! The caller stores the key before invoking [`Database::encrypt_in_place`].
//! The storage layer exports and verifies `<db>.encrypting`, then replaces the
//! plain file and reopens it under the same connection lock. Queued writes use
//! the encrypted connection after the swap. Plain files are then wiped.
//!
//! [`tidy_leftovers`] finishes or undoes a stop between these steps when the
//! database is opened next.

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::PoisonError;

use rusqlite::{Connection, OpenFlags, params};

use crate::{Database, FileState, Result, StorageError, configure, file_state};

impl Database {
    /// Exports and verifies an encrypted copy, then swaps the shared connection
    /// without letting other calls write to the plain database in between.
    /// The caller must save the key before starting.
    pub async fn encrypt_in_place(&self, key: &str) -> Result<()> {
        let path = self.file_path()?;
        let key = key.to_owned();
        let connection = std::sync::Arc::clone(&self.connection);
        let closed = std::sync::Arc::clone(&self.closed);
        tokio::task::spawn_blocking(move || {
            let mut guard = connection.lock().unwrap_or_else(PoisonError::into_inner);
            if file_state(&path).map_err(|err| io_error("reading", &path, err))? != FileState::Plain
            {
                return Err(StorageError::Internal(
                    "the memory database is not plain".to_owned(),
                ));
            }
            let old = sibling(&path, ".plain-old");
            if old
                .try_exists()
                .map_err(|err| io_error("checking", &old, err))?
            {
                return Err(StorageError::Internal(
                    "a previous memory encryption still has a plain fallback".to_owned(),
                ));
            }
            export_copy(&guard, &path, &key)?;
            if let Err(err) = switch_connection(&mut guard, &path, &key) {
                // Neither file could be reopened: the placeholder has no schema.
                if guard.path().is_none_or(str::is_empty) {
                    closed.store(true, std::sync::atomic::Ordering::Release);
                }
                return Err(err);
            }
            drop(guard);
            wipe_plain_files(&path);
            Ok(())
        })
        .await
        .map_err(|err| StorageError::Internal(format!("encrypting the database failed: {err}")))?
    }

    // Crash tests need to leave a copy or simulate only part of a swap.
    #[cfg(test)]
    async fn export_encrypted(&self, key: &str) -> Result<()> {
        let path = self.file_path()?;
        let key = key.to_owned();
        self.call(move |conn| export_copy(conn, &path, &key)).await
    }

    #[cfg(test)]
    async fn discard_encrypted_copy(&self) -> Result<()> {
        let path = self.file_path()?;
        self.call(move |_| remove_file(&sibling(&path, ".encrypting")))
            .await
    }

    #[cfg(test)]
    async fn switch_to_encrypted(&self, key: &str) -> Result<()> {
        let path = self.file_path()?;
        let key = key.to_owned();
        self.call(move |conn| {
            switch_connection(conn, &path, &key)?;
            wipe_plain_files(&path);
            Ok(())
        })
        .await
    }

    fn file_path(&self) -> Result<PathBuf> {
        self.path.clone().ok_or_else(|| {
            StorageError::Internal("an in-memory database has no file to encrypt".to_owned())
        })
    }
}

fn export_copy(conn: &Connection, path: &Path, key: &str) -> Result<()> {
    let copy = sibling(path, ".encrypting");
    remove_copy(&copy)?;
    let result = export(conn, &copy, key);
    if result.is_err() {
        // A partial copy is useless; the plain database is untouched.
        let _ = remove_copy(&copy);
    }
    result
}

fn switch_connection(conn: &mut Connection, path: &Path, key: &str) -> Result<()> {
    let copy = sibling(path, ".encrypting");
    let old = sibling(path, ".plain-old");
    if file_state(&copy).map_err(|err| io_error("reading", &copy, err))? != FileState::Encrypted {
        return Err(StorageError::Internal(
            "there is no encrypted copy to switch to".to_owned(),
        ));
    }
    // Close the plain file: Windows cannot rename a file in use.
    let plain = std::mem::replace(conn, Connection::open_in_memory()?);
    if let Err((plain, err)) = plain.close() {
        *conn = plain;
        return Err(err.into());
    }
    let reopened = swap_files(path, &copy, &old)
        .and_then(|()| sync_dir(path))
        .and_then(|()| open_keyed(path, key));
    match reopened {
        Ok(encrypted) => {
            *conn = encrypted;
            Ok(())
        }
        Err(err) => {
            // Put the plain database back so memory keeps working.
            let restored = restore_plain(path, &copy, &old)
                .and_then(|()| Ok(Connection::open(path)?))
                .and_then(|plain| configure(&plain).map(|()| plain));
            match restored {
                Ok(plain) => *conn = plain,
                Err(restore) => tracing::error!(
                    "the plain database could not be reopened after a failed switch: {restore}"
                ),
            }
            Err(err)
        }
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
pub(crate) fn tidy_leftovers(path: &Path, key: Option<&str>) -> Result<()> {
    let copy = sibling(path, ".encrypting");
    let old = sibling(path, ".plain-old");
    let state = |p: &Path| file_state(p).map_err(|err| io_error("reading", p, err));
    let current = state(path)?;
    if current == FileState::Missing
        && old
            .try_exists()
            .map_err(|err| io_error("checking", &old, err))?
    {
        match state(&copy)? {
            FileState::Encrypted => {
                let key = key.ok_or(StorageError::Locked)?;
                verify_recovery_copy(&copy, key)?;
                // The interrupted rename may have left sidecars under the
                // main name. Move the plain files away before promoting the copy.
                for suffix in ["-wal", "-shm"] {
                    let journal = sibling(path, suffix);
                    if journal
                        .try_exists()
                        .map_err(|err| io_error("checking", &journal, err))?
                    {
                        let target = sibling(&old, suffix);
                        if target
                            .try_exists()
                            .map_err(|err| io_error("checking", &target, err))?
                        {
                            return Err(StorageError::Internal(
                                "conflicting journals from an interrupted memory encryption"
                                    .to_owned(),
                            ));
                        }
                        rename(&journal, &target)?;
                    }
                }
                tracing::warn!("finishing an interrupted memory encryption");
                rename(&copy, path)?;
                sync_dir(path)?;
            }
            FileState::Missing if state(&old)? == FileState::Plain => {
                tracing::warn!(
                    "restoring the plain memory database after an interrupted encryption"
                );
                restore_plain(path, &copy, &old)?;
            }
            _ => {
                return Err(StorageError::Internal(
                    "could not verify either database from an interrupted memory encryption"
                        .to_owned(),
                ));
            }
        }
    } else if current == FileState::Plain
        && !old
            .try_exists()
            .map_err(|err| io_error("checking", &old, err))?
    {
        // Stopped before the swap: the plain database is still in use.
        remove_copy(&copy)?;
    }
    Ok(())
}

fn verify_recovery_copy(path: &Path, key: &str) -> Result<()> {
    let journal = sibling(path, "-wal");
    match std::fs::metadata(&journal) {
        Ok(metadata) if metadata.len() != 0 => {
            return Err(StorageError::Internal(
                "the encrypted recovery copy still depends on its write-ahead log".to_owned(),
            ));
        }
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            return Err(io_error("checking", &journal, err));
        }
        _ => {}
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.pragma_update(None, "key", key)?;
    verify_recovery_database(&conn).map_err(|err| match err {
        StorageError::Sqlite(err) if StorageError::is_corruption(&err) => StorageError::Locked,
        other => other,
    })
}

fn verify_recovery_database(conn: &Connection) -> Result<()> {
    conn.query_row("SELECT version FROM schema_migrations LIMIT 1", [], |_| {
        Ok(())
    })?;
    let integrity: String = conn.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(StorageError::Internal(
            "the encrypted memory database failed its integrity check".to_owned(),
        ));
    }
    Ok(())
}

/// Verify the replacement before migrations can initialize an empty database.
pub(crate) fn verify_interrupted_encryption(path: &Path, conn: &Connection) -> Result<bool> {
    let old = sibling(path, ".plain-old");
    let pending = old
        .try_exists()
        .map_err(|err| io_error("checking", &old, err))?;
    if pending {
        verify_recovery_database(conn)?;
    }
    Ok(pending)
}

/// Runs after the verified replacement has also passed migration checks.
pub(crate) fn finish_interrupted_encryption(path: &Path) {
    tracing::warn!("wiping the plain copy left by an interrupted encryption");
    wipe_plain_files(path);
}

/// Overwrites with zeros, then deletes: every plain migration backup
/// `<db>.v*-*.bak`, then `<db>.plain-old` with its journal. `.plain-old` goes
/// last because it marks an unfinished wipe for [`tidy_leftovers`]. Failures
/// are logged, as the encrypted database is already in use.
fn wipe_plain_files(path: &Path) {
    let old = sibling(path, ".plain-old");
    let result = plain_backups(path).and_then(|mut files| {
        remove_copy(&sibling(path, ".encrypting"))?;
        files.extend([sibling(&old, "-wal"), sibling(&old, "-shm"), old]);
        for file in files {
            wipe(&file).map_err(|err| io_error("wiping", &file, err))?;
        }
        Ok(())
    });
    if let Err(err) = result {
        tracing::warn!("plain database cleanup will be retried: {err}");
    }
}

fn plain_backups(path: &Path) -> Result<Vec<PathBuf>> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Ok(Vec::new());
    };
    let prefix = format!("{}.v", name.to_string_lossy());
    let entries = std::fs::read_dir(dir).map_err(|err| io_error("reading", dir, err))?;
    let mut backups = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| io_error("reading", dir, err))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let file = entry.path();
        if name.starts_with(&prefix)
            && name.ends_with(".bak")
            && entry
                .file_type()
                .map_err(|err| io_error("checking", &file, err))?
                .is_file()
            && file_state(&file).map_err(|err| io_error("reading", &file, err))? == FileState::Plain
        {
            backups.push(file);
        }
    }
    Ok(backups)
}

/// Zero-fills a file and deletes it. On SSDs and copy-on-write file systems
/// the old blocks may survive; this removes the easy way to read them.
fn wipe(file: &Path) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(file) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if !metadata.is_file() {
        return Err(std::io::Error::other(
            "the cleanup target is not a regular file",
        ));
    }
    {
        let mut handle = std::fs::OpenOptions::new().write(true).open(file)?;
        let zeros = vec![0u8; 64 * 1024];
        // Leave the header intact until the data is wiped so interrupted
        // backup cleanup can still recognize a partly wiped plain file.
        let header = metadata.len().min(16);
        handle.seek(SeekFrom::Start(header))?;
        let mut left = metadata.len() - header;
        while left != 0 {
            let chunk = left.min(zeros.len() as u64) as usize;
            handle.write_all(&zeros[..chunk])?;
            left -= chunk as u64;
        }
        handle.sync_data()?;
        handle.rewind()?;
        handle.write_all(&zeros[..header as usize])?;
        handle.sync_all()?;
    }
    std::fs::remove_file(file)
}

pub(crate) fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Renames, retrying briefly while another process (an antivirus scanning
/// the new copy, a backup tool) holds the file open on Windows.
fn rename(from: &Path, to: &Path) -> Result<()> {
    let mut attempt = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(err) if attempt < RENAME_RETRIES && in_use(&err) => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(100 * attempt));
            }
            Err(err) => {
                return Err(StorageError::io(
                    format!("renaming {} to {}", from.display(), to.display()),
                    err,
                ));
            }
        }
    }
}

const RENAME_RETRIES: u64 = 8;

/// ERROR_ACCESS_DENIED or ERROR_SHARING_VIOLATION on Windows.
fn in_use(err: &std::io::Error) -> bool {
    cfg!(windows) && matches!(err.raw_os_error(), Some(5 | 32))
}

/// Makes renames in the database directory durable before plain data is
/// wiped. NTFS journals metadata, so Windows needs nothing here.
#[cfg(unix)]
fn sync_dir(path: &Path) -> Result<()> {
    let Some(dir) = path.parent() else {
        return Ok(());
    };
    std::fs::File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(|err| io_error("syncing", dir, err))
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn sync_dir(_path: &Path) -> Result<()> {
    Ok(())
}

/// Deletes an encrypted copy and the journal SQLite may have left beside it.
fn remove_copy(copy: &Path) -> Result<()> {
    remove_file(copy)?;
    for suffix in ["-wal", "-shm"] {
        remove_file(&sibling(copy, suffix))?;
    }
    Ok(())
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
        found.extend(plain_backups(path).expect("plain backups"));
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
    async fn stale_journals_of_an_old_copy_are_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        let db = plain_database(&path).await;
        let copy = sibling(&path, ".encrypting");
        std::fs::write(sibling(&copy, "-wal"), b"stale").expect("stale wal");
        std::fs::write(sibling(&copy, "-shm"), b"stale").expect("stale shm");
        db.encrypt_in_place(KEY).await.expect("encrypt");
        assert!(!sibling(&copy, "-wal").exists());
        assert!(!sibling(&copy, "-shm").exists());
        assert_eq!(conversations(&db).await, 1);
    }

    #[tokio::test]
    async fn a_closed_database_fails_every_call() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = plain_database(&dir.path().join("jarvis.db")).await;
        db.closed.store(true, std::sync::atomic::Ordering::Release);
        let err = db
            .call(|conn| Ok::<_, StorageError>(conn.execute("SELECT 1", [])?))
            .await
            .expect_err("closed");
        assert!(matches!(err, StorageError::Internal(_)), "{err:?}");
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
        let db = plain_database(&path).await;
        db.export_encrypted(KEY).await.expect("export");
        db.switch_to_encrypted(KEY).await.expect("switch");
        drop(db);
        drop(plain_database(&old).await);
        let backup = sibling(&path, ".v4-1.bak");
        drop(plain_database(&backup).await);
        assert_eq!(file_state(&old).expect("state"), FileState::Plain);

        for key in [None, Some("wrong key".to_owned())] {
            let error = Database::open_with_key(&path, key)
                .await
                .expect_err("locked");
            assert!(matches!(error, StorageError::Locked));
            assert_eq!(file_state(&old).expect("fallback"), FileState::Plain);
            assert_eq!(file_state(&backup).expect("backup"), FileState::Plain);
        }
        let reopened = Database::open_with_key(&path, Some(KEY.to_owned()))
            .await
            .expect("open with the key");
        assert_eq!(conversations(&reopened).await, 1);
        assert!(leftovers(&path).is_empty(), "{:?}", leftovers(&path));
    }

    #[tokio::test]
    async fn a_keyed_file_without_jarvis_schema_cannot_replace_the_plain_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        let old = sibling(&path, ".plain-old");
        drop(plain_database(&old).await);
        let connection = Connection::open(&path).expect("open");
        connection.pragma_update(None, "key", KEY).expect("key");
        connection
            .execute_batch("CREATE TABLE unrelated (id INTEGER)")
            .expect("create table");
        drop(connection);

        Database::open_with_key(&path, Some(KEY.to_owned()))
            .await
            .expect_err("replacement must already contain the JARVIS schema");
        let fallback = Database::open(&old).await.expect("plain fallback");
        assert_eq!(conversations(&fallback).await, 1);
    }

    #[tokio::test]
    async fn a_stop_between_the_renames_finishes_the_switch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jarvis.db");
        let db = plain_database(&path).await;
        db.export_encrypted(KEY).await.expect("export");
        drop(db);
        let old = sibling(&path, ".plain-old");
        std::fs::rename(&path, &old).expect("first rename");

        let error = Database::open(&path)
            .await
            .expect_err("locked before promotion");
        assert!(matches!(error, StorageError::Locked));
        assert!(!path.exists());
        assert_eq!(file_state(&old).expect("fallback"), FileState::Plain);
        assert_eq!(
            file_state(&sibling(&path, ".encrypting")).expect("copy"),
            FileState::Encrypted
        );
        let copy_wal = sibling(&path, ".encrypting-wal");
        std::fs::write(&copy_wal, b"uncheckpointed recovery data").expect("write WAL marker");
        let error = Database::open_with_key(&path, Some(KEY.to_owned()))
            .await
            .expect_err("a copy that needs its WAL cannot be promoted alone");
        assert!(error.to_string().contains("write-ahead log"));
        assert!(!path.exists());
        assert!(old.exists());
        assert!(copy_wal.exists());
        std::fs::remove_file(copy_wal).expect("remove the simulated WAL");

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
