use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::domain::ChainSnapshot;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS chain_snapshots (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    height INTEGER NOT NULL,
    tip_hash TEXT NOT NULL,
    snapshot_json TEXT NOT NULL,
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

        let store = Self { path };
        store.with_connection(|connection| {
            connection
                .execute_batch(SCHEMA)
                .context("failed to initialize chain database schema")
        })?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<ChainSnapshot>> {
        self.with_connection(|connection| {
            let snapshot_json = connection
                .query_row(
                    "SELECT snapshot_json FROM chain_snapshots WHERE id = 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .context("failed to load chain snapshot from database")?;

            snapshot_json
                .map(|json| {
                    serde_json::from_str(&json)
                        .context("failed to parse chain snapshot from database")
                })
                .transpose()
        })
    }

    pub fn save(&self, snapshot: &ChainSnapshot) -> Result<()> {
        let (height, tip_hash) = snapshot_tip(snapshot).context("cannot persist empty chain")?;
        let snapshot_json =
            serde_json::to_string(snapshot).context("failed to serialize chain snapshot")?;
        let updated_at_ms = unix_ms();

        self.with_connection(|connection| {
            connection
                .execute(
                    r#"
INSERT INTO chain_snapshots (id, height, tip_hash, snapshot_json, updated_at_ms)
VALUES (1, ?1, ?2, ?3, ?4)
ON CONFLICT(id) DO UPDATE SET
    height = excluded.height,
    tip_hash = excluded.tip_hash,
    snapshot_json = excluded.snapshot_json,
    updated_at_ms = excluded.updated_at_ms
"#,
                    params![height, tip_hash, snapshot_json, updated_at_ms],
                )
                .context("failed to persist chain snapshot")?;
            Ok(())
        })
    }

    fn with_connection<T>(&self, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let connection = Connection::open(&self.path)
            .with_context(|| format!("failed to open chain database {}", self.path.display()))?;
        connection
            .execute_batch(
                r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
"#,
            )
            .context("failed to configure chain database")?;
        work(&connection)
    }
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
    use std::collections::BTreeMap;

    use tempfile::tempdir;

    use crate::domain::{GenesisBurn, Ledger, Wallet};

    use super::SqliteChainStore;

    #[test]
    fn sqlite_chain_store_roundtrips_snapshot() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("nested/chain.sqlite3")).unwrap();
        let wallet = Wallet::from_seed("alice");
        let mut genesis = BTreeMap::new();
        genesis.insert(wallet.address().to_string(), 1);
        let ledger =
            Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
                .unwrap();

        store.save(&ledger.snapshot()).unwrap();

        assert_eq!(store.load().unwrap(), Some(ledger.snapshot()));
    }

    #[test]
    fn sqlite_chain_store_overwrites_latest_snapshot() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let wallet = Wallet::from_seed("alice");
        let mut genesis = BTreeMap::new();
        genesis.insert(wallet.address().to_string(), 2);
        let mut ledger =
            Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
                .unwrap();
        store.save(&ledger.snapshot()).unwrap();

        let burn = wallet.burn(1, ledger.next_nonce(wallet.address()));
        ledger.submit_transaction(burn).unwrap();
        let block = ledger.mine_next_block(wallet.address(), 1_000).unwrap();
        ledger.apply_locally_mined_block(block).unwrap();
        store.save(&ledger.snapshot()).unwrap();

        let restored = store.load().unwrap().unwrap();
        assert_eq!(restored.blocks.last().unwrap().height, 1);
        assert_eq!(restored, ledger.snapshot());
    }

    #[test]
    fn sqlite_chain_store_reports_invalid_snapshot_json() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        store
            .with_connection(|connection| {
                connection.execute(
                    r#"
INSERT INTO chain_snapshots (id, height, tip_hash, snapshot_json, updated_at_ms)
VALUES (1, 9, 'bad-tip', '{"not":"a chain"}', 0)
"#,
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let error = store.load().unwrap_err();

        assert!(
            format!("{error:#}").contains("failed to parse chain snapshot from database"),
            "{error:#}"
        );
    }
}
