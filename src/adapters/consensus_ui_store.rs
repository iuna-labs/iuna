//! Rebuildable screen-data projection for `iuna-mainnet-candidate`.
//!
//! Consensus blocks remain in the chain database. This adapter writes only derived rows to the
//! separately configured UI database and can replace an abandoned suffix without touching other
//! UI tables.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::{
    adapters::consensus_http::block_detail_view,
    consensus::{Chain, Hash, block_payouts},
};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS consensus_ui_status (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    height INTEGER NOT NULL,
    tip_hash TEXT NOT NULL,
    ledger_blocks INTEGER NOT NULL,
    minted INTEGER NOT NULL,
    tracked_tickets INTEGER NOT NULL,
    utxos INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS consensus_ui_block_details (
    height INTEGER PRIMARY KEY,
    block_hash TEXT NOT NULL UNIQUE,
    detail_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS consensus_ui_heartbeats (
    height INTEGER PRIMARY KEY,
    block_hash TEXT NOT NULL UNIQUE,
    parent_hash TEXT,
    timestamp INTEGER NOT NULL,
    miner_public_key TEXT NOT NULL,
    target TEXT NOT NULL,
    ticket_count INTEGER NOT NULL,
    has_ledger_block INTEGER NOT NULL,
    ledger_transaction_count INTEGER NOT NULL,
    ticket_budget INTEGER NOT NULL DEFAULT 0,
    heartbeat_bytes INTEGER NOT NULL DEFAULT 0,
    chain_bytes INTEGER NOT NULL DEFAULT 0,
    interval_seconds INTEGER,
    minted INTEGER NOT NULL DEFAULT 0,
    utxos INTEGER NOT NULL DEFAULT 0,
    tracked_tickets INTEGER NOT NULL DEFAULT 0,
    ledger_blocks INTEGER NOT NULL DEFAULT 0,
    active_tickets INTEGER NOT NULL DEFAULT 0,
    ticket_weight INTEGER NOT NULL DEFAULT 0,
    ledger_miner_public_key TEXT,
    heartbeat_reward INTEGER NOT NULL DEFAULT 0,
    ledger_reward INTEGER NOT NULL DEFAULT 0
);
"#;

/// Roughly what the Metrics charts can draw usefully; about 2 MB of JSON.
const MAX_METRIC_ROWS: u64 = 5_000;

const HEARTBEAT_METRIC_COLUMNS: &[(&str, &str)] = &[
    ("ticket_budget", "INTEGER NOT NULL DEFAULT 0"),
    ("heartbeat_bytes", "INTEGER NOT NULL DEFAULT 0"),
    ("chain_bytes", "INTEGER NOT NULL DEFAULT 0"),
    ("interval_seconds", "INTEGER"),
    ("minted", "INTEGER NOT NULL DEFAULT 0"),
    ("utxos", "INTEGER NOT NULL DEFAULT 0"),
    ("tracked_tickets", "INTEGER NOT NULL DEFAULT 0"),
    ("ledger_blocks", "INTEGER NOT NULL DEFAULT 0"),
    ("active_tickets", "INTEGER NOT NULL DEFAULT 0"),
    ("ticket_weight", "INTEGER NOT NULL DEFAULT 0"),
    ("ledger_miner_public_key", "TEXT"),
    ("heartbeat_reward", "INTEGER NOT NULL DEFAULT 0"),
    ("ledger_reward", "INTEGER NOT NULL DEFAULT 0"),
];

#[derive(Clone, Debug)]
pub struct ConsensusUiStore {
    path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiStatus {
    pub height: u64,
    pub tip_hash: String,
    pub ledger_blocks: u64,
    pub minted: u64,
    pub tracked_tickets: usize,
    pub utxos: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiHeartbeat {
    pub height: u64,
    pub block_hash: String,
    pub parent_hash: Option<String>,
    pub timestamp: u64,
    pub miner_public_key: String,
    /// The winning ticket's mining key, for Heartbeats carrying a Ledger block.
    pub ledger_miner_public_key: Option<String>,
    pub target: String,
    pub ticket_count: usize,
    pub has_ledger_block: bool,
    pub ledger_transaction_count: usize,
    pub ticket_budget: u64,
    pub heartbeat_bytes: u64,
    pub chain_bytes: u64,
    pub interval_seconds: Option<u64>,
    pub minted: u64,
    pub utxos: usize,
    pub tracked_tickets: usize,
    pub ledger_blocks: u64,
    pub active_tickets: usize,
    pub ticket_weight: u64,
}

/// A wallet's rewards from one block.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WalletRewards {
    pub heartbeat: u64,
    pub ledger: u64,
}

impl ConsensusUiStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let connection = Connection::open(&path)
            .with_context(|| format!("opening UI database {}", path.display()))?;
        connection
            .execute_batch(SCHEMA)
            .context("creating UI schema")?;
        let added_columns = migrate_schema(&connection)?;
        discard_stale_rows(&connection, added_columns)?;
        Ok(Self { path })
    }

    pub fn project(&self, chain: &Chain) -> Result<()> {
        let batch = self.prepare_projection(chain)?;
        self.apply_projection(&batch)
    }

    /// Reads the chain, so it runs under the node lock. It only reads SQLite.
    pub fn prepare_projection(&self, chain: &Chain) -> Result<ProjectionBatch> {
        self.with_connection_mut(|connection| {
            let transaction = connection
                .transaction()
                .context("starting UI projection read")?;
            prepare_projection(&transaction, chain)
        })
    }

    /// Writes a prepared batch. It waits for a disk commit, so run it outside the node lock.
    pub fn apply_projection(&self, batch: &ProjectionBatch) -> Result<()> {
        self.with_connection_mut(|connection| {
            let transaction = connection.transaction().context("starting UI projection")?;
            apply_projection(&transaction, batch)?;
            transaction.commit().context("committing UI projection")
        })
    }

    /// The serialized `BlockDetailView` of a projected block.
    pub fn block_detail(&self, block_hash: &str) -> Result<Option<String>> {
        self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT detail_json FROM consensus_ui_block_details WHERE block_hash = ?1",
                    [block_hash],
                    |row| row.get(0),
                )
                .optional()
                .context("reading block detail")
        })
    }

    pub fn status(&self) -> Result<Option<UiStatus>> {
        self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT height, tip_hash, ledger_blocks, minted, tracked_tickets, utxos \
                     FROM consensus_ui_status WHERE id = 1",
                    [],
                    |row| {
                        Ok(UiStatus {
                            height: row.get(0)?,
                            tip_hash: row.get(1)?,
                            ledger_blocks: row.get(2)?,
                            minted: row.get(3)?,
                            tracked_tickets: row.get(4)?,
                            utxos: row.get(5)?,
                        })
                    },
                )
                .optional()
                .context("reading UI status")
        })
    }

    pub fn heartbeats_before(
        &self,
        before_height: Option<u64>,
        limit: usize,
    ) -> Result<Vec<UiHeartbeat>> {
        self.with_connection(|connection| {
            let mut statement = connection
                .prepare(
                    "SELECT height, block_hash, parent_hash, timestamp, miner_public_key, target, \
                     ticket_count, has_ledger_block, ledger_transaction_count \
                     , ticket_budget, heartbeat_bytes, chain_bytes, interval_seconds, minted, \
                     utxos, tracked_tickets, ledger_blocks, active_tickets, ticket_weight, ledger_miner_public_key \
                     FROM consensus_ui_heartbeats \
                     WHERE (?2 IS NULL OR height < ?2) \
                     ORDER BY height DESC LIMIT ?1",
                )
                .context("preparing heartbeat query")?;
            statement
                .query_map(
                    params![
                        limit.min(10_000),
                        before_height.map(|height| i64::try_from(height).unwrap_or(i64::MAX))
                    ],
                    decode_heartbeat,
                )
                .context("querying heartbeats")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("decoding heartbeat rows")
        })
    }

    /// What `public_key` earned in the newest `heartbeats` blocks, by block hash. The rewards are
    /// computed once at projection, so reading them never replays a block.
    pub fn wallet_rewards(
        &self,
        public_key: &str,
        heartbeats: u64,
    ) -> Result<BTreeMap<String, WalletRewards>> {
        self.wallet_rewards_before(public_key, None, heartbeats)
    }

    /// What `public_key` earned in the `heartbeats` blocks immediately before `before_height`.
    /// With no cursor the page ends at the projected tip.
    pub fn wallet_rewards_before(
        &self,
        public_key: &str,
        before_height: Option<u64>,
        heartbeats: u64,
    ) -> Result<BTreeMap<String, WalletRewards>> {
        self.with_connection(|connection| {
            let mut statement = connection.prepare(
                "WITH boundary(value) AS ( \
                   SELECT COALESCE(?2, COALESCE(MAX(height), -1) + 1) \
                   FROM consensus_ui_heartbeats \
                 ) \
                 SELECT block_hash, miner_public_key = ?1, ledger_miner_public_key IS ?1, \
                 heartbeat_reward, ledger_reward \
                 FROM consensus_ui_heartbeats, boundary \
                 WHERE height < boundary.value AND height >= boundary.value - ?3 \
                 AND (miner_public_key = ?1 OR ledger_miner_public_key = ?1)",
            )?;
            statement
                .query_map(params![public_key, before_height, heartbeats], |row| {
                    let (mined, won) = (row.get::<_, bool>(1)?, row.get::<_, bool>(2)?);
                    Ok((
                        row.get::<_, String>(0)?,
                        WalletRewards {
                            heartbeat: if mined { row.get(3)? } else { 0 },
                            ledger: if won { row.get(4)? } else { 0 },
                        },
                    ))
                })?
                .collect::<rusqlite::Result<_>>()
                .context("decoding wallet rewards")
        })
    }

    pub fn metrics(&self, limit: Option<usize>) -> Result<Vec<UiHeartbeat>> {
        self.metric_rows(limit, MAX_METRIC_ROWS)
    }

    /// Without a limit the whole chain is thinned to about `max_rows` evenly spaced Heartbeats plus
    /// the newest one, so the response stays bounded however long the chain gets.
    fn metric_rows(&self, limit: Option<usize>, max_rows: u64) -> Result<Vec<UiHeartbeat>> {
        let limit = limit
            .map(|value| i64::try_from(value).unwrap_or(i64::MAX))
            .unwrap_or(i64::MAX);
        self.with_connection(|connection| {
            let total = connection.query_row(
                "SELECT COUNT(*) FROM consensus_ui_heartbeats",
                [],
                |row| row.get::<_, u64>(0),
            )?;
            let step = if limit == i64::MAX {
                total.div_ceil(max_rows).max(1)
            } else {
                1
            };
            let mut statement = connection.prepare(
                "SELECT height, block_hash, parent_hash, timestamp, miner_public_key, target, \
                 ticket_count, has_ledger_block, ledger_transaction_count, ticket_budget, \
                 heartbeat_bytes, chain_bytes, interval_seconds, minted, utxos, tracked_tickets, \
                 ledger_blocks, active_tickets, ticket_weight, ledger_miner_public_key \
                 FROM consensus_ui_heartbeats \
                 WHERE height % ?2 = 0 \
                 OR height = (SELECT MAX(height) FROM consensus_ui_heartbeats) \
                 ORDER BY height DESC LIMIT ?1",
            )?;
            statement
                .query_map(params![limit, step], decode_heartbeat)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("decoding metric rows")
        })
    }

    pub fn clear(&self) -> Result<()> {
        self.with_connection_mut(|connection| {
            let transaction = connection.transaction().context("starting UI clear")?;
            transaction.execute("DELETE FROM consensus_ui_heartbeats", [])?;
            transaction.execute("DELETE FROM consensus_ui_block_details", [])?;
            transaction.execute("DELETE FROM consensus_ui_status", [])?;
            transaction.commit().context("committing UI clear")
        })
    }

    fn with_connection<T>(&self, operation: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let connection = Connection::open(&self.path)
            .with_context(|| format!("opening UI database {}", self.path.display()))?;
        operation(&connection)
    }

    fn with_connection_mut<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T>,
    ) -> Result<T> {
        let mut connection = Connection::open(&self.path)
            .with_context(|| format!("opening UI database {}", self.path.display()))?;
        operation(&mut connection)
    }
}

/// Everything a projection writes. Building it needs chain data; writing it only needs SQLite, so
/// the node stays available during the commit.
pub struct ProjectionBatch {
    common_height: Option<u64>,
    rows: Vec<ProjectionRow>,
    status: UiStatus,
}

struct ProjectionRow {
    detail_json: String,
    block_hash: String,
    parent_hash: Option<String>,
    height: u64,
    timestamp: u64,
    miner_public_key: String,
    ledger_miner_public_key: Option<String>,
    heartbeat_reward: u64,
    ledger_reward: u64,
    target: String,
    ticket_count: usize,
    has_ledger_block: bool,
    ledger_transaction_count: usize,
    ticket_budget: u64,
    heartbeat_bytes: u64,
    chain_bytes: u64,
    interval_seconds: Option<u64>,
    minted: u64,
    utxos: usize,
    tracked_tickets: usize,
    ledger_blocks: u64,
    active_tickets: usize,
    ticket_weight: u64,
}

fn prepare_projection(transaction: &Transaction<'_>, chain: &Chain) -> Result<ProjectionBatch> {
    let common_height = projection_common_height(transaction, chain)?;
    let (mut chain_bytes, mut previous_timestamp) = match common_height {
        Some(height) => transaction
            .query_row(
                "SELECT chain_bytes, timestamp FROM consensus_ui_heartbeats WHERE height = ?1",
                [height],
                |row| Ok((row.get::<_, u64>(0)?, Some(row.get::<_, u64>(1)?))),
            )
            .context("reading projection prefix")?,
        None => (0, None),
    };
    let mut rows = Vec::new();
    let mut previous_state = None;
    for block in chain.canonical_blocks_after(common_height) {
        let parent = match (previous_state.take(), block.header.parent) {
            (Some(state), _) => Some(state),
            (None, Some(parent)) => Some(
                chain
                    .state(&parent)
                    .with_context(|| format!("reading state of parent {}", hex(&parent)))?,
            ),
            (None, None) => None,
        };
        let height = block.header.height;
        let state = match &parent {
            Some(parent) => chain.child_state(parent, block),
            None => chain.state(&block.hash()),
        }
        .with_context(|| format!("reading state for Heartbeat {height}"))?;
        let ledger_miner_public_key = block
            .sidecar
            .as_ref()
            .zip(parent.as_ref())
            .and_then(|(sidecar, parent)| parent.tickets.get(&sidecar.header.ticket_id))
            .map(|ticket| hex(&ticket.mining_public_key));
        let payouts = parent
            .as_ref()
            .map(|parent| block_payouts(parent, block))
            .transpose()
            .with_context(|| format!("calculating rewards of Heartbeat {height}"))?
            .unwrap_or_default();
        let heartbeat_reward = payouts
            .heartbeat_total()
            .context("Heartbeat reward overflow")?;
        let ledger_reward = payouts
            .subsidy
            .checked_add(payouts.winner_fees)
            .context("Ledger reward overflow")?;
        let ledger_transaction_count = block
            .sidecar
            .as_ref()
            .map_or(0, |sidecar| sidecar.transactions.len());
        let ticket_budget = block.tickets.iter().try_fold(0_u64, |total, ticket| {
            total
                .checked_add(ticket.budget)
                .context("ticket budget overflow")
        })?;
        let heartbeat_bytes = u64::try_from(
            block.canonical_size()
                + block
                    .sidecar
                    .as_ref()
                    .map_or(0, |sidecar| sidecar.canonical_size()),
        )
        .context("Heartbeat byte size overflow")?;
        chain_bytes = chain_bytes
            .checked_add(heartbeat_bytes)
            .context("chain byte size overflow")?;
        let (active_tickets, ticket_weight) = active_ticket_metrics(&state, height)?;
        let detail = block_detail_view(block, parent.as_deref())
            .with_context(|| format!("building detail of Heartbeat {height}"))?;
        rows.push(ProjectionRow {
            detail_json: serde_json::to_string(&detail).context("serializing block detail")?,
            block_hash: hex(&block.hash()),
            parent_hash: block.header.parent.map(|hash| hex(&hash)),
            height,
            timestamp: block.header.timestamp,
            miner_public_key: hex(&block.header.miner_public_key),
            ledger_miner_public_key,
            heartbeat_reward,
            ledger_reward,
            target: hex(&block.header.target),
            ticket_count: block.tickets.len(),
            has_ledger_block: block.sidecar.is_some(),
            ledger_transaction_count,
            ticket_budget,
            heartbeat_bytes,
            chain_bytes,
            interval_seconds: previous_timestamp
                .map(|timestamp| block.header.timestamp.saturating_sub(timestamp)),
            minted: state.minted,
            utxos: state.utxos.len(),
            tracked_tickets: state.tickets.len(),
            ledger_blocks: state.ledger_blocks,
            active_tickets,
            ticket_weight,
        });
        previous_timestamp = Some(block.header.timestamp);
        previous_state = Some(state);
    }
    let tip = chain.tip();
    Ok(ProjectionBatch {
        common_height,
        rows,
        status: UiStatus {
            height: tip.height,
            tip_hash: hex(&tip.tip),
            ledger_blocks: tip.ledger_blocks,
            minted: tip.minted,
            tracked_tickets: tip.tickets.len(),
            utxos: tip.utxos.len(),
        },
    })
}

fn apply_projection(transaction: &Transaction<'_>, batch: &ProjectionBatch) -> Result<()> {
    if let Some(height) = batch.common_height {
        transaction.execute(
            "DELETE FROM consensus_ui_heartbeats WHERE height > ?1",
            [height],
        )?;
        transaction.execute(
            "DELETE FROM consensus_ui_block_details WHERE height > ?1",
            [height],
        )?;
    } else {
        transaction.execute("DELETE FROM consensus_ui_heartbeats", [])?;
        transaction.execute("DELETE FROM consensus_ui_block_details", [])?;
    }
    for row in &batch.rows {
        transaction
            .execute(
                "INSERT OR REPLACE INTO consensus_ui_block_details(height, block_hash, detail_json) \
                 VALUES(?1, ?2, ?3)",
                params![row.height, row.block_hash, row.detail_json],
            )
            .context("persisting block detail")?;
        transaction
            .execute(
                "INSERT INTO consensus_ui_heartbeats(height, block_hash, parent_hash, timestamp, \
                 miner_public_key, target, ticket_count, has_ledger_block, \
                 ledger_transaction_count, ticket_budget, heartbeat_bytes, chain_bytes, \
                 interval_seconds, minted, utxos, tracked_tickets, ledger_blocks, active_tickets, \
                 ticket_weight, ledger_miner_public_key, heartbeat_reward, ledger_reward) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
                params![
                    row.height,
                    row.block_hash,
                    row.parent_hash,
                    row.timestamp,
                    row.miner_public_key,
                    row.target,
                    row.ticket_count,
                    i64::from(row.has_ledger_block),
                    row.ledger_transaction_count,
                    row.ticket_budget,
                    row.heartbeat_bytes,
                    row.chain_bytes,
                    row.interval_seconds,
                    row.minted,
                    row.utxos,
                    row.tracked_tickets,
                    row.ledger_blocks,
                    row.active_tickets,
                    row.ticket_weight,
                    row.ledger_miner_public_key,
                    row.heartbeat_reward,
                    row.ledger_reward,
                ],
            )
            .with_context(|| format!("projecting Heartbeat {}", row.height))?;
    }
    let status = &batch.status;
    transaction.execute(
        "INSERT INTO consensus_ui_status(id, height, tip_hash, ledger_blocks, minted, tracked_tickets, utxos) \
         VALUES(1, ?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(id) DO UPDATE SET \
         height = excluded.height, tip_hash = excluded.tip_hash, \
         ledger_blocks = excluded.ledger_blocks, minted = excluded.minted, \
         tracked_tickets = excluded.tracked_tickets, utxos = excluded.utxos",
        params![
            status.height,
            status.tip_hash,
            status.ledger_blocks,
            status.minted,
            status.tracked_tickets,
            status.utxos,
        ],
    )?;
    Ok(())
}

fn projection_common_height(transaction: &Transaction<'_>, chain: &Chain) -> Result<Option<u64>> {
    let mut statement = transaction
        .prepare(
            "SELECT height, block_hash FROM consensus_ui_heartbeats \
             WHERE height <= ?1 ORDER BY height DESC",
        )
        .context("preparing projection ancestor query")?;
    let rows = statement
        .query_map([chain.tip().height], |row| {
            Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?))
        })
        .context("querying projection ancestor")?;
    for row in rows {
        let (height, stored_hash) = row.context("reading projection ancestor")?;
        if chain
            .canonical_block_at_height(height)
            .is_some_and(|block| hex(&block.hash()) == stored_hash)
        {
            return Ok(Some(height));
        }
    }
    Ok(None)
}

fn decode_heartbeat(row: &rusqlite::Row<'_>) -> rusqlite::Result<UiHeartbeat> {
    Ok(UiHeartbeat {
        height: row.get(0)?,
        block_hash: row.get(1)?,
        parent_hash: row.get(2)?,
        timestamp: row.get(3)?,
        miner_public_key: row.get(4)?,
        ledger_miner_public_key: row.get(19)?,
        target: row.get(5)?,
        ticket_count: row.get(6)?,
        has_ledger_block: row.get::<_, i64>(7)? != 0,
        ledger_transaction_count: row.get(8)?,
        ticket_budget: row.get(9)?,
        heartbeat_bytes: row.get(10)?,
        chain_bytes: row.get(11)?,
        interval_seconds: row.get(12)?,
        minted: row.get(13)?,
        utxos: row.get(14)?,
        tracked_tickets: row.get(15)?,
        ledger_blocks: row.get(16)?,
        active_tickets: row.get(17)?,
        ticket_weight: row.get(18)?,
    })
}

fn active_ticket_metrics(
    state: &crate::consensus::ConsensusState,
    height: u64,
) -> Result<(usize, u64)> {
    let mut count = 0_usize;
    let mut weight = 0_u64;
    for ticket in state.tickets.values() {
        if let Some(ticket_weight) = ticket.weight_at(height) {
            count = count
                .checked_add(1)
                .context("active ticket count overflow")?;
            weight = weight
                .checked_add(ticket_weight)
                .context("active ticket weight overflow")?;
        }
    }
    Ok((count, weight))
}

/// Returns whether any column was added.
fn migrate_schema(connection: &Connection) -> Result<bool> {
    let columns = connection
        .prepare("PRAGMA table_info(consensus_ui_heartbeats)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut added_columns = false;
    for (name, definition) in HEARTBEAT_METRIC_COLUMNS {
        if !columns.iter().any(|column| column == name) {
            connection.execute_batch(&format!(
                "ALTER TABLE consensus_ui_heartbeats ADD COLUMN {name} {definition}"
            ))?;
            added_columns = true;
        }
    }
    Ok(added_columns)
}

/// Rows from before a metric column hold its default, and projection never revisits a row whose
/// block hash still matches, so the rebuildable table starts over. Projected rows always have
/// `chain_bytes > 0`, so a zero repairs databases migrated before this check existed. That query
/// scans the table, so this runs once per open.
fn discard_stale_rows(connection: &Connection, added_columns: bool) -> Result<()> {
    let stale = added_columns
        || connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM consensus_ui_heartbeats WHERE chain_bytes = 0) \
             OR (EXISTS(SELECT 1 FROM consensus_ui_heartbeats) \
                 AND NOT EXISTS(SELECT 1 FROM consensus_ui_block_details))",
            [],
            |row| row.get::<_, bool>(0),
        )?;
    if stale {
        connection.execute("DELETE FROM consensus_ui_heartbeats", [])?;
    }
    Ok(())
}

fn hex(bytes: &Hash) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::{
        GENESIS_TIMESTAMP, HEARTBEAT_TARGET_INTERVAL_SECS, TrackedTicket, mine_child,
        pinned_genesis,
    };

    #[test]
    fn ticket_metrics_follow_active_slots_and_spread_budget_exactly() {
        let (_, mut state) = pinned_genesis().unwrap();
        state.tickets.insert(
            [8; 32],
            TrackedTicket {
                id: [8; 32],
                included_height: 10,
                budget: 10,
                duration: 3,
                mining_public_key: [9; 32],
                lottery_seed: [10; 32],
            },
        );

        assert_eq!(active_ticket_metrics(&state, 15).unwrap(), (0, 0));
        assert_eq!(active_ticket_metrics(&state, 16).unwrap(), (1, 4));
        assert_eq!(active_ticket_metrics(&state, 17).unwrap(), (1, 3));
        assert_eq!(active_ticket_metrics(&state, 18).unwrap(), (1, 3));
        assert_eq!(active_ticket_metrics(&state, 19).unwrap(), (0, 0));
    }

    #[test]
    fn a_previously_migrated_projection_with_zero_metrics_is_rebuilt() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ui.sqlite3");
        let store = ConsensusUiStore::open(&path).unwrap();
        store.project(&Chain::from_genesis().unwrap()).unwrap();
        drop(store);
        Connection::open(&path)
            .unwrap()
            .execute("UPDATE consensus_ui_heartbeats SET chain_bytes = 0", [])
            .unwrap();
        // The check scans the table, so ordinary reads must not run it.
        assert_eq!(
            ConsensusUiStore { path: path.clone() }
                .metrics(None)
                .unwrap()
                .len(),
            1
        );

        let store = ConsensusUiStore::open(&path).unwrap();
        assert!(store.metrics(None).unwrap().is_empty());
        store.project(&Chain::from_genesis().unwrap()).unwrap();
        assert!(store.metrics(None).unwrap()[0].chain_bytes > 0);
    }

    #[test]
    fn wallet_rewards_come_from_the_projection_within_the_window() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConsensusUiStore::open(directory.path().join("ui.sqlite3")).unwrap();
        let mut chain = Chain::from_genesis().unwrap();
        for (height, miner) in [(1, 92), (2, 93), (3, 92)] {
            let timestamp = GENESIS_TIMESTAMP + height * HEARTBEAT_TARGET_INTERVAL_SECS;
            let block = mine_child(chain.tip(), timestamp, [miner; 32], Vec::new(), None)
                .unwrap()
                .0;
            assert!(chain.submit(block, timestamp).unwrap());
        }
        store.project(&chain).unwrap();
        let blocks = chain.canonical_blocks();
        let miner = hex(&[92; 32]);

        let rewards = store.wallet_rewards(&miner, 10).unwrap();
        assert_eq!(rewards.len(), 2);
        assert!(rewards.contains_key(&hex(&blocks[1].hash())));
        assert!(rewards.contains_key(&hex(&blocks[3].hash())));
        assert!(rewards.values().all(|reward| reward.ledger == 0));

        let newest = store.wallet_rewards(&miner, 1).unwrap();
        assert_eq!(newest.len(), 1);
        assert!(newest.contains_key(&hex(&blocks[3].hash())));

        let older = store.wallet_rewards_before(&miner, Some(3), 2).unwrap();
        assert_eq!(older.len(), 1);
        assert!(older.contains_key(&hex(&blocks[1].hash())));
    }

    #[test]
    fn unlimited_metrics_are_thinned_and_keep_the_newest_heartbeat() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConsensusUiStore::open(directory.path().join("ui.sqlite3")).unwrap();
        let mut chain = Chain::from_genesis().unwrap();
        for height in 1..=4 {
            let timestamp = GENESIS_TIMESTAMP + height * HEARTBEAT_TARGET_INTERVAL_SECS;
            let block = mine_child(chain.tip(), timestamp, [91; 32], Vec::new(), None)
                .unwrap()
                .0;
            assert!(chain.submit(block, timestamp).unwrap());
        }
        store.project(&chain).unwrap();
        let heights =
            |rows: Vec<UiHeartbeat>| rows.iter().map(|row| row.height).collect::<Vec<_>>();

        assert_eq!(
            heights(store.metric_rows(None, 5).unwrap()),
            [4, 3, 2, 1, 0]
        );
        assert_eq!(heights(store.metric_rows(None, 2).unwrap()), [4, 3, 0]);
        assert_eq!(heights(store.metric_rows(Some(2), 2).unwrap()), [4, 3]);
    }

    #[test]
    fn projection_replaces_a_reorged_suffix_and_preserves_other_tables() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ui.sqlite3");
        let store = ConsensusUiStore::open(&path).unwrap();
        let mut chain = Chain::from_genesis().unwrap();
        let genesis = chain.tip().clone();
        let timestamp = GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS;
        let (left, _) = mine_child(&genesis, timestamp, [71; 32], Vec::new(), None).unwrap();
        let (right, right_state) =
            mine_child(&genesis, timestamp, [72; 32], Vec::new(), None).unwrap();
        assert!(chain.submit(left.clone(), timestamp).unwrap());
        store.project(&chain).unwrap();

        assert!(!chain.submit(right.clone(), timestamp).unwrap());
        let (right_child, _) = mine_child(
            &right_state,
            timestamp + HEARTBEAT_TARGET_INTERVAL_SECS,
            [73; 32],
            Vec::new(),
            None,
        )
        .unwrap();
        assert!(
            chain
                .submit(right_child, timestamp + HEARTBEAT_TARGET_INTERVAL_SECS)
                .unwrap()
        );
        Connection::open(&path)
            .unwrap()
            .execute("CREATE TABLE unrelated(value INTEGER)", [])
            .unwrap();
        store.project(&chain).unwrap();

        let rows = store.heartbeats_before(None, 10).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].block_hash, hex(&right.hash()));
        let older = store.heartbeats_before(Some(2), 10).unwrap();
        assert_eq!(
            older.iter().map(|row| row.height).collect::<Vec<_>>(),
            [1, 0]
        );
        let metrics = store.metrics(Some(2)).unwrap();
        assert_eq!(
            metrics.iter().map(|row| row.height).collect::<Vec<_>>(),
            [2, 1]
        );
        assert!(metrics[0].chain_bytes > metrics[1].chain_bytes);
        assert_eq!(
            metrics[0].interval_seconds,
            Some(HEARTBEAT_TARGET_INTERVAL_SECS)
        );
        assert!(metrics[0].heartbeat_bytes > 0);
        assert_eq!(metrics[0].minted, chain.tip().minted);
        assert_eq!(metrics[0].utxos, chain.tip().utxos.len());
        assert_eq!(store.status().unwrap().unwrap().height, 2);
        assert!(
            Connection::open(&path)
                .unwrap()
                .prepare("SELECT * FROM unrelated")
                .is_ok()
        );
    }

    #[test]
    fn projection_appends_without_rewriting_the_metric_prefix() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ui.sqlite3");
        let store = ConsensusUiStore::open(&path).unwrap();
        let mut chain = Chain::from_genesis().unwrap();
        let first_timestamp = GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS;
        let first = mine_child(chain.tip(), first_timestamp, [81; 32], Vec::new(), None)
            .unwrap()
            .0;
        assert!(chain.submit(first, first_timestamp).unwrap());
        store.project(&chain).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                r#"
CREATE TRIGGER protect_metric_prefix_update
BEFORE UPDATE ON consensus_ui_heartbeats WHEN OLD.height <= 1
BEGIN SELECT RAISE(FAIL, 'metric prefix was rewritten'); END;
CREATE TRIGGER protect_metric_prefix_delete
BEFORE DELETE ON consensus_ui_heartbeats WHEN OLD.height <= 1
BEGIN SELECT RAISE(FAIL, 'metric prefix was deleted'); END;
"#,
            )
            .unwrap();

        let second_timestamp = first_timestamp + HEARTBEAT_TARGET_INTERVAL_SECS;
        let second = mine_child(chain.tip(), second_timestamp, [82; 32], Vec::new(), None)
            .unwrap()
            .0;
        assert!(chain.submit(second, second_timestamp).unwrap());
        store.project(&chain).unwrap();

        assert_eq!(store.status().unwrap().unwrap().height, 2);
        assert_eq!(store.metrics(None).unwrap().len(), 3);
    }

    #[test]
    fn opening_an_old_projection_adds_metric_columns() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ui.sqlite3");
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE consensus_ui_heartbeats (
                    height INTEGER PRIMARY KEY,
                    block_hash TEXT NOT NULL UNIQUE,
                    parent_hash TEXT,
                    timestamp INTEGER NOT NULL,
                    miner_public_key TEXT NOT NULL,
                    target TEXT NOT NULL,
                    ticket_count INTEGER NOT NULL,
                    has_ledger_block INTEGER NOT NULL,
                    ledger_transaction_count INTEGER NOT NULL
                );
                INSERT INTO consensus_ui_heartbeats VALUES (0, 'old', NULL, 1, 'miner', 'ff', 0, 0, 0);",
            )
            .unwrap();

        let store = ConsensusUiStore::open(&path).unwrap();
        assert!(store.metrics(None).unwrap().is_empty());
        store.project(&Chain::from_genesis().unwrap()).unwrap();
        assert!(store.metrics(None).unwrap()[0].chain_bytes > 0);
        let connection = Connection::open(&path).unwrap();
        let mut statement = connection
            .prepare("PRAGMA table_info(consensus_ui_heartbeats)")
            .unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for (column, _) in HEARTBEAT_METRIC_COLUMNS {
            assert!(columns.iter().any(|existing| existing == column));
        }
    }

    #[test]
    fn rows_from_before_block_details_are_projected_again() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ui.sqlite3");
        let store = ConsensusUiStore::open(&path).unwrap();
        store.project(&Chain::from_genesis().unwrap()).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute("DELETE FROM consensus_ui_block_details", [])
            .unwrap();

        let reopened = ConsensusUiStore::open(&path).unwrap();
        assert!(reopened.heartbeats_before(None, 10).unwrap().is_empty());
    }

    #[test]
    fn clear_removes_only_consensus_projection_rows() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ui.sqlite3");
        let store = ConsensusUiStore::open(&path).unwrap();
        store.project(&Chain::from_genesis().unwrap()).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute("CREATE TABLE unrelated(value INTEGER)", [])
            .unwrap();

        store.clear().unwrap();

        assert!(store.status().unwrap().is_none());
        assert!(store.heartbeats_before(None, 10).unwrap().is_empty());
        assert!(
            Connection::open(&path)
                .unwrap()
                .prepare("SELECT * FROM unrelated")
                .is_ok()
        );
    }
}
