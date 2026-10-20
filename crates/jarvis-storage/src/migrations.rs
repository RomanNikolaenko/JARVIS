//! Versioned schema migrations.
//!
//! Every migration runs once, in order, inside its own transaction, and is
//! recorded with a checksum of its SQL. Opening a database fails when it was
//! migrated by a newer build (no silent downgrade) or when an applied
//! migration's SQL changed (migrations are append-only). Before migrating a
//! database that already holds data, a copy is written next to it.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

use crate::error::{Result, StorageError};

/// One schema change.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// All migrations of this build, in order. Append only: never edit or
/// remove an entry that was released.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "memory",
        sql: include_str!("../migrations/0001_memory.sql"),
    },
    Migration {
        version: 2,
        name: "agents",
        sql: include_str!("../migrations/0002_agents.sql"),
    },
];

/// Schema version this build creates.
pub const SCHEMA_VERSION: u32 = 2;

/// What happened while opening a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub schema_version: u32,
    /// Versions applied while opening.
    pub applied: Vec<u32>,
    /// Copy written before migrating existing data.
    pub backup: Option<PathBuf>,
}

pub(crate) fn migrate(
    conn: &mut Connection,
    path: Option<&Path>,
    migrations: &[Migration],
) -> Result<MigrationReport> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY,
             name       TEXT NOT NULL,
             checksum   TEXT NOT NULL,
             applied_at INTEGER NOT NULL
         )",
    )?;
    let applied: Vec<(u32, String)> = {
        let mut statement =
            conn.prepare("SELECT version, checksum FROM schema_migrations ORDER BY version")?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };

    let supported = migrations.last().map_or(0, |m| m.version);
    let current = applied.last().map_or(0, |(version, _)| *version);
    if current > supported {
        return Err(StorageError::NewerSchema {
            found: current,
            supported,
        });
    }
    for (index, (version, recorded)) in applied.iter().enumerate() {
        let known = migrations.get(index).filter(|m| m.version == *version);
        match known {
            Some(migration) if checksum(migration.sql) == *recorded => {}
            Some(_) => return Err(StorageError::MigrationChanged { version: *version }),
            None => {
                return Err(StorageError::Internal(format!(
                    "the database records migration {version}, which this build does not have at that position"
                )));
            }
        }
    }

    let pending = &migrations[applied.len()..];
    let mut report = MigrationReport {
        schema_version: current,
        applied: Vec::new(),
        backup: None,
    };
    if pending.is_empty() {
        return Ok(report);
    }
    if current > 0
        && let Some(path) = path
    {
        report.backup = Some(backup(conn, path, current)?);
    }
    for migration in pending {
        let tx = conn.transaction()?;
        tx.execute_batch(migration.sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, checksum, applied_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                migration.version,
                migration.name,
                checksum(migration.sql),
                now_ms()
            ],
        )?;
        tx.pragma_update(None, "user_version", migration.version)?;
        tx.commit()?;
        tracing::info!(
            "database migrated to version {} ({})",
            migration.version,
            migration.name
        );
        report.applied.push(migration.version);
        report.schema_version = migration.version;
    }
    Ok(report)
}

/// Writes a consistent copy of the database next to it (`VACUUM INTO`).
fn backup(conn: &Connection, path: &Path, version: u32) -> Result<PathBuf> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "jarvis.db".to_owned());
    let target = path.with_file_name(format!("{name}.v{version}-{}.bak", now_ms()));
    conn.execute("VACUUM INTO ?1", [target.to_string_lossy()])?;
    tracing::info!("database backed up to {}", target.display());
    Ok(target)
}

/// SHA-256 of the SQL with line endings normalized, so a checkout with CRLF
/// line endings produces the same checksum.
pub(crate) fn checksum(sql: &str) -> String {
    let normalized = sql.replace("\r\n", "\n");
    Sha256::digest(normalized.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: Migration = Migration {
        version: 1,
        name: "one",
        sql: "CREATE TABLE a (x INTEGER);",
    };
    const TWO: Migration = Migration {
        version: 2,
        name: "two",
        sql: "CREATE TABLE b (y INTEGER); INSERT INTO b VALUES (1);",
    };
    const BROKEN: Migration = Migration {
        version: 2,
        name: "broken",
        sql: "CREATE TABLE c (z INTEGER); THIS IS NOT SQL;",
    };

    fn tables(conn: &Connection) -> Vec<String> {
        let mut statement = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .expect("prepare");
        statement
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows")
    }

    #[test]
    fn the_embedded_migrations_are_consistent() {
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(migration.version as usize, index + 1);
        }
        assert_eq!(MIGRATIONS.last().map(|m| m.version), Some(SCHEMA_VERSION));
        assert_eq!(checksum("a\r\nb"), checksum("a\nb"));
    }

    #[test]
    fn applies_pending_migrations_once_and_backs_up_existing_data() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.db");
        let mut conn = Connection::open(&path).expect("open");

        let first = migrate(&mut conn, Some(&path), &[ONE]).expect("first");
        assert_eq!(first.applied, vec![1]);
        assert!(first.backup.is_none(), "a new database needs no backup");

        let again = migrate(&mut conn, Some(&path), &[ONE]).expect("again");
        assert!(again.applied.is_empty());

        let upgrade = migrate(&mut conn, Some(&path), &[ONE, TWO]).expect("upgrade");
        assert_eq!(upgrade.applied, vec![2]);
        assert_eq!(upgrade.schema_version, 2);
        let backup = upgrade.backup.expect("backup before migrating data");
        let copy = Connection::open(&backup).expect("open backup");
        assert!(tables(&copy).contains(&"a".to_owned()));
        assert!(!tables(&copy).contains(&"b".to_owned()));
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("user_version");
        assert_eq!(version, 2);
    }

    #[test]
    fn a_failed_migration_changes_nothing() {
        let mut conn = Connection::open_in_memory().expect("open");
        migrate(&mut conn, None, &[ONE]).expect("first");
        assert!(migrate(&mut conn, None, &[ONE, BROKEN]).is_err());
        assert!(!tables(&conn).contains(&"c".to_owned()));
        let report = migrate(&mut conn, None, &[ONE]).expect("still at version 1");
        assert_eq!(report.schema_version, 1);
    }

    #[test]
    fn newer_and_changed_schemas_are_refused() {
        let mut conn = Connection::open_in_memory().expect("open");
        migrate(&mut conn, None, &[ONE, TWO]).expect("migrate");
        assert!(matches!(
            migrate(&mut conn, None, &[ONE]),
            Err(StorageError::NewerSchema {
                found: 2,
                supported: 1
            })
        ));

        let edited = Migration {
            sql: "CREATE TABLE a (x INTEGER, extra TEXT);",
            ..ONE
        };
        assert!(matches!(
            migrate(&mut conn, None, &[edited, TWO]),
            Err(StorageError::MigrationChanged { version: 1 })
        ));
    }
}
