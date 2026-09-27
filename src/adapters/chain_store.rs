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
CREATE TABLE IF NOT EXISTS chain_verification (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    tip_hash TEXT NOT NULL,
    verifier_version TEXT NOT NULL,
    verified_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS pending_transactions_v2 (
    transaction_id TEXT PRIMARY KEY,
    envelope TEXT NOT NULL,
    position INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
"#;

// This identifies the consensus rules, not the application release. UI, packaging, and
// other non-consensus releases must not invalidate a chain that this node already verified.
// Bump this value only when historical validation semantics change, and add the old ruleset to
// `revalidation_from_height` with the first affected block height.
const CURRENT_CONSENSUS_RULESET: &str = "iuna-consensus-v2";

// v0.4.10 introduced the verification marker and wrote the package version into it. Its
// validator is identical to the first stable consensus ruleset, so it follows that ruleset's
// migration boundary.
const LEGACY_EQUIVALENT_VERIFIER_VERSIONS: &[&str] = &["0.4.10"];

struct ConsensusRulesetMigration {
    from_ruleset: &'static str,
    revalidate_from_height: u64,
}

// When a future release changes consensus validation, bump CURRENT_CONSENSUS_RULESET and add a
// direct migration for every still-supported older ruleset. The height is the first block whose
// validity can differ under the new rules.
const CONSENSUS_RULESET_MIGRATIONS: &[ConsensusRulesetMigration] = &[ConsensusRulesetMigration {
    from_ruleset: "iuna-consensus-v1",
    revalidate_from_height:
        crate::domain::TRANSACTION_V2_AUTHORIZATION_AGGREGATION_ACTIVATION_HEIGHT,
}];

#[derive(Clone, Debug)]
pub struct SqliteChainStore {
    path: PathBuf,
}

#[derive(Debug)]
pub struct LoadedChainSnapshot {
    pub snapshot: ChainSnapshot,
    /// First block that must be validated again. `None` means the entire persisted chain is
    /// already trusted under the current consensus ruleset.
    pub revalidation_from_height: Option<u64>,
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
        Ok(self
            .load_with_verification_status()?
            .map(|loaded| loaded.snapshot))
    }

    pub fn contains_chain(&self) -> Result<bool> {
        self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM chain_snapshots WHERE id = 1)",
                    [],
                    |row| row.get(0),
                )
                .context("failed to inspect chain database")
        })
    }

    pub fn load_pending_transactions_v2(&self) -> Result<Vec<(String, String)>> {
        self.with_connection(|connection| {
            let mut statement = connection
                .prepare(
                    r#"
SELECT transaction_id, envelope
FROM pending_transactions_v2
ORDER BY position ASC
"#,
                )
                .context("failed to prepare pending transaction v2 query")?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .context("failed to load pending transactions v2")?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .context("failed to read pending transaction v2 rows")
        })
    }

    pub fn save_pending_transaction_v2(&self, transaction_id: &str, envelope: &str) -> Result<()> {
        self.with_connection_mut(|connection| {
            connection
                .execute(
                    r#"
INSERT INTO pending_transactions_v2 (transaction_id, envelope, position, updated_at_ms)
VALUES (
    ?1,
    ?2,
    COALESCE((SELECT MAX(position) + 1 FROM pending_transactions_v2), 0),
    ?3
)
ON CONFLICT(transaction_id) DO UPDATE SET
    envelope = excluded.envelope,
    updated_at_ms = excluded.updated_at_ms
"#,
                    params![transaction_id, envelope, unix_ms()],
                )
                .context("failed to persist pending transaction v2")?;
            Ok(())
        })
    }

    pub fn replace_pending_transactions_v2(&self, rows: &[(String, String)]) -> Result<()> {
        let updated_at_ms = unix_ms();
        self.with_connection_mut(|connection| {
            let transaction = connection
                .transaction()
                .context("failed to start pending transaction v2 persistence transaction")?;
            transaction
                .execute("DELETE FROM pending_transactions_v2", [])
                .context("failed to clear pending transactions v2")?;
            for (position, (transaction_id, envelope)) in rows.iter().enumerate() {
                transaction
                    .execute(
                        r#"
INSERT INTO pending_transactions_v2 (
    transaction_id, envelope, position, updated_at_ms
)
VALUES (?1, ?2, ?3, ?4)
"#,
                        params![transaction_id, envelope, position, updated_at_ms],
                    )
                    .with_context(|| {
                        format!("failed to persist pending transaction v2 {transaction_id}")
                    })?;
            }
            transaction
                .commit()
                .context("failed to commit pending transaction v2 persistence transaction")?;
            Ok(())
        })
    }

    pub fn load_with_verification_status(&self) -> Result<Option<LoadedChainSnapshot>> {
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

            let snapshot = stored
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
                    Ok((snapshot, tip_hash))
                })
                .transpose()?;
            let Some((snapshot, tip_hash)) = snapshot else {
                return Ok(None);
            };
            let stored_ruleset = connection
                .query_row(
                    r#"
SELECT verifier_version FROM chain_verification
WHERE id = 1 AND tip_hash = ?1
"#,
                    params![tip_hash],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .context("failed to inspect chain verification status")?;
            Ok(Some(LoadedChainSnapshot {
                snapshot,
                revalidation_from_height: revalidation_from_height(stored_ruleset.as_deref()),
            }))
        })
    }

    pub fn save(&self, snapshot: &ChainSnapshot) -> Result<()> {
        self.save_with_verification_status(snapshot, false)
    }

    /// Persist a snapshot that has already passed consensus validation in this binary.
    pub fn save_verified(&self, snapshot: &ChainSnapshot) -> Result<()> {
        self.save_with_verification_status(snapshot, true)
    }

    fn save_with_verification_status(
        &self,
        snapshot: &ChainSnapshot,
        verified: bool,
    ) -> Result<()> {
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
            if verified {
                transaction
                    .execute(
                        r#"
INSERT INTO chain_verification (id, tip_hash, verifier_version, verified_at_ms)
VALUES (1, ?1, ?2, ?3)
ON CONFLICT(id) DO UPDATE SET
    tip_hash = excluded.tip_hash,
    verifier_version = excluded.verifier_version,
    verified_at_ms = excluded.verified_at_ms
"#,
                        params![tip_hash, CURRENT_CONSENSUS_RULESET, updated_at_ms],
                    )
                    .context("failed to persist chain verification status")?;
            } else {
                transaction
                    .execute("DELETE FROM chain_verification", [])
                    .context("failed to clear chain verification status")?;
            }
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
                .execute("DELETE FROM chain_verification", [])
                .context("failed to delete chain verification status")?;
            transaction
                .execute("DELETE FROM pending_transactions_v2", [])
                .context("failed to delete pending transactions v2")?;
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

fn revalidation_from_height(stored_ruleset: Option<&str>) -> Option<u64> {
    match stored_ruleset {
        Some(CURRENT_CONSENSUS_RULESET) => None,
        Some(version) if LEGACY_EQUIVALENT_VERIFIER_VERSIONS.contains(&version) => {
            Some(crate::domain::TRANSACTION_V2_AUTHORIZATION_AGGREGATION_ACTIVATION_HEIGHT)
        }
        Some(ruleset) => CONSENSUS_RULESET_MIGRATIONS
            .iter()
            .find(|migration| migration.from_ruleset == ruleset)
            .map(|migration| migration.revalidate_from_height)
            // Unknown markers are untrusted.
            .or(Some(1)),
        // A missing marker means the snapshot was not persisted as validated.
        None => Some(1),
    }
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
    fn pending_transaction_v2_journal_preserves_order_and_clears_with_chain() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        store
            .save_pending_transaction_v2("tx-b", "envelope-b")
            .unwrap();
        store
            .save_pending_transaction_v2("tx-a", "envelope-a")
            .unwrap();

        assert_eq!(
            store.load_pending_transactions_v2().unwrap(),
            vec![
                ("tx-b".to_string(), "envelope-b".to_string()),
                ("tx-a".to_string(), "envelope-a".to_string()),
            ]
        );

        store
            .replace_pending_transactions_v2(&[("tx-a".to_string(), "replacement-a".to_string())])
            .unwrap();
        assert_eq!(
            store.load_pending_transactions_v2().unwrap(),
            vec![("tx-a".to_string(), "replacement-a".to_string())]
        );

        store.clear_chain().unwrap();
        assert!(store.load_pending_transactions_v2().unwrap().is_empty());
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

    #[test]
    fn verified_snapshot_is_trusted_only_for_current_ruleset_and_tip() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let snapshot = test_snapshot("chain-store-verification-status");
        store.save_verified(&snapshot).unwrap();

        let loaded = store.load_with_verification_status().unwrap().unwrap();
        assert_eq!(loaded.revalidation_from_height, None);

        store
            .with_connection_mut(|connection| {
                connection.execute(
                    "UPDATE chain_verification SET verifier_version = 'previous-version'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let loaded = store.load_with_verification_status().unwrap().unwrap();
        assert_eq!(loaded.revalidation_from_height, Some(1));

        store
            .with_connection_mut(|connection| {
                connection.execute(
                    "UPDATE chain_verification SET verifier_version = ?1, tip_hash = 'other-tip'",
                    [super::CURRENT_CONSENSUS_RULESET],
                )?;
                Ok(())
            })
            .unwrap();
        let loaded = store.load_with_verification_status().unwrap().unwrap();
        assert_eq!(loaded.revalidation_from_height, Some(1));
    }

    #[test]
    fn ordinary_save_invalidates_previous_verification_status() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let snapshot = test_snapshot("chain-store-unverified-save");
        store.save_verified(&snapshot).unwrap();
        assert!(
            store
                .load_with_verification_status()
                .unwrap()
                .unwrap()
                .revalidation_from_height
                .is_none()
        );

        store.save(&snapshot).unwrap();

        assert!(store.contains_chain().unwrap());
        assert!(
            store
                .load_with_verification_status()
                .unwrap()
                .unwrap()
                .revalidation_from_height
                .is_some()
        );
    }

    #[test]
    fn opening_database_without_verification_table_migrates_as_untrusted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("chain.sqlite3");
        let store = SqliteChainStore::open(&path).unwrap();
        let snapshot = test_snapshot("chain-store-verification-migration");
        store.save_verified(&snapshot).unwrap();
        drop(store);
        Connection::open(&path)
            .unwrap()
            .execute("DROP TABLE chain_verification", [])
            .unwrap();

        let reopened = SqliteChainStore::open(&path).unwrap();
        let loaded = reopened.load_with_verification_status().unwrap().unwrap();

        assert_eq!(loaded.snapshot, snapshot);
        assert_eq!(loaded.revalidation_from_height, Some(1));
    }

    #[test]
    fn v0410_verification_marker_revalidates_from_the_v2_authorization_fork() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let snapshot = test_snapshot("chain-store-legacy-ruleset-marker");
        store.save_verified(&snapshot).unwrap();
        store
            .with_connection_mut(|connection| {
                connection.execute(
                    "UPDATE chain_verification SET verifier_version = '0.4.10'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let loaded = store.load_with_verification_status().unwrap().unwrap();

        assert_eq!(
            loaded.revalidation_from_height,
            Some(crate::domain::TRANSACTION_V2_AUTHORIZATION_AGGREGATION_ACTIVATION_HEIGHT)
        );
    }

    #[test]
    fn consensus_v1_marker_revalidates_from_the_v2_authorization_fork() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let snapshot = test_snapshot("chain-store-v1-ruleset-marker");
        store.save_verified(&snapshot).unwrap();
        store
            .with_connection_mut(|connection| {
                connection.execute(
                    "UPDATE chain_verification SET verifier_version = 'iuna-consensus-v1'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let loaded = store.load_with_verification_status().unwrap().unwrap();

        assert_eq!(
            loaded.revalidation_from_height,
            Some(crate::domain::TRANSACTION_V2_AUTHORIZATION_AGGREGATION_ACTIVATION_HEIGHT)
        );
    }
}
