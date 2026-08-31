use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    compact::{decode_compact_snapshot, encode_compact_snapshot, legacy_compact_snapshot_version},
    domain::ChainSnapshot,
};

#[cfg(feature = "fuzzing")]
pub fn fuzz_decode_compact_snapshot(bytes: &[u8]) -> Result<ChainSnapshot> {
    decode_compact_snapshot(bytes)
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS chain_snapshots (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    height INTEGER NOT NULL,
    tip_hash TEXT NOT NULL,
    snapshot_blob BLOB NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
"#;

#[derive(Clone, Debug)]
pub struct SqliteChainStore {
    path: PathBuf,
}

impl SqliteChainStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create chain database directory {}",
                    parent.display()
                )
            })?;
        }

        if let Some(reason) = legacy_chain_reason(&path)? {
            let archive_path = archive_legacy_chain(&path)?;
            println!(
                "archived incompatible chain database ({reason}): {} -> {}",
                path.display(),
                archive_path.display()
            );
        }

        let store = Self { path };
        store.with_connection_mut(|connection| {
            connection
                .execute_batch(SCHEMA)
                .context("failed to initialize chain database schema")?;
            Ok(())
        })?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<ChainSnapshot>> {
        self.with_connection(|connection| {
            let stored = connection
                .query_row(
                    "SELECT height, tip_hash, snapshot_blob FROM chain_snapshots WHERE id = 1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, u64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Vec<u8>>(2)?,
                        ))
                    },
                )
                .optional()
                .context("failed to load chain snapshot from database")?;

            stored
                .map(|(stored_height, stored_tip_hash, blob)| {
                    let snapshot = decode_compact_snapshot(&blob)
                        .context("failed to parse compact chain snapshot from database")?;
                    let (height, tip_hash) = snapshot_tip(&snapshot)
                        .context("compact chain snapshot contains no blocks")?;
                    if height != stored_height || tip_hash != stored_tip_hash {
                        anyhow::bail!(
                            "compact chain snapshot tip does not match database metadata"
                        );
                    }
                    Ok(snapshot)
                })
                .transpose()
        })
    }

    pub fn save(&self, snapshot: &ChainSnapshot) -> Result<()> {
        let (height, tip_hash) = snapshot_tip(snapshot).context("cannot persist empty chain")?;
        let snapshot_blob =
            encode_compact_snapshot(snapshot).context("failed to encode compact chain snapshot")?;
        let updated_at_ms = unix_ms();

        self.with_connection_mut(|connection| {
            let transaction = connection
                .transaction()
                .context("failed to start chain persistence transaction")?;
            transaction
                .execute(
                    r#"
INSERT INTO chain_snapshots (id, height, tip_hash, snapshot_blob, updated_at_ms)
VALUES (1, ?1, ?2, ?3, ?4)
ON CONFLICT(id) DO UPDATE SET
    height = excluded.height,
    tip_hash = excluded.tip_hash,
    snapshot_blob = excluded.snapshot_blob,
    updated_at_ms = excluded.updated_at_ms
"#,
                    params![height, tip_hash, snapshot_blob, updated_at_ms],
                )
                .context("failed to persist chain snapshot")?;
            transaction
                .commit()
                .context("failed to commit chain persistence transaction")?;
            Ok(())
        })
    }

    pub fn clear_chain(&self) -> Result<()> {
        self.with_connection_mut(|connection| {
            let transaction = connection
                .transaction()
                .context("failed to start chain reset transaction")?;
            transaction
                .execute("DELETE FROM chain_snapshots", [])
                .context("failed to delete chain snapshot")?;
            transaction
                .commit()
                .context("failed to commit chain reset transaction")?;
            Ok(())
        })
    }

    fn with_connection<T>(&self, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let connection = self.open_connection()?;
        connection
            .execute_batch(
                r#"
PRAGMA busy_timeout = 5000;
PRAGMA synchronous = NORMAL;
"#,
            )
            .context("failed to configure chain database connection")?;
        work(&connection)
    }

    fn with_connection_mut<T>(&self, work: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut connection = self.open_connection()?;
        connection
            .execute_batch(
                r#"
PRAGMA journal_mode = WAL;
PRAGMA busy_timeout = 5000;
PRAGMA synchronous = NORMAL;
"#,
            )
            .context("failed to configure chain database connection")?;
        work(&mut connection)
    }

    fn open_connection(&self) -> Result<Connection> {
        Connection::open(&self.path)
            .with_context(|| format!("failed to open chain database {}", self.path.display()))
    }
}

fn legacy_chain_reason(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }

    let connection = Connection::open(path)
        .with_context(|| format!("failed to inspect chain database {}", path.display()))?;
    connection
        .execute_batch("PRAGMA busy_timeout = 5000;")
        .context("failed to configure chain database inspection")?;

    let table_exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'chain_snapshots'",
            [],
            |_| Ok(()),
        )
        .optional()
        .context("failed to inspect chain database schema")?
        .is_some();
    if !table_exists {
        return Ok(None);
    }

    let mut columns = connection
        .prepare("SELECT name FROM pragma_table_info('chain_snapshots')")
        .context("failed to inspect chain snapshot columns")?;
    let columns = columns
        .query_map([], |row| row.get::<_, String>(0))
        .context("failed to read chain snapshot columns")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read chain snapshot columns")?;
    let has_snapshot_blob = columns.iter().any(|column| column == "snapshot_blob");
    if !has_snapshot_blob && columns.iter().any(|column| column == "snapshot_json") {
        return Ok(Some("legacy JSON snapshot schema".to_string()));
    }
    if !has_snapshot_blob {
        return Ok(None);
    }

    let snapshot_blob = connection
        .query_row(
            "SELECT snapshot_blob FROM chain_snapshots WHERE id = 1",
            [],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .context("failed to inspect compact chain snapshot version")?;
    Ok(snapshot_blob
        .as_deref()
        .and_then(legacy_compact_snapshot_version)
        .map(|version| format!("compact snapshot version {version}")))
}

fn archive_legacy_chain(path: &Path) -> Result<PathBuf> {
    // Fold committed WAL contents into the database before moving the archive.
    // Renaming any remaining sidecars first prevents them from being attached to
    // the fresh database if startup is interrupted between the moves.
    let connection = Connection::open(path)
        .with_context(|| format!("failed to prepare legacy chain database {}", path.display()))?;
    connection
        .execute_batch("PRAGMA busy_timeout = 5000; PRAGMA wal_checkpoint(TRUNCATE);")
        .context("failed to checkpoint legacy chain database before archiving")?;
    drop(connection);

    let archive_path = available_archive_path(path);
    for suffix in ["-wal", "-shm"] {
        let source = path_with_suffix(path, suffix);
        if source.exists() {
            let destination = path_with_suffix(&archive_path, suffix);
            fs::rename(&source, &destination).with_context(|| {
                format!(
                    "failed to archive legacy chain sidecar {} as {}",
                    source.display(),
                    destination.display()
                )
            })?;
        }
    }
    fs::rename(path, &archive_path).with_context(|| {
        format!(
            "failed to archive legacy chain database {} as {}",
            path.display(),
            archive_path.display()
        )
    })?;
    Ok(archive_path)
}

fn available_archive_path(path: &Path) -> PathBuf {
    for index in 0_u64.. {
        let suffix = if index == 0 {
            ".pre-v6".to_string()
        } else {
            format!(".pre-v6.{index}")
        };
        let candidate = path_with_suffix(path, &suffix);
        if !candidate.exists()
            && !path_with_suffix(&candidate, "-wal").exists()
            && !path_with_suffix(&candidate, "-shm").exists()
        {
            return candidate;
        }
    }
    unreachable!("archive suffix counter exhausted")
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn snapshot_tip(snapshot: &ChainSnapshot) -> Option<(u64, String)> {
    snapshot
        .blocks
        .last()
        .map(|block| (block.height, block.hash.clone()))
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use rusqlite::Connection;
    use tempfile::tempdir;

    use crate::domain::{ChainSnapshot, GenesisBurn, Ledger, Wallet};

    use super::{SCHEMA, SqliteChainStore};

    const LEGACY_JSON_SCHEMA: &str = r#"
CREATE TABLE chain_snapshots (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    height INTEGER NOT NULL,
    tip_hash TEXT NOT NULL,
    snapshot_json TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
INSERT INTO chain_snapshots (id, height, tip_hash, snapshot_json, updated_at_ms)
VALUES (1, 0, 'legacy-tip', '{}', 0);
"#;

    fn test_snapshot(seed: &str) -> ChainSnapshot {
        let wallet = Wallet::from_seed(seed);
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), 1);
        Ledger::new_with_genesis_burns(allocations, vec![GenesisBurn::new(wallet.address(), 1)], 1)
            .unwrap()
            .snapshot()
    }

    #[test]
    fn open_archives_legacy_json_schema_and_creates_fresh_database() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("chain.sqlite3");
        Connection::open(&path)
            .unwrap()
            .execute_batch(LEGACY_JSON_SCHEMA)
            .unwrap();

        let store = SqliteChainStore::open(&path).unwrap();

        assert!(store.load().unwrap().is_none());
        let archive = dir.path().join("chain.sqlite3.pre-v6");
        assert!(archive.exists());
        let legacy_json: String = Connection::open(archive)
            .unwrap()
            .query_row(
                "SELECT snapshot_json FROM chain_snapshots WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_json, "{}");
    }

    #[test]
    fn open_archives_legacy_compact_snapshot_and_uses_unique_name() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("chain.sqlite3");
        let existing_archive = dir.path().join("chain.sqlite3.pre-v6");
        fs::write(&existing_archive, b"existing archive").unwrap();
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        let mut legacy_blob = b"IUNA-SNAPSHOT".to_vec();
        legacy_blob.push(5);
        connection
            .execute(
                r#"
INSERT INTO chain_snapshots (id, height, tip_hash, snapshot_blob, updated_at_ms)
VALUES (1, 0, 'legacy-tip', ?1, 0)
"#,
                [&legacy_blob],
            )
            .unwrap();
        drop(connection);

        let store = SqliteChainStore::open(&path).unwrap();

        assert!(store.load().unwrap().is_none());
        assert_eq!(fs::read(existing_archive).unwrap(), b"existing archive");
        let archived_blob: Vec<u8> = Connection::open(dir.path().join("chain.sqlite3.pre-v6.1"))
            .unwrap()
            .query_row(
                "SELECT snapshot_blob FROM chain_snapshots WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(archived_blob, legacy_blob);
    }

    #[test]
    fn open_does_not_archive_corrupt_or_future_compact_snapshots() {
        for (name, blob) in [
            ("corrupt", vec![0, 1, 2, 3]),
            ("future", [b"IUNA-SNAPSHOT".as_slice(), &[8]].concat()),
        ] {
            let dir = tempdir().unwrap();
            let path = dir.path().join(format!("{name}.sqlite3"));
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch(SCHEMA).unwrap();
            connection
                .execute(
                    r#"
INSERT INTO chain_snapshots (id, height, tip_hash, snapshot_blob, updated_at_ms)
VALUES (1, 0, 'bad-tip', ?1, 0)
"#,
                    [&blob],
                )
                .unwrap();
            drop(connection);

            let store = SqliteChainStore::open(&path).unwrap();

            assert!(store.load().is_err());
            assert!(!dir.path().join(format!("{name}.sqlite3.pre-v6")).exists());
        }
    }

    #[test]
    fn failed_snapshot_save_keeps_last_committed_chain() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let snapshot = test_snapshot("chain-store-rollback");
        let tip = snapshot.blocks.last().unwrap().hash.clone();
        store.save(&snapshot).unwrap();

        let mut invalid = snapshot.clone();
        invalid.blocks.clear();
        let error = store.save(&invalid).unwrap_err();

        assert!(error.to_string().contains("cannot persist empty chain"));
        let restored = store.load().unwrap().unwrap();
        assert_eq!(restored.blocks.last().unwrap().hash, tip);
    }

    #[test]
    fn load_rejects_snapshot_that_does_not_match_stored_tip_metadata() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        store
            .save(&test_snapshot("chain-store-tip-integrity"))
            .unwrap();
        store
            .with_connection_mut(|connection| {
                connection.execute(
                    "UPDATE chain_snapshots SET tip_hash = ?1 WHERE id = 1",
                    ["0".repeat(64)],
                )?;
                Ok(())
            })
            .unwrap();

        let error = store.load().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("snapshot tip does not match database metadata")
        );
    }
}
