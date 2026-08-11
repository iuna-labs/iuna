use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use serde::Serialize;

use crate::{
    adapters::ui_index::{UiChainIndex, build_ui_chain_index},
    domain::{
        AGGREGATE_FINALIZER_FEE_ACTIVATION_HEIGHT, Amount, BLINDED_COMMITTER_FEE_BPS,
        BLINDED_REVEAL_BUNDLE_SIGNER_FEE_BPS, BlindedTransaction, Block, BurnLeaderRank,
        ChainSnapshot, Ledger, MINE_REWARD, OutPoint, REVEAL_COMMITTEE_SIZE,
        RevealedBlindedTransaction, Transaction, TxInput, TxOutput, blinded_reveal_finalizer_fee,
        hex_hash, reveal_committee_slot_count, revealed_blinded_transactions,
    },
};

mod compact;
use compact::{blinded_fee_share, decode_compact_snapshot, encode_compact_snapshot};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS chain_snapshots (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    height INTEGER NOT NULL,
    tip_hash TEXT NOT NULL,
    snapshot_blob BLOB NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

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

CREATE TABLE IF NOT EXISTS ui_revealed_transactions (
    height INTEGER NOT NULL,
    commitment TEXT PRIMARY KEY,
    included_by TEXT NOT NULL,
    transaction_json BLOB NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_ui_revealed_transactions_height
ON ui_revealed_transactions(height);

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

const UI_CACHE_SCHEMA_VERSION: u32 = 1;

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
                .context("failed to initialize chain database schema")?;
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

    pub fn load(&self) -> Result<Option<ChainSnapshot>> {
        self.with_connection(|connection| {
            let snapshot_blob = connection
                .query_row(
                    "SELECT snapshot_blob FROM chain_snapshots WHERE id = 1",
                    [],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()
                .context("failed to load chain snapshot from database")?;

            snapshot_blob
                .map(|blob| {
                    decode_compact_snapshot(&blob)
                        .context("failed to parse compact chain snapshot from database")
                })
                .transpose()
        })
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
                revealed_by_height: load_ui_revealed_transactions(connection)?,
                burn_leader_ranks_by_hash: load_ui_burn_leader_ranks(connection)?,
            }))
        })
    }

    pub fn save(&self, snapshot: &ChainSnapshot) -> Result<()> {
        self.save_with_metrics(snapshot, false)
    }

    pub fn save_with_metrics(&self, snapshot: &ChainSnapshot, keep_metrics: bool) -> Result<()> {
        let (height, tip_hash) = snapshot_tip(snapshot).context("cannot persist empty chain")?;
        let snapshot_blob =
            encode_compact_snapshot(snapshot).context("failed to encode compact chain snapshot")?;
        let updated_at_ms = unix_ms();
        let ui_index = build_ui_chain_index(snapshot);
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
            match metrics {
                Some(metrics) => replace_metrics(&transaction, &metrics)?,
                None => clear_metrics_in_transaction(&transaction)?,
            }
            replace_ui_chain_index(&transaction, &ui_index, updated_at_ms)?;
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

    pub fn clear_chain(&self) -> Result<()> {
        self.with_connection_mut(|connection| {
            let transaction = connection
                .transaction()
                .context("failed to start chain reset transaction")?;
            transaction
                .execute("DELETE FROM chain_snapshots", [])
                .context("failed to delete chain snapshot")?;
            clear_metrics_in_transaction(&transaction)?;
            clear_ui_chain_index_in_transaction(&transaction)?;
            transaction
                .commit()
                .context("failed to commit chain reset transaction")?;
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
    for (height, revealed_transactions) in &index.revealed_by_height {
        for revealed in revealed_transactions {
            let transaction_json = serde_json::to_vec(&revealed.transaction)
                .context("failed to serialize UI revealed transaction")?;
            transaction
                .execute(
                    r#"
INSERT INTO ui_revealed_transactions (height, commitment, included_by, transaction_json)
VALUES (?1, ?2, ?3, ?4)
"#,
                    params![
                        height,
                        revealed.commitment,
                        revealed.included_by,
                        transaction_json
                    ],
                )
                .with_context(|| {
                    format!(
                        "failed to persist UI revealed transaction {}",
                        revealed.commitment
                    )
                })?;
        }
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

fn clear_ui_chain_index_in_transaction(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    transaction
        .execute("DELETE FROM ui_cache_meta", [])
        .context("failed to clear old UI cache metadata")?;
    transaction
        .execute("DELETE FROM ui_output_index", [])
        .context("failed to clear old UI output index")?;
    transaction
        .execute("DELETE FROM ui_revealed_transactions", [])
        .context("failed to clear old UI revealed transaction index")?;
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

fn load_ui_revealed_transactions(
    connection: &Connection,
) -> Result<BTreeMap<u64, Vec<RevealedBlindedTransaction>>> {
    let mut statement = connection
        .prepare(
            r#"
SELECT height, commitment, included_by, transaction_json
FROM ui_revealed_transactions
ORDER BY height, commitment
"#,
        )
        .context("failed to prepare UI revealed transaction query")?;
    let rows = statement
        .query_map([], |row| {
            let transaction_json = row.get::<_, Vec<u8>>(3)?;
            let transaction =
                serde_json::from_slice::<Transaction>(&transaction_json).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        transaction_json.len(),
                        rusqlite::types::Type::Blob,
                        Box::new(error),
                    )
                })?;
            Ok(RevealedBlindedTransaction {
                height: row.get(0)?,
                commitment: row.get(1)?,
                included_by: row.get(2)?,
                transaction,
            })
        })
        .context("failed to load UI revealed transactions")?;
    let mut by_height = BTreeMap::<u64, Vec<RevealedBlindedTransaction>>::new();
    for revealed in rows {
        let revealed = revealed.context("failed to read UI revealed transaction row")?;
        by_height.entry(revealed.height).or_default().push(revealed);
    }
    Ok(by_height)
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
    let revealed = revealed_blinded_transactions(snapshot)?.into_iter().fold(
        BTreeMap::<u64, Vec<crate::domain::RevealedBlindedTransaction>>::new(),
        |mut by_height, revealed| {
            by_height.entry(revealed.height).or_default().push(revealed);
            by_height
        },
    );
    let mut known_wallet_addresses = snapshot
        .genesis_allocations
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut total_burned_amount = 0_u64;
    let mut rows = Vec::with_capacity(snapshot.blocks.len());
    let mut previous_timestamp_ms = None;
    let mut active_blinded = BTreeMap::<String, BlindedTransaction>::new();
    let mut metric_utxos = metric_genesis_utxos(snapshot);
    let mut metric_locked_blinded_inputs = BTreeMap::<String, Amount>::new();
    let reveal_bundle_slots_by_height = ledger
        .burn_leader_ranks_for_blocks(snapshot.blocks.iter().map(|block| block.height))
        .map(|ranks_by_height| {
            ranks_by_height
                .into_iter()
                .map(|(height, ranks)| (height, reveal_committee_slot_count(ranks.len())))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    for block in &snapshot.blocks {
        let revealed_transactions = revealed.get(&block.height).cloned().unwrap_or_default();
        let mut transfer_count = 0_u64;
        let mut burn_count = 0_u64;
        let mut mine_count = 0_u64;
        let mut burned_amount = 0_u64;
        let mut burned_fee_amount = 0_u64;
        let mut fees_amount = 0_u64;

        known_wallet_addresses.insert(block.miner.clone());
        for signature in &block.reveal_bundle_section.signatures {
            known_wallet_addresses.insert(signature.member.clone());
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
        for revealed in &revealed_transactions {
            let transaction = &revealed.transaction;
            known_wallet_addresses.insert(revealed.included_by.clone());
            collect_transaction_addresses(transaction, &mut known_wallet_addresses);
            metric_index_transaction_outputs(&mut metric_utxos, transaction);
            metric_index_blinded_fee_outputs(
                &mut metric_utxos,
                &revealed.commitment,
                &revealed.included_by,
                block,
                transaction.fee(),
                reveal_bundle_slots_by_height
                    .get(&block.height)
                    .copied()
                    .unwrap_or(REVEAL_COMMITTEE_SIZE),
            );
            fees_amount = fees_amount
                .checked_add(transaction.fee())
                .context("block metric fees overflow")?;
            let committer_fee = blinded_fee_share(transaction.fee(), BLINDED_COMMITTER_FEE_BPS);
            let included_reveal_bundle_count = block.included_reveal_bundle_count();
            let available_reveal_bundle_slots = reveal_bundle_slots_by_height
                .get(&block.height)
                .copied()
                .unwrap_or(REVEAL_COMMITTEE_SIZE);
            let reveal_finalizer_fee = blinded_reveal_finalizer_fee(
                transaction.fee(),
                included_reveal_bundle_count,
                available_reveal_bundle_slots,
            );
            let reveal_bundle_signer_fees =
                blinded_fee_share(transaction.fee(), BLINDED_REVEAL_BUNDLE_SIGNER_FEE_BPS)
                    .saturating_mul(included_reveal_bundle_count as u64);
            let distributed_fee = committer_fee
                .saturating_add(reveal_finalizer_fee)
                .saturating_add(reveal_bundle_signer_fees);
            burned_fee_amount = burned_fee_amount
                .checked_add(transaction.fee().saturating_sub(distributed_fee))
                .context("block metric burned fees overflow")?;
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
        let revealed_commitments = block
            .all_blinded_reveals()
            .into_iter()
            .map(|reveal| reveal.commitment.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let mut expired_blinded_fee_values = Vec::new();
        active_blinded.retain(|commitment, transaction| {
            if revealed_commitments.contains(commitment) {
                metric_locked_blinded_inputs.remove(commitment);
                return false;
            }
            if block.height >= transaction.expires_at_height {
                if !transaction.inputs.is_empty() {
                    expired_blinded_fee_values.push(transaction.fee);
                    metric_index_expired_blinded_change(
                        &mut metric_utxos,
                        commitment,
                        transaction,
                        metric_locked_blinded_inputs
                            .remove(commitment)
                            .unwrap_or_default(),
                    );
                }
                return false;
            }
            true
        });
        let expired_blinded_fees =
            expired_blinded_fee_values
                .into_iter()
                .try_fold(0_u64, |total, fee| {
                    total
                        .checked_add(fee)
                        .context("block metric expiry fees overflow")
                })?;
        fees_amount = fees_amount
            .checked_add(expired_blinded_fees)
            .context("block metric expiry fees overflow")?;
        burned_fee_amount = burned_fee_amount
            .checked_add(expired_blinded_fees)
            .context("block metric expired burned fees overflow")?;
        for transaction in &block.blinded_transactions {
            for input in &transaction.inputs {
                known_wallet_addresses.insert(input.owner.clone());
            }
            let locked_total = metric_spend_blinded_inputs(transaction, &mut metric_utxos)?;
            metric_locked_blinded_inputs.insert(transaction.commitment.clone(), locked_total);
            active_blinded.insert(transaction.commitment.clone(), transaction.clone());
        }
        total_burned_amount = total_burned_amount
            .checked_add(burned_amount)
            .and_then(|amount| amount.checked_add(burned_fee_amount))
            .context("total burned metric overflows")?;

        if block.height > 0 {
            running_ledger
                .apply_preverified_block_at(block.clone(), u64::MAX)
                .with_context(|| format!("failed to replay block {} for metrics", block.height))?;
        }
        metric_index_block_reward(&mut metric_utxos, block);
        let circulating_supply = ledger_circulating_supply(&running_ledger)?
            .checked_add(metric_locked_supply(&metric_locked_blinded_inputs)?)
            .context("circulating supply metric overflows")?;
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

fn metric_locked_supply(locked: &BTreeMap<String, Amount>) -> Result<Amount> {
    locked.values().try_fold(0_u64, |total, amount| {
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

fn metric_spend_blinded_inputs(
    transaction: &BlindedTransaction,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) -> Result<Amount> {
    metric_spend_inputs(&transaction.inputs, utxos)
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

fn metric_index_blinded_fee_outputs(
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
    commitment: &str,
    included_by: &str,
    block: &Block,
    fee: Amount,
    available_reveal_bundle_slots: usize,
) {
    if fee == 0 {
        return;
    }
    let committer_fee = blinded_fee_share(fee, BLINDED_COMMITTER_FEE_BPS);
    if committer_fee > 0 {
        utxos.insert(
            metric_blinded_committer_fee_outpoint(commitment),
            TxOutput {
                address: included_by.to_string(),
                amount: committer_fee,
            },
        );
    }
    let reveal_finalizer_fee = blinded_reveal_finalizer_fee(
        fee,
        block.included_reveal_bundle_count(),
        available_reveal_bundle_slots,
    );
    if reveal_finalizer_fee > 0 && block.height < AGGREGATE_FINALIZER_FEE_ACTIVATION_HEIGHT {
        utxos.insert(
            metric_blinded_executor_fee_outpoint(commitment),
            TxOutput {
                address: block.miner.clone(),
                amount: reveal_finalizer_fee,
            },
        );
    }
    let reveal_bundle_signer_fee = blinded_fee_share(fee, BLINDED_REVEAL_BUNDLE_SIGNER_FEE_BPS);
    if reveal_bundle_signer_fee > 0 {
        for signature in &block.reveal_bundle_section.signatures {
            utxos.insert(
                metric_blinded_reveal_bundle_signer_fee_outpoint(commitment, signature.slot),
                TxOutput {
                    address: signature.member.clone(),
                    amount: reveal_bundle_signer_fee,
                },
            );
        }
    }
}

fn metric_index_expired_blinded_change(
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
    commitment: &str,
    transaction: &BlindedTransaction,
    locked_total: Amount,
) {
    let Some(first_input) = transaction.inputs.first() else {
        return;
    };
    let change = locked_total.saturating_sub(transaction.fee);
    if change == 0 {
        return;
    }
    utxos.insert(
        metric_blinded_expiry_change_outpoint(commitment),
        TxOutput {
            address: first_input.owner.clone(),
            amount: change,
        },
    );
}

fn metric_index_block_reward(utxos: &mut BTreeMap<OutPoint, TxOutput>, block: &Block) {
    if block.reward == 0 {
        return;
    }
    utxos.insert(
        metric_reward_outpoint(&block.hash),
        TxOutput {
            address: block.miner.clone(),
            amount: block.reward,
        },
    );
}

fn metric_genesis_allocation_outpoint(address: &str) -> OutPoint {
    OutPoint {
        txid: hex_hash(format!("iuna-genesis-allocation:{address}")),
        index: 0,
    }
}

fn metric_reward_outpoint(block_hash: &str) -> OutPoint {
    OutPoint {
        txid: block_hash.to_string(),
        index: u32::MAX,
    }
}

fn metric_blinded_committer_fee_outpoint(commitment: &str) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: u32::MAX - 1,
    }
}

fn metric_blinded_executor_fee_outpoint(commitment: &str) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: u32::MAX - 2,
    }
}

fn metric_blinded_reveal_bundle_signer_fee_outpoint(commitment: &str, slot: u8) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: u32::MAX - 3 - u32::from(slot),
    }
}

fn metric_blinded_expiry_change_outpoint(commitment: &str) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
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
mod tests;
