use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value};

use serde::Serialize;

use crate::{
    adapters::ui_index::{UiChainIndex, build_ui_chain_index},
    domain::{
        Amount, Block, BurnLeaderRank, ChainSnapshot, Ledger, MINE_RETARGET_WINDOW_BLOCKS,
        MINE_REWARD, OutPoint, Transaction, TxInput, TxOutput, retarget_mine_difficulty_bits,
    },
};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS block_metrics (
    height INTEGER PRIMARY KEY,
    block_hash TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    block_time_ms INTEGER,
    mine_difficulty_bits INTEGER NOT NULL,
    circulating_supply INTEGER NOT NULL,
    known_wallet_addresses INTEGER NOT NULL DEFAULT 0,
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

CREATE TABLE IF NOT EXISTS metrics_cache_meta (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    schema_version INTEGER NOT NULL,
    height INTEGER NOT NULL,
    tip_hash TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS metric_known_addresses (
    address TEXT PRIMARY KEY,
    first_seen_height INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS ui_leaderboards (
    kind TEXT NOT NULL,
    address TEXT NOT NULL,
    amount INTEGER NOT NULL,
    count INTEGER NOT NULL,
    PRIMARY KEY (kind, address)
);

CREATE INDEX IF NOT EXISTS idx_ui_leaderboards_rank
ON ui_leaderboards(kind, amount DESC, count DESC, address ASC);

CREATE TABLE IF NOT EXISTS ui_cache_meta (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    schema_version INTEGER NOT NULL,
    tip_hash TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS ui_output_index (
    txid TEXT NOT NULL,
    output_index INTEGER NOT NULL,
    address TEXT NOT NULL,
    amount INTEGER NOT NULL,
    PRIMARY KEY (txid, output_index)
);

CREATE TABLE IF NOT EXISTS ui_utxos (
    txid TEXT NOT NULL,
    output_index INTEGER NOT NULL,
    address TEXT NOT NULL,
    amount INTEGER NOT NULL,
    PRIMARY KEY (txid, output_index)
);

CREATE INDEX IF NOT EXISTS idx_ui_utxos_address
ON ui_utxos(address);

CREATE TABLE IF NOT EXISTS ui_wallet_transactions (
    address TEXT NOT NULL,
    sort_key INTEGER NOT NULL,
    kind TEXT NOT NULL,
    signature TEXT NOT NULL,
    block_height INTEGER NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    block_finalizer TEXT NOT NULL,
    transaction_json BLOB NOT NULL,
    PRIMARY KEY (address, signature)
);

CREATE INDEX IF NOT EXISTS idx_ui_wallet_transactions_address_kind_sort
ON ui_wallet_transactions(address, kind, sort_key DESC);

CREATE INDEX IF NOT EXISTS idx_ui_wallet_transactions_address_sort
ON ui_wallet_transactions(address, sort_key DESC);

CREATE TABLE IF NOT EXISTS ui_burn_leader_ranks (
    block_hash TEXT NOT NULL,
    rank INTEGER NOT NULL,
    ticket_id TEXT NOT NULL,
    owner TEXT NOT NULL,
    amount INTEGER NOT NULL,
    eligible_from_height INTEGER NOT NULL,
    eligible_until_height INTEGER NOT NULL,
    PRIMARY KEY (block_hash, rank)
);

CREATE TABLE IF NOT EXISTS ui_burn_leader_rank_blocks (
    block_hash TEXT PRIMARY KEY
);
"#;

const RESET_SCHEMA: &str = r#"
DROP TABLE IF EXISTS block_metrics;
DROP TABLE IF EXISTS metrics_cache_meta;
DROP TABLE IF EXISTS metric_known_addresses;
DROP TABLE IF EXISTS ui_leaderboards;
DROP TABLE IF EXISTS ui_cache_meta;
DROP TABLE IF EXISTS ui_output_index;
DROP TABLE IF EXISTS ui_utxos;
DROP TABLE IF EXISTS ui_wallet_transactions;
DROP TABLE IF EXISTS ui_revealed_transactions;
DROP TABLE IF EXISTS ui_burn_leader_ranks;
DROP TABLE IF EXISTS ui_burn_leader_rank_blocks;
"#;

const UI_DATA_SCHEMA_VERSION: u32 = 1;
const UI_CACHE_SCHEMA_VERSION: u32 = 3;
const METRICS_CACHE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockMetricRow {
    pub height: u64,
    pub block_hash: String,
    pub timestamp_ms: u64,
    pub block_time_ms: Option<u64>,
    pub mine_difficulty_bits: u32,
    pub circulating_supply: Amount,
    pub known_wallet_addresses: u64,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiLeaderboardEntry {
    pub address: String,
    pub amount: Amount,
    pub count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WalletTransactionProjection {
    pub sort_key: u64,
    pub kind: String,
    pub block_height: u64,
    pub timestamp_ms: u64,
    pub block_finalizer: String,
    pub transaction: Transaction,
}

#[derive(Clone, Debug)]
pub struct SqliteUiDataStore {
    path: PathBuf,
}

impl SqliteUiDataStore {
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
        store.with_connection_mut(|connection| {
            initialize_ui_data_schema(connection, store.path())
        })?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn load_ui_chain_index(&self, tip_hash: &str) -> Result<Option<UiChainIndex>> {
        self.with_connection(|connection| {
            let meta = connection
                .query_row(
                    "SELECT schema_version, tip_hash FROM ui_cache_meta WHERE id = 1",
                    [],
                    |row| Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .context("failed to load UI chain index metadata")?;
            let Some((schema_version, stored_tip_hash)) = meta else {
                return Ok(None);
            };
            if schema_version != UI_CACHE_SCHEMA_VERSION || stored_tip_hash != tip_hash {
                return Ok(None);
            }

            Ok(Some(UiChainIndex {
                tip_hash: Some(stored_tip_hash),
                outputs: load_ui_output_index(connection)?,
                burn_leader_ranks_by_hash: load_ui_burn_leader_ranks(connection)?,
            }))
        })
    }

    pub fn is_projected_to(&self, tip_hash: &str) -> Result<bool> {
        self.with_connection(|connection| {
            let projected = connection
                .query_row(
                    "SELECT schema_version, tip_hash FROM ui_cache_meta WHERE id = 1",
                    [],
                    |row| Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .context("failed to load UI data projection metadata")?
                .is_some_and(|(schema_version, stored_tip_hash)| {
                    schema_version == UI_CACHE_SCHEMA_VERSION && stored_tip_hash == tip_hash
                });
            Ok(projected)
        })
    }

    pub fn metrics_are_projected_to(&self, tip_hash: &str) -> Result<bool> {
        self.with_connection(|connection| {
            let projected = connection
                .query_row(
                    "SELECT schema_version, tip_hash FROM metrics_cache_meta WHERE id = 1",
                    [],
                    |row| Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .context("failed to inspect metrics projection")?
                .is_some_and(|(schema_version, stored_tip_hash)| {
                    schema_version == METRICS_CACHE_SCHEMA_VERSION && stored_tip_hash == tip_hash
                });
            Ok(projected)
        })
    }

    pub fn project_snapshot(&self, snapshot: &ChainSnapshot, keep_metrics: bool) -> Result<()> {
        let updated_at_ms = unix_ms();
        validate_projection_snapshot_structure(snapshot)?;
        let ledger = Ledger::from_preverified_snapshot(snapshot.clone())
            .context("failed to rebuild ledger for UI UTXO projection")?;

        if keep_metrics {
            self.project_metrics_snapshot(snapshot, updated_at_ms)?;
        } else {
            self.clear_metrics()?;
        }

        let ui_index = build_ui_chain_index(snapshot);
        let utxos = ledger.all_utxos();
        let wallet_transactions = wallet_transactions_from_snapshot(snapshot);
        let leaderboards = build_ui_leaderboards(&utxos, &wallet_transactions)?;

        self.with_connection_mut(|connection| {
            let transaction = connection
                .transaction()
                .context("failed to start UI data projection transaction")?;
            replace_ui_chain_index(&transaction, &ui_index, updated_at_ms)?;
            replace_ui_utxos(&transaction, &utxos)?;
            replace_ui_wallet_transactions(&transaction, &wallet_transactions)?;
            replace_ui_leaderboards(&transaction, &leaderboards)?;
            transaction
                .commit()
                .context("failed to commit UI data projection transaction")?;
            Ok(())
        })
    }

    pub fn replace_metrics_for_snapshot(&self, snapshot: &ChainSnapshot) -> Result<()> {
        validate_projection_snapshot_structure(snapshot)?;
        self.project_metrics_snapshot(snapshot, unix_ms())
    }

    pub fn clear_metrics(&self) -> Result<()> {
        self.with_connection_mut(clear_metrics_state)
    }

    pub fn clear_all(&self) -> Result<()> {
        self.with_connection_mut(|connection| {
            let transaction = connection
                .transaction()
                .context("failed to start UI data reset transaction")?;
            clear_metrics_in_transaction(&transaction)?;
            clear_ui_chain_index_in_transaction(&transaction)?;
            transaction
                .commit()
                .context("failed to commit UI data reset transaction")?;
            Ok(())
        })
    }

    fn project_metrics_snapshot(&self, snapshot: &ChainSnapshot, updated_at_ms: u64) -> Result<()> {
        self.with_connection_mut(|connection| {
            let transaction = connection
                .transaction()
                .context("failed to start incremental metrics transaction")?;
            project_metrics_in_transaction(&transaction, snapshot, updated_at_ms)?;
            transaction
                .commit()
                .context("failed to commit incremental metrics transaction")?;
            Ok(())
        })
    }

    pub fn load_metrics(&self) -> Result<Vec<BlockMetricRow>> {
        self.with_connection(|connection| {
            let mut statement = connection
                .prepare(
                    r#"
SELECT height, block_hash, timestamp_ms, block_time_ms, mine_difficulty_bits,
       circulating_supply, known_wallet_addresses, transaction_count, transfer_count, burn_count,
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
                        known_wallet_addresses: row.get(6)?,
                        transaction_count: row.get(7)?,
                        transfer_count: row.get(8)?,
                        burn_count: row.get(9)?,
                        mine_count: row.get(10)?,
                        burned_amount: row.get(11)?,
                        total_burned_amount: row.get(12)?,
                        fees_amount: row.get(13)?,
                        reward_amount: row.get(14)?,
                        vdf_rounds: row.get(15)?,
                        finalizer_rank: row.get(16)?,
                    })
                })
                .context("failed to load block metrics")?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .context("failed to read block metrics rows")
        })
    }

    pub fn load_recent_metrics(&self, limit: usize) -> Result<Vec<BlockMetricRow>> {
        self.with_connection(|connection| {
            let mut statement = connection
                .prepare(
                    r#"
SELECT height, block_hash, timestamp_ms, block_time_ms, mine_difficulty_bits,
       circulating_supply, known_wallet_addresses, transaction_count, transfer_count, burn_count,
       mine_count, burned_amount, total_burned_amount, fees_amount, reward_amount,
       vdf_rounds, finalizer_rank
FROM block_metrics
ORDER BY height DESC
LIMIT ?1
"#,
                )
                .context("failed to prepare recent block metrics query")?;
            let rows = statement
                .query_map([limit as u64], |row| {
                    Ok(BlockMetricRow {
                        height: row.get(0)?,
                        block_hash: row.get(1)?,
                        timestamp_ms: row.get(2)?,
                        block_time_ms: row.get(3)?,
                        mine_difficulty_bits: row.get(4)?,
                        circulating_supply: row.get(5)?,
                        known_wallet_addresses: row.get(6)?,
                        transaction_count: row.get(7)?,
                        transfer_count: row.get(8)?,
                        burn_count: row.get(9)?,
                        mine_count: row.get(10)?,
                        burned_amount: row.get(11)?,
                        total_burned_amount: row.get(12)?,
                        fees_amount: row.get(13)?,
                        reward_amount: row.get(14)?,
                        vdf_rounds: row.get(15)?,
                        finalizer_rank: row.get(16)?,
                    })
                })
                .context("failed to load recent block metrics")?;
            let mut rows = rows
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("failed to read recent block metrics rows")?;
            rows.reverse();
            Ok(rows)
        })
    }

    pub fn load_leaderboards(&self, limit: usize) -> Result<UiLeaderboards> {
        self.with_connection(|connection| {
            Ok(UiLeaderboards {
                balances: load_balance_leaderboard(connection, limit)?,
                miners: load_transaction_leaderboard(connection, "mine", limit)?,
                burners: load_transaction_leaderboard(connection, "burn", limit)?,
            })
        })
    }

    pub fn load_wallet_utxos(&self, address: &str) -> Result<Vec<(OutPoint, TxOutput)>> {
        self.with_connection(|connection| load_wallet_utxos(connection, address))
    }

    pub fn load_outputs(
        &self,
        outpoints: &BTreeSet<OutPoint>,
    ) -> Result<BTreeMap<OutPoint, TxOutput>> {
        self.with_connection(|connection| load_outputs(connection, outpoints))
    }

    pub fn load_wallet_transactions(
        &self,
        address: &str,
        kinds: &[&str],
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<WalletTransactionProjection>, usize)> {
        self.with_connection(|connection| {
            load_wallet_transactions(connection, address, kinds, offset, limit)
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
            .context("failed to configure UI data database connection")?;
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
            .context("failed to configure UI data database connection")?;
        work(&mut connection)
    }

    fn open_connection(&self) -> Result<Connection> {
        Connection::open(&self.path)
            .with_context(|| format!("failed to open UI data database {}", self.path.display()))
    }
}

fn initialize_ui_data_schema(connection: &mut Connection, path: &Path) -> Result<()> {
    let stored_version = connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
        .context("failed to inspect UI data database schema version")?;
    let has_tables = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .context("failed to inspect UI data database tables")?;

    if has_tables && stored_version != UI_DATA_SCHEMA_VERSION {
        println!(
            "rebuilding incompatible UI cache schema at {} (version {stored_version}, expected {UI_DATA_SCHEMA_VERSION})",
            path.display()
        );
        let transaction = connection
            .transaction()
            .context("failed to start UI data schema rebuild transaction")?;
        transaction
            .execute_batch(RESET_SCHEMA)
            .context("failed to clear incompatible UI data database schema")?;
        transaction
            .execute_batch(SCHEMA)
            .context("failed to recreate UI data database schema")?;
        transaction
            .pragma_update(None, "user_version", UI_DATA_SCHEMA_VERSION)
            .context("failed to record UI data database schema version")?;
        transaction
            .commit()
            .context("failed to commit UI data database schema rebuild")?;
        return Ok(());
    }

    connection
        .execute_batch(SCHEMA)
        .context("failed to initialize UI data database schema")?;
    ensure_block_metrics_column(
        connection,
        "known_wallet_addresses",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    connection
        .pragma_update(None, "user_version", UI_DATA_SCHEMA_VERSION)
        .context("failed to record UI data database schema version")?;
    Ok(())
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UiLeaderboards {
    pub balances: Vec<UiLeaderboardEntry>,
    pub miners: Vec<UiLeaderboardEntry>,
    pub burners: Vec<UiLeaderboardEntry>,
}

fn load_balance_leaderboard(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<UiLeaderboardEntry>> {
    load_materialized_leaderboard(connection, "balances", limit, false)
}

fn load_transaction_leaderboard(
    connection: &Connection,
    kind: &str,
    limit: usize,
) -> Result<Vec<UiLeaderboardEntry>> {
    load_materialized_leaderboard(connection, kind, limit, true)
}

fn load_materialized_leaderboard(
    connection: &Connection,
    kind: &str,
    limit: usize,
    rank_by_count: bool,
) -> Result<Vec<UiLeaderboardEntry>> {
    let order = if rank_by_count {
        "amount DESC, count DESC, address ASC"
    } else {
        "amount DESC, address ASC"
    };
    let mut statement = connection
        .prepare(&format!(
            r#"
SELECT address, amount, count
FROM ui_leaderboards
WHERE kind = ?1
ORDER BY {order}
LIMIT ?2
"#
        ))
        .with_context(|| format!("failed to prepare {kind} leaderboard query"))?;
    let rows = statement
        .query_map(params![kind, limit as u64], |row| {
            Ok(UiLeaderboardEntry {
                address: row.get(0)?,
                amount: row.get(1)?,
                count: row.get(2)?,
            })
        })
        .with_context(|| format!("failed to load {kind} leaderboard"))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read {kind} leaderboard rows"))
}

fn project_metrics_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    snapshot: &ChainSnapshot,
    updated_at_ms: u64,
) -> Result<()> {
    let tip = snapshot
        .blocks
        .last()
        .context("cannot project metrics for empty chain snapshot")?;
    let mut common_height = metrics_common_height(transaction, snapshot)?;
    let mut previous = common_height
        .map(|height| load_metric_at_height(transaction, height))
        .transpose()?
        .flatten();
    if common_height.is_some() && previous.is_none() {
        common_height = None;
    }

    match common_height {
        Some(height) => {
            transaction
                .execute("DELETE FROM block_metrics WHERE height > ?1", [height])
                .context("failed to truncate reorged block metrics")?;
            transaction
                .execute(
                    "DELETE FROM metric_known_addresses WHERE first_seen_height > ?1",
                    [height],
                )
                .context("failed to truncate reorged metric addresses")?;
        }
        None => clear_metrics_in_transaction(transaction)?,
    }

    let mut known_wallet_addresses = previous
        .as_ref()
        .map(|metric| metric.known_wallet_addresses)
        .unwrap_or_default();
    if previous.is_none() {
        for address in snapshot.genesis_allocations.keys() {
            known_wallet_addresses = known_wallet_addresses
                .checked_add(insert_metric_address(transaction, address, 0)?)
                .context("known metric address count overflows")?;
        }
    }

    let start_height = common_height.map_or(0, |height| height.saturating_add(1));
    for block in snapshot
        .blocks
        .iter()
        .filter(|block| block.height >= start_height)
    {
        known_wallet_addresses = known_wallet_addresses
            .checked_add(index_metric_addresses(transaction, block)?)
            .context("known metric address count overflows")?;
        let mut metric = incremental_metric_for_block(snapshot, block, previous.as_ref())?;
        metric.known_wallet_addresses = known_wallet_addresses;
        insert_metric(transaction, &metric)?;
        previous = Some(metric);
    }

    transaction
        .execute(
            r#"
INSERT INTO metrics_cache_meta (id, schema_version, height, tip_hash, updated_at_ms)
VALUES (1, ?1, ?2, ?3, ?4)
ON CONFLICT(id) DO UPDATE SET
    schema_version = excluded.schema_version,
    height = excluded.height,
    tip_hash = excluded.tip_hash,
    updated_at_ms = excluded.updated_at_ms
"#,
            params![
                METRICS_CACHE_SCHEMA_VERSION,
                tip.height,
                tip.hash,
                updated_at_ms
            ],
        )
        .context("failed to update metrics cache metadata")?;
    Ok(())
}

fn metrics_common_height(
    transaction: &rusqlite::Transaction<'_>,
    snapshot: &ChainSnapshot,
) -> Result<Option<u64>> {
    let meta = transaction
        .query_row(
            "SELECT schema_version, height, tip_hash FROM metrics_cache_meta WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .context("failed to load metrics cache metadata")?;
    let Some((schema_version, stored_height, stored_hash)) = meta else {
        return Ok(None);
    };
    if schema_version != METRICS_CACHE_SCHEMA_VERSION {
        return Ok(None);
    }

    let candidate_height = stored_height.min(snapshot.blocks.len().saturating_sub(1) as u64);
    if candidate_height == stored_height
        && snapshot.blocks[candidate_height as usize].hash == stored_hash
    {
        return Ok(Some(candidate_height));
    }

    let mut statement = transaction
        .prepare(
            "SELECT height, block_hash FROM block_metrics WHERE height <= ?1 ORDER BY height DESC",
        )
        .context("failed to prepare metrics common-ancestor query")?;
    let rows = statement
        .query_map([candidate_height], |row| {
            Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?))
        })
        .context("failed to query metrics common ancestor")?;
    for row in rows {
        let (height, hash) = row.context("failed to read metrics common ancestor")?;
        if snapshot
            .blocks
            .get(height as usize)
            .is_some_and(|block| block.hash == hash)
        {
            return Ok(Some(height));
        }
    }
    Ok(None)
}

fn load_metric_at_height(
    transaction: &rusqlite::Transaction<'_>,
    height: u64,
) -> Result<Option<BlockMetricRow>> {
    transaction
        .query_row(
            r#"
SELECT height, block_hash, timestamp_ms, block_time_ms, mine_difficulty_bits,
       circulating_supply, known_wallet_addresses, transaction_count, transfer_count, burn_count,
       mine_count, burned_amount, total_burned_amount, fees_amount, reward_amount,
       vdf_rounds, finalizer_rank
FROM block_metrics
WHERE height = ?1
"#,
            [height],
            |row| {
                Ok(BlockMetricRow {
                    height: row.get(0)?,
                    block_hash: row.get(1)?,
                    timestamp_ms: row.get(2)?,
                    block_time_ms: row.get(3)?,
                    mine_difficulty_bits: row.get(4)?,
                    circulating_supply: row.get(5)?,
                    known_wallet_addresses: row.get(6)?,
                    transaction_count: row.get(7)?,
                    transfer_count: row.get(8)?,
                    burn_count: row.get(9)?,
                    mine_count: row.get(10)?,
                    burned_amount: row.get(11)?,
                    total_burned_amount: row.get(12)?,
                    fees_amount: row.get(13)?,
                    reward_amount: row.get(14)?,
                    vdf_rounds: row.get(15)?,
                    finalizer_rank: row.get(16)?,
                })
            },
        )
        .optional()
        .context("failed to load previous block metric")
}

fn insert_metric(transaction: &rusqlite::Transaction<'_>, metric: &BlockMetricRow) -> Result<()> {
    transaction
        .execute(
            r#"
INSERT INTO block_metrics (
    height, block_hash, timestamp_ms, block_time_ms, mine_difficulty_bits,
    circulating_supply, known_wallet_addresses, transaction_count, transfer_count, burn_count,
    mine_count, burned_amount, total_burned_amount, fees_amount, reward_amount, vdf_rounds,
    finalizer_rank
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
"#,
            params![
                metric.height,
                metric.block_hash,
                metric.timestamp_ms,
                metric.block_time_ms,
                metric.mine_difficulty_bits,
                metric.circulating_supply,
                metric.known_wallet_addresses,
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
    Ok(())
}

fn ensure_block_metrics_column(
    connection: &Connection,
    name: &str,
    definition: &str,
) -> Result<()> {
    let mut statement = connection
        .prepare("PRAGMA table_info(block_metrics)")
        .context("failed to inspect block_metrics schema")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .context("failed to query block_metrics columns")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read block_metrics columns")?;
    if columns.iter().any(|column| column == name) {
        return Ok(());
    }
    connection
        .execute(
            &format!("ALTER TABLE block_metrics ADD COLUMN {name} {definition}"),
            [],
        )
        .with_context(|| format!("failed to add block_metrics.{name} column"))?;
    Ok(())
}

fn clear_metrics_in_transaction(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    transaction
        .execute("DELETE FROM block_metrics", [])
        .context("failed to clear old block metrics")?;
    transaction
        .execute("DELETE FROM metrics_cache_meta", [])
        .context("failed to clear old metrics cache metadata")?;
    transaction
        .execute("DELETE FROM metric_known_addresses", [])
        .context("failed to clear old metric addresses")?;
    Ok(())
}

fn clear_metrics_state(connection: &mut Connection) -> Result<()> {
    let transaction = connection
        .transaction()
        .context("failed to start metrics cleanup transaction")?;
    clear_metrics_in_transaction(&transaction)?;
    transaction
        .commit()
        .context("failed to commit metrics cleanup transaction")?;
    Ok(())
}

fn replace_ui_chain_index(
    transaction: &rusqlite::Transaction<'_>,
    index: &UiChainIndex,
    updated_at_ms: u64,
) -> Result<()> {
    clear_ui_chain_index_in_transaction(transaction)?;
    let Some(tip_hash) = &index.tip_hash else {
        return Ok(());
    };
    transaction
        .execute(
            r#"
INSERT INTO ui_cache_meta (id, schema_version, tip_hash, updated_at_ms)
VALUES (1, ?1, ?2, ?3)
"#,
            params![UI_CACHE_SCHEMA_VERSION, tip_hash, updated_at_ms],
        )
        .context("failed to persist UI chain index metadata")?;
    for (outpoint, output) in &index.outputs {
        transaction
            .execute(
                r#"
INSERT INTO ui_output_index (txid, output_index, address, amount)
VALUES (?1, ?2, ?3, ?4)
"#,
                params![outpoint.txid, outpoint.index, output.address, output.amount],
            )
            .with_context(|| {
                format!(
                    "failed to persist UI output index row {}:{}",
                    outpoint.txid, outpoint.index
                )
            })?;
    }
    for (block_hash, ranks) in &index.burn_leader_ranks_by_hash {
        transaction
            .execute(
                "INSERT INTO ui_burn_leader_rank_blocks (block_hash) VALUES (?1)",
                params![block_hash],
            )
            .with_context(|| format!("failed to persist UI burn leader rank block {block_hash}"))?;
        for rank in ranks {
            transaction
                .execute(
                    r#"
INSERT INTO ui_burn_leader_ranks (
    block_hash, rank, ticket_id, owner, amount, eligible_from_height, eligible_until_height
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
"#,
                    params![
                        block_hash,
                        rank.rank,
                        rank.ticket_id,
                        rank.owner,
                        rank.amount,
                        rank.eligible_from_height,
                        rank.eligible_until_height,
                    ],
                )
                .with_context(|| {
                    format!(
                        "failed to persist UI burn leader rank {} for block {}",
                        rank.rank, block_hash
                    )
                })?;
        }
    }
    Ok(())
}

fn replace_ui_utxos(
    transaction: &rusqlite::Transaction<'_>,
    utxos: &[(OutPoint, TxOutput)],
) -> Result<()> {
    transaction
        .execute("DELETE FROM ui_utxos", [])
        .context("failed to clear old UI UTXO index")?;
    for (outpoint, output) in utxos {
        transaction
            .execute(
                r#"
INSERT INTO ui_utxos (txid, output_index, address, amount)
VALUES (?1, ?2, ?3, ?4)
"#,
                params![outpoint.txid, outpoint.index, output.address, output.amount],
            )
            .with_context(|| {
                format!(
                    "failed to persist UI UTXO row {}:{}",
                    outpoint.txid, outpoint.index
                )
            })?;
    }
    Ok(())
}

fn replace_ui_wallet_transactions(
    transaction: &rusqlite::Transaction<'_>,
    rows: &[(String, WalletTransactionProjection)],
) -> Result<()> {
    transaction
        .execute("DELETE FROM ui_wallet_transactions", [])
        .context("failed to clear old UI wallet transaction index")?;
    for (address, row) in rows {
        let transaction_json = serde_json::to_vec(&row.transaction)
            .context("failed to serialize UI wallet transaction")?;
        transaction
            .execute(
                r#"
INSERT INTO ui_wallet_transactions (
    address, sort_key, kind, signature, block_height, timestamp_ms, block_finalizer,
    transaction_json
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
"#,
                params![
                    address,
                    row.sort_key,
                    row.kind,
                    row.transaction.signature(),
                    row.block_height,
                    row.timestamp_ms,
                    row.block_finalizer,
                    transaction_json,
                ],
            )
            .with_context(|| {
                format!(
                    "failed to persist UI wallet transaction {} for {}",
                    row.transaction.signature(),
                    address
                )
            })?;
    }
    Ok(())
}

fn replace_ui_leaderboards(
    transaction: &rusqlite::Transaction<'_>,
    rows: &[(String, UiLeaderboardEntry)],
) -> Result<()> {
    transaction
        .execute("DELETE FROM ui_leaderboards", [])
        .context("failed to clear old UI leaderboards")?;
    for (kind, row) in rows {
        transaction
            .execute(
                r#"
INSERT INTO ui_leaderboards (kind, address, amount, count)
VALUES (?1, ?2, ?3, ?4)
"#,
                params![kind, row.address, row.amount, row.count],
            )
            .with_context(|| {
                format!(
                    "failed to persist {kind} leaderboard row for {}",
                    row.address
                )
            })?;
    }
    Ok(())
}

fn clear_ui_chain_index_in_transaction(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    transaction
        .execute("DELETE FROM ui_cache_meta", [])
        .context("failed to clear old UI cache metadata")?;
    transaction
        .execute("DELETE FROM ui_output_index", [])
        .context("failed to clear old UI output index")?;
    transaction
        .execute("DELETE FROM ui_utxos", [])
        .context("failed to clear old UI UTXO index")?;
    transaction
        .execute("DELETE FROM ui_wallet_transactions", [])
        .context("failed to clear old UI wallet transaction index")?;
    transaction
        .execute("DELETE FROM ui_leaderboards", [])
        .context("failed to clear old UI leaderboards")?;
    transaction
        .execute("DELETE FROM ui_burn_leader_ranks", [])
        .context("failed to clear old UI burn leader rank index")?;
    transaction
        .execute("DELETE FROM ui_burn_leader_rank_blocks", [])
        .context("failed to clear old UI burn leader rank block index")?;
    Ok(())
}

fn load_ui_output_index(connection: &Connection) -> Result<BTreeMap<OutPoint, TxOutput>> {
    let mut statement = connection
        .prepare(
            r#"
SELECT txid, output_index, address, amount
FROM ui_output_index
ORDER BY txid, output_index
"#,
        )
        .context("failed to prepare UI output index query")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                OutPoint {
                    txid: row.get(0)?,
                    index: row.get(1)?,
                },
                TxOutput {
                    address: row.get(2)?,
                    amount: row.get(3)?,
                },
            ))
        })
        .context("failed to load UI output index")?;
    rows.collect::<std::result::Result<BTreeMap<_, _>, _>>()
        .context("failed to read UI output index rows")
}

fn load_outputs(
    connection: &Connection,
    outpoints: &BTreeSet<OutPoint>,
) -> Result<BTreeMap<OutPoint, TxOutput>> {
    if outpoints.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut outputs = BTreeMap::new();
    let mut statement = connection
        .prepare(
            r#"
SELECT address, amount
FROM ui_output_index
WHERE txid = ?1 AND output_index = ?2
"#,
        )
        .context("failed to prepare narrow UI output lookup")?;
    for outpoint in outpoints {
        let output = statement
            .query_row(params![outpoint.txid, outpoint.index], |row| {
                Ok(TxOutput {
                    address: row.get(0)?,
                    amount: row.get(1)?,
                })
            })
            .optional()
            .with_context(|| {
                format!(
                    "failed to load UI output {}:{}",
                    outpoint.txid, outpoint.index
                )
            })?;
        if let Some(output) = output {
            outputs.insert(outpoint.clone(), output);
        }
    }
    Ok(outputs)
}

fn load_wallet_utxos(connection: &Connection, address: &str) -> Result<Vec<(OutPoint, TxOutput)>> {
    let mut statement = connection
        .prepare(
            r#"
SELECT txid, output_index, address, amount
FROM ui_utxos
WHERE address = ?1
ORDER BY amount DESC, txid ASC, output_index ASC
"#,
        )
        .context("failed to prepare UI wallet UTXO query")?;
    let rows = statement
        .query_map([address], |row| {
            Ok((
                OutPoint {
                    txid: row.get(0)?,
                    index: row.get(1)?,
                },
                TxOutput {
                    address: row.get(2)?,
                    amount: row.get(3)?,
                },
            ))
        })
        .context("failed to load UI wallet UTXOs")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read UI wallet UTXO rows")
}

fn load_wallet_transactions(
    connection: &Connection,
    address: &str,
    kinds: &[&str],
    offset: usize,
    limit: usize,
) -> Result<(Vec<WalletTransactionProjection>, usize)> {
    if kinds.is_empty() {
        return Ok((Vec::new(), 0));
    }
    if wallet_transaction_kinds_cover_all(kinds) {
        let total = connection
            .query_row(
                "SELECT COUNT(*) FROM ui_wallet_transactions WHERE address = ?1",
                params![address],
                |row| row.get::<_, u64>(0),
            )
            .context("failed to count UI wallet transactions")? as usize;
        let mut statement = connection
            .prepare(
                r#"
SELECT sort_key, kind, block_height, timestamp_ms, block_finalizer, transaction_json
FROM ui_wallet_transactions
WHERE address = ?1
ORDER BY sort_key DESC
LIMIT ?2 OFFSET ?3
"#,
            )
            .context("failed to prepare UI wallet transactions query")?;
        let rows = read_wallet_transaction_rows(
            statement.query_map(params![address, limit as i64, offset as i64], |row| {
                wallet_transaction_projection_from_row(row)
            })?,
        )?;
        return Ok((rows, total));
    }
    let placeholders = std::iter::repeat_n("?", kinds.len())
        .collect::<Vec<_>>()
        .join(", ");
    let count_sql = format!(
        "SELECT COUNT(*) FROM ui_wallet_transactions WHERE address = ? AND kind IN ({placeholders})"
    );
    let mut count_params = Vec::<Value>::with_capacity(kinds.len() + 1);
    count_params.push(Value::Text(address.to_string()));
    for kind in kinds {
        count_params.push(Value::Text((*kind).to_string()));
    }
    let total = connection
        .query_row(&count_sql, params_from_iter(count_params.iter()), |row| {
            row.get::<_, u64>(0)
        })
        .context("failed to count UI wallet transactions")? as usize;

    let query_sql = format!(
        r#"
SELECT sort_key, kind, block_height, timestamp_ms, block_finalizer, transaction_json
FROM ui_wallet_transactions
WHERE address = ? AND kind IN ({placeholders})
ORDER BY sort_key DESC
LIMIT ? OFFSET ?
"#
    );
    let mut query_params = Vec::<Value>::with_capacity(kinds.len() + 3);
    query_params.push(Value::Text(address.to_string()));
    for kind in kinds {
        query_params.push(Value::Text((*kind).to_string()));
    }
    query_params.push(Value::Integer(limit as i64));
    query_params.push(Value::Integer(offset as i64));
    let mut statement = connection
        .prepare(&query_sql)
        .context("failed to prepare UI wallet transactions query")?;
    let rows = read_wallet_transaction_rows(
        statement
            .query_map(params_from_iter(query_params.iter()), |row| {
                wallet_transaction_projection_from_row(row)
            })
            .context("failed to load UI wallet transactions")?,
    )?;
    Ok((rows, total))
}

fn wallet_transaction_kinds_cover_all(kinds: &[&str]) -> bool {
    ["transfer", "mine", "burn"]
        .into_iter()
        .all(|kind| kinds.contains(&kind))
}

fn wallet_transaction_projection_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<WalletTransactionProjection> {
    let transaction_json = row.get::<_, Vec<u8>>(5)?;
    let transaction =
        serde_json::from_slice::<Transaction>(&transaction_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                transaction_json.len(),
                rusqlite::types::Type::Blob,
                Box::new(error),
            )
        })?;
    Ok(WalletTransactionProjection {
        sort_key: row.get(0)?,
        kind: row.get(1)?,
        block_height: row.get(2)?,
        timestamp_ms: row.get(3)?,
        block_finalizer: row.get(4)?,
        transaction,
    })
}

fn read_wallet_transaction_rows(
    rows: rusqlite::MappedRows<
        '_,
        impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<WalletTransactionProjection>,
    >,
) -> Result<Vec<WalletTransactionProjection>> {
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read UI wallet transaction rows")
}

fn load_ui_burn_leader_ranks(
    connection: &Connection,
) -> Result<BTreeMap<String, Vec<BurnLeaderRank>>> {
    let mut blocks_statement = connection
        .prepare("SELECT block_hash FROM ui_burn_leader_rank_blocks ORDER BY block_hash")
        .context("failed to prepare UI burn leader rank block query")?;
    let blocks = blocks_statement
        .query_map([], |row| row.get::<_, String>(0))
        .context("failed to load UI burn leader rank blocks")?;
    let mut by_block_hash = BTreeMap::<String, Vec<BurnLeaderRank>>::new();
    for block_hash in blocks {
        by_block_hash.insert(
            block_hash.context("failed to read UI burn leader rank block row")?,
            Vec::new(),
        );
    }

    let mut statement = connection
        .prepare(
            r#"
SELECT block_hash, rank, ticket_id, owner, amount, eligible_from_height, eligible_until_height
FROM ui_burn_leader_ranks
ORDER BY block_hash, rank
"#,
        )
        .context("failed to prepare UI burn leader rank query")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                BurnLeaderRank {
                    rank: row.get(1)?,
                    ticket_id: row.get(2)?,
                    owner: row.get(3)?,
                    amount: row.get(4)?,
                    eligible_from_height: row.get(5)?,
                    eligible_until_height: row.get(6)?,
                },
            ))
        })
        .context("failed to load UI burn leader ranks")?;
    for row in rows {
        let (block_hash, rank) = row.context("failed to read UI burn leader rank row")?;
        by_block_hash.entry(block_hash).or_default().push(rank);
    }
    Ok(by_block_hash)
}

fn build_ui_leaderboards(
    utxos: &[(OutPoint, TxOutput)],
    wallet_transactions: &[(String, WalletTransactionProjection)],
) -> Result<Vec<(String, UiLeaderboardEntry)>> {
    let mut by_kind = BTreeMap::<(String, String), UiLeaderboardEntry>::new();
    for (_, output) in utxos {
        if output.amount == 0 {
            continue;
        }
        let key = ("balances".to_string(), output.address.clone());
        let entry = by_kind.entry(key).or_insert_with(|| UiLeaderboardEntry {
            address: output.address.clone(),
            amount: 0,
            count: 0,
        });
        entry.amount = entry
            .amount
            .checked_add(output.amount)
            .context("balance leaderboard amount overflows")?;
        entry.count = entry
            .count
            .checked_add(1)
            .context("balance leaderboard count overflows")?;
    }
    for (address, projection) in wallet_transactions {
        let (kind, amount) = match &projection.transaction {
            Transaction::Mine { .. } => ("mine", MINE_REWARD),
            Transaction::Burn { amount, .. } => ("burn", *amount),
            Transaction::Transfer { .. } => continue,
        };
        let key = (kind.to_string(), address.clone());
        let entry = by_kind.entry(key).or_insert_with(|| UiLeaderboardEntry {
            address: address.clone(),
            amount: 0,
            count: 0,
        });
        entry.amount = entry
            .amount
            .checked_add(amount)
            .with_context(|| format!("{kind} leaderboard amount overflows"))?;
        entry.count = entry
            .count
            .checked_add(1)
            .with_context(|| format!("{kind} leaderboard count overflows"))?;
    }
    Ok(by_kind
        .into_iter()
        .map(|((kind, _), entry)| (kind, entry))
        .collect())
}

fn wallet_transactions_from_snapshot(
    snapshot: &ChainSnapshot,
) -> Vec<(String, WalletTransactionProjection)> {
    let mut rows = Vec::new();
    for block in &snapshot.blocks {
        for (index, transaction) in block.transactions.iter().rev().enumerate() {
            push_wallet_transaction_projection(
                &mut rows,
                transaction,
                block,
                block.height as u128 * 10_000 + index as u128,
            );
        }
    }
    rows
}

fn push_wallet_transaction_projection(
    rows: &mut Vec<(String, WalletTransactionProjection)>,
    transaction: &Transaction,
    block: &Block,
    sort_key: u128,
) {
    let kind = transaction_kind(transaction).to_string();
    let projection = WalletTransactionProjection {
        sort_key: sort_key.min(u128::from(u64::MAX)) as u64,
        kind,
        block_height: block.height,
        timestamp_ms: block.timestamp_ms,
        block_finalizer: block.miner.clone(),
        transaction: transaction.clone(),
    };
    for address in wallet_transaction_addresses(transaction) {
        rows.push((address, projection.clone()));
    }
}

fn wallet_transaction_addresses(transaction: &Transaction) -> Vec<String> {
    match transaction {
        Transaction::Transfer { .. } => {
            let mut addresses = vec![transaction.sender().to_string()];
            if let Some(to) = transaction.to() {
                if to != transaction.sender() {
                    addresses.push(to.to_string());
                }
            }
            addresses
        }
        Transaction::Burn { .. } => vec![transaction.sender().to_string()],
        Transaction::Mine { recipient, .. } => vec![recipient.clone()],
    }
}

fn transaction_kind(transaction: &Transaction) -> &'static str {
    match transaction {
        Transaction::Transfer { .. } => "transfer",
        Transaction::Burn { .. } => "burn",
        Transaction::Mine { .. } => "mine",
    }
}

fn validate_projection_snapshot_structure(snapshot: &ChainSnapshot) -> Result<()> {
    if snapshot.blocks.is_empty() {
        anyhow::bail!("chain snapshot is empty");
    }
    for (index, block) in snapshot.blocks.iter().enumerate() {
        if block.height != index as u64 {
            anyhow::bail!("chain snapshot block heights are not contiguous");
        }
        if index > 0 && block.prev_hash != snapshot.blocks[index - 1].hash {
            anyhow::bail!("chain snapshot parent hashes are not contiguous");
        }
    }
    Ok(())
}

fn incremental_metric_for_block(
    snapshot: &ChainSnapshot,
    block: &Block,
    previous: Option<&BlockMetricRow>,
) -> Result<BlockMetricRow> {
    let mut circulating_supply = match previous {
        Some(previous) => previous.circulating_supply,
        None => snapshot
            .genesis_allocations
            .values()
            .try_fold(0_u64, |total, amount| {
                total
                    .checked_add(*amount)
                    .context("genesis circulating supply overflows")
            })?,
    };
    let mut transfer_count = 0_u64;
    let mut burn_count = 0_u64;
    let mut mine_count = 0_u64;
    let mut burned_amount = 0_u64;
    let mut fees_amount = 0_u64;

    for transaction in &block.transactions {
        fees_amount = fees_amount
            .checked_add(transaction.fee())
            .context("block metric fees overflow")?;
        match transaction {
            Transaction::Transfer { fee, .. } => {
                transfer_count += 1;
                circulating_supply = circulating_supply
                    .checked_sub(*fee)
                    .context("transfer fee exceeds circulating supply")?;
            }
            Transaction::Burn { amount, fee, .. } => {
                burn_count += 1;
                burned_amount = burned_amount
                    .checked_add(*amount)
                    .context("block metric burns overflow")?;
                circulating_supply = circulating_supply
                    .checked_sub(*amount)
                    .and_then(|supply| supply.checked_sub(*fee))
                    .context("burn exceeds circulating supply")?;
            }
            Transaction::Mine { .. } => {
                mine_count += 1;
                circulating_supply = circulating_supply
                    .checked_add(MINE_REWARD)
                    .context("mine reward circulating supply overflows")?;
            }
        }
    }
    circulating_supply = circulating_supply
        .checked_add(block.reward)
        .context("block reward circulating supply overflows")?;
    let total_burned_amount = previous
        .map(|row| row.total_burned_amount)
        .unwrap_or_default()
        .checked_add(burned_amount)
        .context("total burned metric overflows")?;

    Ok(BlockMetricRow {
        height: block.height,
        block_hash: block.hash.clone(),
        timestamp_ms: block.timestamp_ms,
        block_time_ms: previous.map(|row| block.timestamp_ms.saturating_sub(row.timestamp_ms)),
        mine_difficulty_bits: metric_difficulty_for_block(snapshot, block, previous),
        circulating_supply,
        known_wallet_addresses: 0,
        transaction_count: block.transactions.len() as u64,
        transfer_count,
        burn_count,
        mine_count,
        burned_amount,
        total_burned_amount,
        fees_amount,
        reward_amount: block.reward,
        vdf_rounds: block.vdf_rounds,
        finalizer_rank: block.finalizer_rank,
    })
}

fn metric_difficulty_for_block(
    snapshot: &ChainSnapshot,
    block: &Block,
    previous: Option<&BlockMetricRow>,
) -> u32 {
    let previous_difficulty = previous
        .map(|row| row.mine_difficulty_bits)
        .unwrap_or(snapshot.launch_profile.mine_difficulty_bits);
    if block.height == 0 || block.height % MINE_RETARGET_WINDOW_BLOCKS != 0 {
        return previous_difficulty;
    }
    let window_start = block.height + 1 - MINE_RETARGET_WINDOW_BLOCKS;
    let mine_actions = snapshot.blocks[window_start as usize..=block.height as usize]
        .iter()
        .flat_map(|candidate| candidate.transactions.iter())
        .filter(|transaction| matches!(transaction, Transaction::Mine { .. }))
        .count() as u64;
    retarget_mine_difficulty_bits(previous_difficulty, mine_actions)
}

fn index_metric_addresses(transaction: &rusqlite::Transaction<'_>, block: &Block) -> Result<u64> {
    let mut inserted = insert_metric_address(transaction, &block.miner, block.height)?;
    for signature in &block.burn_bundle_section.signatures {
        inserted = inserted
            .checked_add(insert_metric_address(
                transaction,
                &signature.member,
                block.height,
            )?)
            .context("known metric address count overflows")?;
    }
    let mut addresses = BTreeSet::new();
    for public_transaction in &block.transactions {
        collect_transaction_addresses(public_transaction, &mut addresses);
    }
    for address in addresses {
        inserted = inserted
            .checked_add(insert_metric_address(transaction, &address, block.height)?)
            .context("known metric address count overflows")?;
    }
    Ok(inserted)
}

fn insert_metric_address(
    transaction: &rusqlite::Transaction<'_>,
    address: &str,
    first_seen_height: u64,
) -> Result<u64> {
    let inserted = transaction
        .execute(
            "INSERT OR IGNORE INTO metric_known_addresses (address, first_seen_height) VALUES (?1, ?2)",
            params![address, first_seen_height],
        )
        .with_context(|| format!("failed to index metric address {address}"))?;
    Ok(inserted as u64)
}

fn collect_transaction_addresses(transaction: &Transaction, addresses: &mut BTreeSet<String>) {
    match transaction {
        Transaction::Transfer {
            inputs, outputs, ..
        } => {
            collect_input_addresses(inputs, addresses);
            collect_output_addresses(outputs, addresses);
        }
        Transaction::Burn { inputs, change, .. } => {
            collect_input_addresses(inputs, addresses);
            collect_output_addresses(change, addresses);
        }
        Transaction::Mine { recipient, .. } => {
            addresses.insert(recipient.clone());
        }
    }
}

fn collect_input_addresses(inputs: &[TxInput], addresses: &mut BTreeSet<String>) {
    for input in inputs {
        addresses.insert(input.owner.clone());
    }
}

fn collect_output_addresses(outputs: &[TxOutput], addresses: &mut BTreeSet<String>) {
    for output in outputs {
        addresses.insert(output.address.clone());
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
    use std::collections::BTreeMap;

    use rusqlite::Connection;
    use tempfile::tempdir;

    use crate::domain::{ChainSnapshot, GenesisBurn, Ledger, Wallet};

    use super::SqliteUiDataStore;

    fn test_snapshot(seed: &str) -> ChainSnapshot {
        let wallet = Wallet::from_seed(seed);
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), 1);
        Ledger::new_with_genesis_burns(allocations, vec![GenesisBurn::new(wallet.address(), 1)], 1)
            .unwrap()
            .snapshot()
    }

    fn test_ledger(seed: &str) -> (Ledger, Wallet) {
        let wallet = Wallet::from_seed(seed);
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), 1_000);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(wallet.address(), 10)],
            1,
        )
        .unwrap();
        (ledger, wallet)
    }

    fn append_test_block(ledger: &mut Ledger, wallet: &Wallet, timestamp_ms: u64) {
        let burn = ledger.build_burn(wallet, 1, 1).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let block = ledger.mine_next_block(wallet, timestamp_ms).unwrap();
        ledger.apply_locally_mined_block(block).unwrap();
    }

    #[test]
    fn opening_legacy_ui_schema_rebuilds_the_derived_cache() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ui_data.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                r#"
CREATE TABLE ui_wallet_transactions (
    address TEXT NOT NULL,
    sort_key INTEGER NOT NULL,
    kind TEXT NOT NULL,
    signature TEXT NOT NULL,
    block_height INTEGER NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    block_finalizer TEXT NOT NULL,
    blinded INTEGER NOT NULL,
    transaction_json BLOB NOT NULL,
    PRIMARY KEY (address, signature)
);
"#,
            )
            .unwrap();
        drop(connection);

        let store = SqliteUiDataStore::open(&path).unwrap();
        let connection = Connection::open(&path).unwrap();
        let columns = connection
            .prepare("PRAGMA table_info(ui_wallet_transactions)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let schema_version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .unwrap();
        assert!(!columns.iter().any(|column| column == "blinded"));
        assert_eq!(schema_version, super::UI_DATA_SCHEMA_VERSION);
        drop(connection);

        let seed = "legacy-ui-schema";
        let snapshot = test_snapshot(seed);
        let tip = snapshot.blocks.last().unwrap().hash.clone();
        let wallet = Wallet::from_seed(seed);
        store.project_snapshot(&snapshot, true).unwrap();
        let (_, total) = store
            .load_wallet_transactions(wallet.address(), &["burn"], 0, 10)
            .unwrap();
        assert!(total > 0);

        drop(store);
        let reopened = SqliteUiDataStore::open(&path).unwrap();
        assert!(reopened.is_projected_to(&tip).unwrap());
        assert!(reopened.metrics_are_projected_to(&tip).unwrap());
    }

    #[test]
    fn failed_ui_projection_keeps_last_committed_projection() {
        let dir = tempdir().unwrap();
        let store = SqliteUiDataStore::open(dir.path().join("ui_data.sqlite3")).unwrap();
        let snapshot = test_snapshot("ui-data-rollback");
        let tip = snapshot.blocks.last().unwrap().hash.clone();
        store.project_snapshot(&snapshot, true).unwrap();
        assert!(store.is_projected_to(&tip).unwrap());
        assert!(store.metrics_are_projected_to(&tip).unwrap());
        assert!(!store.load_metrics().unwrap().is_empty());

        let mut invalid = snapshot.clone();
        invalid.blocks.clear();
        let error = store.project_snapshot(&invalid, true).unwrap_err();

        assert!(
            format!("{error:#}").contains("chain snapshot is empty"),
            "{error:#}"
        );
        assert!(store.is_projected_to(&tip).unwrap());
        assert!(store.metrics_are_projected_to(&tip).unwrap());
        assert!(!store.load_metrics().unwrap().is_empty());
    }

    #[test]
    fn metrics_projection_appends_without_rewriting_the_consistent_prefix() {
        let dir = tempdir().unwrap();
        let store = SqliteUiDataStore::open(dir.path().join("ui_data.sqlite3")).unwrap();
        let (mut ledger, wallet) = test_ledger("incremental-metrics");
        append_test_block(&mut ledger, &wallet, 1_000);
        append_test_block(&mut ledger, &wallet, 2_000);
        store.project_snapshot(&ledger.snapshot(), true).unwrap();
        let prefix = store.load_metrics().unwrap();

        let connection = Connection::open(store.path()).unwrap();
        connection
            .execute_batch(
                r#"
CREATE TRIGGER protect_metric_prefix
BEFORE DELETE ON block_metrics
WHEN OLD.height <= 2
BEGIN
    SELECT RAISE(FAIL, 'consistent metric prefix was rewritten');
END;
"#,
            )
            .unwrap();
        drop(connection);

        append_test_block(&mut ledger, &wallet, 3_000);
        let snapshot = ledger.snapshot();
        store.project_snapshot(&snapshot, true).unwrap();
        let metrics = store.load_metrics().unwrap();

        assert_eq!(&metrics[..prefix.len()], prefix.as_slice());
        assert_eq!(metrics.len(), snapshot.blocks.len());
        assert_eq!(metrics.last().unwrap().block_hash, ledger.tip_hash());
        assert_eq!(
            metrics.last().unwrap().circulating_supply,
            ledger
                .all_utxos()
                .iter()
                .map(|(_, output)| output.amount)
                .sum::<u64>()
        );
        assert_eq!(
            metrics.last().unwrap().mine_difficulty_bits,
            ledger.mine_difficulty_bits_at_height(ledger.height())
        );
    }

    #[test]
    fn metrics_projection_replaces_only_the_reorged_suffix() {
        let dir = tempdir().unwrap();
        let store = SqliteUiDataStore::open(dir.path().join("ui_data.sqlite3")).unwrap();
        let (mut base, wallet) = test_ledger("reorg-metrics");
        append_test_block(&mut base, &wallet, 1_000);
        let mut first_branch = base.clone();
        let mut second_branch = base;
        append_test_block(&mut first_branch, &wallet, 2_000);
        append_test_block(&mut second_branch, &wallet, 3_000);
        store
            .project_snapshot(&first_branch.snapshot(), true)
            .unwrap();

        let connection = Connection::open(store.path()).unwrap();
        connection
            .execute_batch(
                r#"
CREATE TRIGGER protect_metric_common_ancestor
BEFORE DELETE ON block_metrics
WHEN OLD.height <= 1
BEGIN
    SELECT RAISE(FAIL, 'metric common ancestor was rewritten');
END;
"#,
            )
            .unwrap();
        drop(connection);

        store
            .project_snapshot(&second_branch.snapshot(), true)
            .unwrap();
        let metrics = store.load_metrics().unwrap();

        assert_eq!(metrics.len(), second_branch.chain().len());
        assert_eq!(metrics.last().unwrap().block_hash, second_branch.tip_hash());
        assert!(
            store
                .metrics_are_projected_to(second_branch.tip_hash())
                .unwrap()
        );
        assert!(
            !store
                .metrics_are_projected_to(first_branch.tip_hash())
                .unwrap()
        );
    }

    #[test]
    fn incremental_metrics_match_the_ledger_across_a_difficulty_retarget() {
        let dir = tempdir().unwrap();
        let store = SqliteUiDataStore::open(dir.path().join("ui_data.sqlite3")).unwrap();
        let (mut ledger, wallet) = test_ledger("retarget-metrics");

        store.project_snapshot(&ledger.snapshot(), true).unwrap();
        for height in 1..=super::MINE_RETARGET_WINDOW_BLOCKS + 2 {
            append_test_block(&mut ledger, &wallet, height * 1_000);
            store.project_snapshot(&ledger.snapshot(), true).unwrap();
            let latest = store.load_metrics().unwrap().pop().unwrap();

            assert_eq!(latest.height, height);
            assert_eq!(
                latest.mine_difficulty_bits,
                ledger.mine_difficulty_bits_at_height(height)
            );
            assert_eq!(
                latest.circulating_supply,
                ledger
                    .all_utxos()
                    .iter()
                    .map(|(_, output)| output.amount)
                    .sum::<u64>()
            );
        }
    }

    #[test]
    fn leaderboards_are_served_from_their_materialized_projection() {
        let dir = tempdir().unwrap();
        let store = SqliteUiDataStore::open(dir.path().join("ui_data.sqlite3")).unwrap();
        let (mut ledger, wallet) = test_ledger("materialized-leaderboards");
        append_test_block(&mut ledger, &wallet, 1_000);
        store.project_snapshot(&ledger.snapshot(), true).unwrap();
        let expected = store.load_leaderboards(10).unwrap();
        assert!(!expected.balances.is_empty());
        assert!(!expected.burners.is_empty());

        let connection = Connection::open(store.path()).unwrap();
        connection.execute("DELETE FROM ui_utxos", []).unwrap();
        connection
            .execute("DELETE FROM ui_wallet_transactions", [])
            .unwrap();

        assert_eq!(store.load_leaderboards(10).unwrap(), expected);
    }
}
