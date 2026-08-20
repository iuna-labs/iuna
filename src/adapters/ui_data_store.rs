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
        Amount, Block, BurnLeaderRank, ChainSnapshot, Ledger, MINE_REWARD, OutPoint, Transaction,
        TxInput, TxOutput, hex_hash, reward_outputs_for_block,
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

const UI_CACHE_SCHEMA_VERSION: u32 = 2;

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
            connection
                .execute_batch(SCHEMA)
                .context("failed to initialize UI data database schema")?;
            ensure_block_metrics_column(
                connection,
                "known_wallet_addresses",
                "INTEGER NOT NULL DEFAULT 0",
            )?;
            Ok(())
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

    pub(crate) fn is_projected_to(&self, tip_hash: &str) -> Result<bool> {
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

    pub fn project_snapshot(&self, snapshot: &ChainSnapshot, keep_metrics: bool) -> Result<()> {
        let updated_at_ms = unix_ms();
        let ui_index = build_ui_chain_index(snapshot);
        let utxos = Ledger::from_persisted_snapshot(snapshot.clone())
            .context("failed to rebuild ledger for UI UTXO projection")?
            .all_utxos();
        let wallet_transactions = wallet_transactions_from_snapshot(snapshot);
        let metrics = if keep_metrics {
            Some(metrics_from_snapshot(snapshot)?)
        } else {
            None
        };

        self.with_connection_mut(|connection| {
            let transaction = connection
                .transaction()
                .context("failed to start UI data projection transaction")?;
            match metrics {
                Some(metrics) => replace_metrics(&transaction, &metrics)?,
                None => clear_metrics_in_transaction(&transaction)?,
            }
            replace_ui_chain_index(&transaction, &ui_index, updated_at_ms)?;
            replace_ui_utxos(&transaction, &utxos)?;
            replace_ui_wallet_transactions(&transaction, &wallet_transactions)?;
            transaction
                .commit()
                .context("failed to commit UI data projection transaction")?;
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
        self.with_connection_mut(|connection| {
            connection
                .execute("DELETE FROM block_metrics", [])
                .context("failed to delete block metrics")?;
            Ok(())
        })
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
    let mut statement = connection
        .prepare(
            r#"
SELECT address, SUM(amount) AS total_amount, COUNT(*) AS output_count
FROM ui_utxos
GROUP BY address
HAVING total_amount > 0
ORDER BY total_amount DESC, address ASC
LIMIT ?1
"#,
        )
        .context("failed to prepare balance leaderboard query")?;
    let rows = statement
        .query_map([limit as u64], |row| {
            Ok(UiLeaderboardEntry {
                address: row.get(0)?,
                amount: row.get(1)?,
                count: row.get(2)?,
            })
        })
        .context("failed to load balance leaderboard")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read balance leaderboard rows")
}

fn load_transaction_leaderboard(
    connection: &Connection,
    kind: &str,
    limit: usize,
) -> Result<Vec<UiLeaderboardEntry>> {
    let mut statement = connection
        .prepare(
            r#"
SELECT address, transaction_json
FROM ui_wallet_transactions
WHERE kind = ?1
"#,
        )
        .with_context(|| format!("failed to prepare {kind} leaderboard query"))?;
    let rows = statement
        .query_map([kind], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .with_context(|| format!("failed to load {kind} leaderboard"))?;
    let mut entries = BTreeMap::<String, UiLeaderboardEntry>::new();
    for row in rows {
        let (address, transaction_json) =
            row.with_context(|| format!("failed to read {kind} leaderboard row"))?;
        let transaction =
            serde_json::from_slice::<Transaction>(&transaction_json).with_context(|| {
                format!(
                    "failed to parse {kind} leaderboard transaction JSON with {} bytes",
                    transaction_json.len()
                )
            })?;
        let amount = match transaction {
            Transaction::Mine { .. } => MINE_REWARD,
            Transaction::Burn { amount, .. } => amount,
            Transaction::Transfer { .. } => 0,
        };
        let entry = entries
            .entry(address.clone())
            .or_insert_with(|| UiLeaderboardEntry {
                address,
                amount: 0,
                count: 0,
            });
        entry.amount = entry
            .amount
            .checked_add(amount)
            .with_context(|| format!("{kind} leaderboard amount overflow"))?;
        entry.count = entry
            .count
            .checked_add(1)
            .with_context(|| format!("{kind} leaderboard count overflow"))?;
    }
    let mut entries = entries.into_values().collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .amount
            .cmp(&left.amount)
            .then_with(|| right.count.cmp(&left.count))
            .then_with(|| left.address.cmp(&right.address))
    });
    entries.truncate(limit);
    Ok(entries)
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
    }
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

fn metrics_from_snapshot(snapshot: &ChainSnapshot) -> Result<Vec<BlockMetricRow>> {
    let ledger = Ledger::from_persisted_snapshot(snapshot.clone())
        .context("failed to rebuild ledger for metrics")?;
    let genesis = snapshot
        .blocks
        .first()
        .cloned()
        .context("cannot compute metrics for empty chain snapshot")?;
    let mut running_ledger = Ledger::from_persisted_snapshot(ChainSnapshot {
        genesis_allocations: snapshot.genesis_allocations.clone(),
        vdf_rounds: snapshot.vdf_rounds,
        launch_profile: snapshot.launch_profile.clone(),
        blocks: vec![genesis],
    })
    .context("failed to rebuild genesis ledger for metrics")?;
    let mut known_wallet_addresses = snapshot
        .genesis_allocations
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut total_burned_amount = 0_u64;
    let mut rows = Vec::with_capacity(snapshot.blocks.len());
    let mut previous_timestamp_ms = None;
    let mut metric_utxos = metric_genesis_utxos(snapshot);

    for block in &snapshot.blocks {
        let reward_committee = if block.height == 0 {
            Vec::new()
        } else {
            running_ledger.burn_committee_for_block(block)
        };
        let mut transfer_count = 0_u64;
        let mut burn_count = 0_u64;
        let mut mine_count = 0_u64;
        let mut burned_amount = 0_u64;
        let burned_fee_amount = 0_u64;
        let mut fees_amount = 0_u64;

        known_wallet_addresses.insert(block.miner.clone());
        for signature in &block.burn_bundle_section.signatures {
            known_wallet_addresses.insert(signature.member.clone());
        }
        for (_, output) in reward_outputs_for_block(block, &reward_committee) {
            known_wallet_addresses.insert(output.address);
        }
        for transaction in &block.transactions {
            collect_transaction_addresses(transaction, &mut known_wallet_addresses);
            metric_apply_public_transaction(transaction, &mut metric_utxos)?;
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
                }
            }
        }
        metric_index_block_reward(&mut metric_utxos, block, &reward_committee);
        total_burned_amount = total_burned_amount
            .checked_add(burned_amount)
            .and_then(|amount| amount.checked_add(burned_fee_amount))
            .context("total burned metric overflows")?;

        if block.height > 0 {
            running_ledger
                .apply_preverified_block_at(block.clone(), u64::MAX)
                .with_context(|| format!("failed to replay block {} for metrics", block.height))?;
        }
        let circulating_supply = ledger_circulating_supply(&running_ledger)?;
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
            known_wallet_addresses: known_wallet_addresses.len() as u64,
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
        });
    }
    Ok(rows)
}

fn ledger_circulating_supply(ledger: &Ledger) -> Result<Amount> {
    ledger
        .status()
        .balances
        .values()
        .try_fold(0_u64, |total, amount| {
            total
                .checked_add(*amount)
                .context("circulating supply metric overflows")
        })
}

fn metric_genesis_utxos(snapshot: &ChainSnapshot) -> BTreeMap<OutPoint, TxOutput> {
    snapshot
        .genesis_allocations
        .iter()
        .filter(|(_, amount)| **amount > 0)
        .map(|(address, amount)| {
            (
                metric_genesis_allocation_outpoint(address),
                TxOutput {
                    address: address.clone(),
                    amount: *amount,
                },
            )
        })
        .collect()
}

fn metric_apply_public_transaction(
    transaction: &Transaction,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) -> Result<()> {
    metric_spend_transaction_inputs(transaction, utxos)?;
    metric_index_transaction_outputs(utxos, transaction);
    Ok(())
}

fn metric_spend_transaction_inputs(
    transaction: &Transaction,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) -> Result<Amount> {
    let inputs = match transaction {
        Transaction::Transfer { inputs, .. } | Transaction::Burn { inputs, .. } => inputs,
        Transaction::Mine { .. } => return Ok(0),
    };
    metric_spend_inputs(inputs, utxos)
}

fn metric_spend_inputs(
    inputs: &[TxInput],
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) -> Result<Amount> {
    inputs.iter().try_fold(0_u64, |total, input| {
        let output = utxos.remove(&input.outpoint).with_context(|| {
            format!(
                "metric replay spends missing output {}:{}",
                input.outpoint.txid, input.outpoint.index
            )
        })?;
        total
            .checked_add(output.amount)
            .context("metric replay input total overflows")
    })
}

fn metric_index_transaction_outputs(
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
    transaction: &Transaction,
) {
    let outputs = match transaction {
        Transaction::Transfer { outputs, .. } => outputs.clone(),
        Transaction::Burn { change, .. } => change.clone(),
        Transaction::Mine { recipient, .. } => vec![TxOutput {
            address: recipient.clone(),
            amount: MINE_REWARD,
        }],
    };
    for (index, output) in outputs.iter().enumerate() {
        utxos.insert(
            OutPoint {
                txid: transaction.signature().to_string(),
                index: index as u32,
            },
            output.clone(),
        );
    }
}

fn metric_index_block_reward(
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
    block: &Block,
    committee: &[crate::domain::BurnCommitteeMember],
) {
    for (outpoint, output) in reward_outputs_for_block(block, committee) {
        utxos.insert(outpoint, output);
    }
}

fn metric_genesis_allocation_outpoint(address: &str) -> OutPoint {
    OutPoint {
        txid: hex_hash(format!("iuna-genesis-allocation:{address}")),
        index: 0,
    }
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

    #[test]
    fn failed_ui_projection_keeps_last_committed_projection() {
        let dir = tempdir().unwrap();
        let store = SqliteUiDataStore::open(dir.path().join("ui_data.sqlite3")).unwrap();
        let snapshot = test_snapshot("ui-data-rollback");
        let tip = snapshot.blocks.last().unwrap().hash.clone();
        store.project_snapshot(&snapshot, true).unwrap();
        assert!(store.is_projected_to(&tip).unwrap());
        assert!(!store.load_metrics().unwrap().is_empty());

        let mut invalid = snapshot.clone();
        invalid.blocks.clear();
        let error = store.project_snapshot(&invalid, true).unwrap_err();

        assert!(
            format!("{error:#}").contains("chain snapshot is empty"),
            "{error:#}"
        );
        assert!(store.is_projected_to(&tip).unwrap());
        assert!(!store.load_metrics().unwrap().is_empty());
    }
}
