use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use serde::Serialize;

use crate::domain::{
    Amount, ChainSnapshot, Ledger, MINE_REWARD, Transaction, revealed_blinded_transactions,
};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS chain_snapshots (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    height INTEGER NOT NULL,
    tip_hash TEXT NOT NULL,
    snapshot_json TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS block_metrics (
    height INTEGER PRIMARY KEY,
    block_hash TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    block_time_ms INTEGER,
    mine_difficulty_bits INTEGER NOT NULL,
    circulating_supply INTEGER NOT NULL,
    transaction_count INTEGER NOT NULL,
    transfer_count INTEGER NOT NULL,
    burn_count INTEGER NOT NULL,
    mine_count INTEGER NOT NULL,
    burned_amount INTEGER NOT NULL,
    total_burned_amount INTEGER NOT NULL,
    fees_amount INTEGER NOT NULL,
    reward_amount INTEGER NOT NULL,
    vdf_rounds INTEGER NOT NULL,
    finalizer_rank INTEGER NOT NULL
);
"#;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockMetricRow {
    pub height: u64,
    pub block_hash: String,
    pub timestamp_ms: u64,
    pub block_time_ms: Option<u64>,
    pub mine_difficulty_bits: u32,
    pub circulating_supply: Amount,
    pub transaction_count: u64,
    pub transfer_count: u64,
    pub burn_count: u64,
    pub mine_count: u64,
    pub burned_amount: Amount,
    pub total_burned_amount: Amount,
    pub fees_amount: Amount,
    pub reward_amount: Amount,
    pub vdf_rounds: u64,
    pub finalizer_rank: u32,
}

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
        self.save_with_metrics(snapshot, false)
    }

    pub fn save_with_metrics(&self, snapshot: &ChainSnapshot, keep_metrics: bool) -> Result<()> {
        let (height, tip_hash) = snapshot_tip(snapshot).context("cannot persist empty chain")?;
        let snapshot_json =
            serde_json::to_string(snapshot).context("failed to serialize chain snapshot")?;
        let updated_at_ms = unix_ms();
        let metrics = if keep_metrics {
            Some(metrics_from_snapshot(snapshot)?)
        } else {
            None
        };

        self.with_connection_mut(|connection| {
            let transaction = connection
                .transaction()
                .context("failed to start chain persistence transaction")?;
            transaction
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
            match metrics {
                Some(metrics) => replace_metrics(&transaction, &metrics)?,
                None => clear_metrics_in_transaction(&transaction)?,
            }
            transaction
                .commit()
                .context("failed to commit chain persistence transaction")?;
            Ok(())
        })
    }

    pub fn replace_metrics_for_snapshot(&self, snapshot: &ChainSnapshot) -> Result<()> {
        let metrics = metrics_from_snapshot(snapshot)?;
        self.with_connection_mut(|connection| {
            let transaction = connection
                .transaction()
                .context("failed to start metrics transaction")?;
            replace_metrics(&transaction, &metrics)?;
            transaction
                .commit()
                .context("failed to commit metrics transaction")?;
            Ok(())
        })
    }

    pub fn clear_metrics(&self) -> Result<()> {
        self.with_connection(|connection| {
            connection
                .execute("DELETE FROM block_metrics", [])
                .context("failed to delete block metrics")?;
            Ok(())
        })
    }

    pub fn load_metrics(&self) -> Result<Vec<BlockMetricRow>> {
        self.with_connection(|connection| {
            let mut statement = connection
                .prepare(
                    r#"
SELECT height, block_hash, timestamp_ms, block_time_ms, mine_difficulty_bits,
       circulating_supply, transaction_count, transfer_count, burn_count,
       mine_count, burned_amount, total_burned_amount, fees_amount, reward_amount,
       vdf_rounds, finalizer_rank
FROM block_metrics
ORDER BY height ASC
"#,
                )
                .context("failed to prepare block metrics query")?;
            let rows = statement
                .query_map([], |row| {
                    Ok(BlockMetricRow {
                        height: row.get(0)?,
                        block_hash: row.get(1)?,
                        timestamp_ms: row.get(2)?,
                        block_time_ms: row.get(3)?,
                        mine_difficulty_bits: row.get(4)?,
                        circulating_supply: row.get(5)?,
                        transaction_count: row.get(6)?,
                        transfer_count: row.get(7)?,
                        burn_count: row.get(8)?,
                        mine_count: row.get(9)?,
                        burned_amount: row.get(10)?,
                        total_burned_amount: row.get(11)?,
                        fees_amount: row.get(12)?,
                        reward_amount: row.get(13)?,
                        vdf_rounds: row.get(14)?,
                        finalizer_rank: row.get(15)?,
                    })
                })
                .context("failed to load block metrics")?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .context("failed to read block metrics rows")
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

    fn with_connection_mut<T>(&self, work: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut connection = Connection::open(&self.path)
            .with_context(|| format!("failed to open chain database {}", self.path.display()))?;
        connection
            .execute_batch(
                r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
"#,
            )
            .context("failed to configure chain database")?;
        work(&mut connection)
    }
}

fn replace_metrics(
    transaction: &rusqlite::Transaction<'_>,
    metrics: &[BlockMetricRow],
) -> Result<()> {
    clear_metrics_in_transaction(transaction)?;
    for metric in metrics {
        transaction
            .execute(
                r#"
INSERT INTO block_metrics (
    height, block_hash, timestamp_ms, block_time_ms, mine_difficulty_bits,
    circulating_supply, transaction_count, transfer_count, burn_count, mine_count,
    burned_amount, total_burned_amount, fees_amount, reward_amount, vdf_rounds, finalizer_rank
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
"#,
                params![
                    metric.height,
                    metric.block_hash,
                    metric.timestamp_ms,
                    metric.block_time_ms,
                    metric.mine_difficulty_bits,
                    metric.circulating_supply,
                    metric.transaction_count,
                    metric.transfer_count,
                    metric.burn_count,
                    metric.mine_count,
                    metric.burned_amount,
                    metric.total_burned_amount,
                    metric.fees_amount,
                    metric.reward_amount,
                    metric.vdf_rounds,
                    metric.finalizer_rank,
                ],
            )
            .with_context(|| format!("failed to insert metrics for block {}", metric.height))?;
    }
    Ok(())
}

fn clear_metrics_in_transaction(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    transaction
        .execute("DELETE FROM block_metrics", [])
        .context("failed to clear old block metrics")?;
    Ok(())
}

fn metrics_from_snapshot(snapshot: &ChainSnapshot) -> Result<Vec<BlockMetricRow>> {
    let ledger = Ledger::from_persisted_snapshot(snapshot.clone())
        .context("failed to rebuild ledger for metrics")?;
    let revealed = revealed_blinded_transactions(snapshot)?.into_iter().fold(
        std::collections::BTreeMap::<u64, Vec<Transaction>>::new(),
        |mut by_height, revealed| {
            by_height
                .entry(revealed.height)
                .or_default()
                .push(revealed.transaction);
            by_height
        },
    );
    let mut circulating_supply =
        snapshot
            .genesis_allocations
            .values()
            .try_fold(0_u64, |total, amount| {
                total
                    .checked_add(*amount)
                    .context("genesis allocation total overflows")
            })?;
    let mut total_burned_amount = 0_u64;
    let mut rows = Vec::with_capacity(snapshot.blocks.len());
    let mut previous_timestamp_ms = None;

    for block in &snapshot.blocks {
        let revealed_transactions = revealed.get(&block.height).cloned().unwrap_or_default();
        let mut transfer_count = 0_u64;
        let mut burn_count = 0_u64;
        let mut mine_count = 0_u64;
        let mut burned_amount = 0_u64;
        let mut mine_issued_amount = 0_u64;
        let mut fees_amount = 0_u64;

        for transaction in block
            .transactions
            .iter()
            .chain(revealed_transactions.iter())
        {
            fees_amount = fees_amount
                .checked_add(transaction.fee())
                .context("block metric fees overflow")?;
            match transaction {
                Transaction::Transfer { .. } => transfer_count += 1,
                Transaction::Burn { amount, .. } => {
                    burn_count += 1;
                    burned_amount = burned_amount
                        .checked_add(*amount)
                        .context("block metric burns overflow")?;
                }
                Transaction::Mine { .. } => {
                    mine_count += 1;
                    mine_issued_amount = mine_issued_amount
                        .checked_add(MINE_REWARD)
                        .and_then(|amount| amount.checked_add(transaction.fee()))
                        .context("block metric mine issuance overflow")?;
                }
            }
        }
        total_burned_amount = total_burned_amount
            .checked_add(burned_amount)
            .context("total burned metric overflows")?;

        let issued_amount = block_issued_amount(block.height, block.reward, mine_issued_amount)
            .with_context(|| {
                format!("block metric issuance overflows at block {}", block.height)
            })?;
        circulating_supply = circulating_supply
            .checked_add(issued_amount)
            .with_context(|| {
                format!(
                    "circulating supply metric overflows while adding issuance at block {}",
                    block.height
                )
            })?;
        circulating_supply = circulating_supply
            .checked_sub(burned_amount)
            .with_context(|| {
                format!(
                    "circulating supply metric underflows while subtracting burns at block {}",
                    block.height
                )
            })?;
        let block_time_ms =
            previous_timestamp_ms.map(|previous| block.timestamp_ms.saturating_sub(previous));
        previous_timestamp_ms = Some(block.timestamp_ms);
        rows.push(BlockMetricRow {
            height: block.height,
            block_hash: block.hash.clone(),
            timestamp_ms: block.timestamp_ms,
            block_time_ms,
            mine_difficulty_bits: ledger.mine_difficulty_bits_at_height(block.height),
            circulating_supply,
            transaction_count: (block.transactions.len() + revealed_transactions.len()) as u64,
            transfer_count,
            burn_count,
            mine_count,
            burned_amount,
            total_burned_amount,
            fees_amount,
            reward_amount: block.reward,
            vdf_rounds: block.vdf_rounds,
            finalizer_rank: block.finalizer_rank,
        });
    }
    Ok(rows)
}

fn block_issued_amount(
    height: u64,
    reward_amount: Amount,
    mine_issued_amount: Amount,
) -> Option<Amount> {
    let genesis_reward = if height == 0 { reward_amount } else { 0 };
    mine_issued_amount.checked_add(genesis_reward)
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

    use crate::domain::{BLOCK_REWARD, GenesisBurn, Ledger, Wallet};

    use super::{BlockMetricRow, SqliteChainStore, replace_metrics};

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

        let burn = ledger.build_burn(&wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let block = ledger.mine_next_block(&wallet, 1_000).unwrap();
        ledger.apply_locally_mined_block(block).unwrap();
        store.save(&ledger.snapshot()).unwrap();

        let restored = store.load().unwrap().unwrap();
        assert_eq!(restored.blocks.last().unwrap().height, 1);
        assert_eq!(restored, ledger.snapshot());
    }

    #[test]
    fn sqlite_chain_store_saves_and_clears_block_metrics() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let wallet = Wallet::from_seed("metrics-alice");
        let mut genesis = BTreeMap::new();
        genesis.insert(wallet.address().to_string(), 10);
        let mut ledger =
            Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
                .unwrap();
        let burn = ledger.build_burn(&wallet, 2, 1).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let block = ledger.mine_next_block(&wallet, 1_000).unwrap();
        ledger.apply_locally_mined_block(block).unwrap();

        store.save_with_metrics(&ledger.snapshot(), true).unwrap();
        let metrics = store.load_metrics().unwrap();

        assert_eq!(metrics.last().unwrap().height, 1);
        assert_eq!(metrics.last().unwrap().burn_count, 1);
        assert_eq!(metrics.last().unwrap().burned_amount, 2);
        assert_eq!(metrics.last().unwrap().fees_amount, 1);
        assert_eq!(metrics.last().unwrap().circulating_supply, BLOCK_REWARD + 7);

        store.clear_metrics().unwrap();
        assert!(store.load_metrics().unwrap().is_empty());
    }

    #[test]
    fn sqlite_chain_store_metrics_include_revealed_blinded_burns() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let alice = Wallet::from_seed("metrics-blinded-alice");
        let bob = Wallet::from_seed("metrics-blinded-bob");
        let carol = Wallet::from_seed("metrics-blinded-carol");
        let wallets = [alice.clone(), bob.clone()];
        let mut genesis = BTreeMap::new();
        genesis.insert(alice.address().to_string(), 10_000_000);
        genesis.insert(bob.address().to_string(), 10_000_000);
        genesis.insert(carol.address().to_string(), 10_000_000);
        let mut ledger = Ledger::new_with_genesis_burns(
            genesis,
            vec![
                GenesisBurn::new(alice.address(), 1_000_000),
                GenesisBurn::new(bob.address(), 1_000_000),
            ],
            1,
        )
        .unwrap();
        let blinded = ledger
            .build_blinded_burn(&carol, 3, 7, ledger.height() + 4)
            .unwrap();
        ledger
            .submit_blinded_transaction(blinded.transaction)
            .unwrap();
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let wallet = wallets
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap();
        let burn = ledger.build_burn(wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let block = ledger.mine_next_block(wallet, 1).unwrap();
        ledger.apply_locally_mined_block(block).unwrap();
        ledger.submit_blinded_reveal(blinded.reveal).unwrap();
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let wallet = wallets
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap();
        let burn = ledger.build_burn(wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let block = ledger.mine_next_block(wallet, 2).unwrap();
        ledger.apply_locally_mined_block(block).unwrap();

        store.save_with_metrics(&ledger.snapshot(), true).unwrap();
        let metrics = store.load_metrics().unwrap();
        let last = metrics.last().unwrap();
        let supply_from_balances = ledger.status().balances.values().copied().sum::<u64>();

        assert_eq!(last.burn_count, 2);
        assert_eq!(last.burned_amount, 4);
        assert_eq!(last.fees_amount, 7);
        assert_eq!(last.circulating_supply, supply_from_balances);
    }

    #[test]
    fn sqlite_chain_store_roundtrips_vdf_round_metrics_above_legacy_u32_limit() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let vdf_rounds = u64::from(u32::MAX) + 42;

        store
            .with_connection_mut(|connection| {
                let transaction = connection.transaction().unwrap();
                replace_metrics(
                    &transaction,
                    &[BlockMetricRow {
                        height: 1,
                        block_hash: "hash".to_string(),
                        timestamp_ms: 1_000,
                        block_time_ms: Some(415_000),
                        mine_difficulty_bits: 12,
                        circulating_supply: 100,
                        transaction_count: 0,
                        transfer_count: 0,
                        burn_count: 0,
                        mine_count: 0,
                        burned_amount: 0,
                        total_burned_amount: 0,
                        fees_amount: 0,
                        reward_amount: 0,
                        vdf_rounds,
                        finalizer_rank: 0,
                    }],
                )?;
                transaction.commit().unwrap();
                Ok(())
            })
            .unwrap();

        let metrics = store.load_metrics().unwrap();
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].vdf_rounds, vdf_rounds);
    }

    #[test]
    fn sqlite_chain_store_metrics_include_genesis_reward_when_burn_consumes_allocation() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let wallet = Wallet::from_seed("metrics-genesis-reward");
        let mut genesis = BTreeMap::new();
        genesis.insert(wallet.address().to_string(), 1);
        let ledger =
            Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
                .unwrap();

        store.save_with_metrics(&ledger.snapshot(), true).unwrap();
        let metrics = store.load_metrics().unwrap();

        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].height, 0);
        assert_eq!(metrics[0].burned_amount, 1);
        assert_eq!(metrics[0].reward_amount, BLOCK_REWARD);
        assert_eq!(metrics[0].circulating_supply, BLOCK_REWARD);
    }

    #[test]
    fn sqlite_chain_store_disabled_metrics_save_deletes_old_metrics() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let wallet = Wallet::from_seed("metrics-cleanup");
        let mut genesis = BTreeMap::new();
        genesis.insert(wallet.address().to_string(), 10);
        let ledger =
            Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
                .unwrap();

        store.save_with_metrics(&ledger.snapshot(), true).unwrap();
        assert!(!store.load_metrics().unwrap().is_empty());

        store.save_with_metrics(&ledger.snapshot(), false).unwrap();
        assert!(store.load_metrics().unwrap().is_empty());
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
