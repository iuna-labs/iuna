//! Durable canonical-chain storage for `iuna-mainnet-candidate`.
//!
//! This adapter stores consensus input (canonical blocks and identity metadata). Derived UI rows
//! deliberately belong in `ui_data_store`, and recent fork states remain owned by the consensus
//! domain. Loading always replays and validates blocks from the pinned genesis.
//!
//! The one exception is the checkpoint table, which keeps old post-states off the heap. It is a
//! cache of that replay: loading empties it and the replay fills it again, so a row never
//! outlives the process that derived it.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail, ensure};
use num_bigint::BigUint;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

use crate::app::consensus_node::ChainRepository;
use crate::consensus::{
    Chain, ConsensusState, GENESIS_HASH, Hash, Heartbeat, HeartbeatBlock,
    MAX_HEARTBEAT_BLOCK_BYTES, MAX_LEDGER_SIDECAR_BYTES, NETWORK_ID, OutPoint, Output,
    StateArchive, TrackedTicket,
};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS consensus_chain_blocks (
    height INTEGER PRIMARY KEY,
    block_hash BLOB NOT NULL UNIQUE CHECK(length(block_hash) = 32),
    block_json BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS consensus_chain_metadata (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    network_id TEXT NOT NULL,
    genesis_hash BLOB NOT NULL CHECK(length(genesis_hash) = 32),
    height INTEGER NOT NULL,
    tip_hash BLOB NOT NULL CHECK(length(tip_hash) = 32)
);
CREATE TABLE IF NOT EXISTS consensus_chain_checkpoints (
    block_hash BLOB PRIMARY KEY CHECK(length(block_hash) = 32),
    height INTEGER NOT NULL,
    state BLOB NOT NULL
);
"#;

// JSON has field names and decimal numbers in addition to canonical bytes. This is a defensive
// decoding limit, not a consensus block-size limit; canonical limits are checked during replay.
const MAX_STORED_BLOCK_JSON_BYTES: usize =
    4 * (MAX_HEARTBEAT_BLOCK_BYTES + MAX_LEDGER_SIDECAR_BYTES);

#[derive(Clone, Debug)]
pub struct ConsensusChainStore {
    path: PathBuf,
}

impl ConsensusChainStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let connection = Connection::open(&path)
            .with_context(|| format!("opening chain database {}", path.display()))?;
        connection
            .execute_batch(SCHEMA)
            .context("creating chain schema")?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn contains_chain(&self) -> Result<bool> {
        self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT 1 FROM consensus_chain_metadata WHERE id = 1",
                    [],
                    |_| Ok(()),
                )
                .optional()
                .map(|value| value.is_some())
                .context("checking chain metadata")
        })
    }

    pub fn save(&self, chain: &Chain) -> Result<()> {
        self.with_connection_mut(|connection| {
            let transaction = connection.transaction().context("starting chain save")?;
            save_chain(&transaction, chain)?;
            transaction.commit().context("committing chain save")
        })
    }

    /// Load and fully replay the canonical chain; persisted state itself is never trusted. These
    /// blocks already passed the future-timestamp bound, so a clock that went backwards must not
    /// stop the node from starting.
    pub fn load(&self) -> Result<Option<Chain>> {
        self.with_connection(|connection| load_chain(connection, self.new_chain()?))
    }

    fn new_chain(&self) -> Result<Chain> {
        Ok(Chain::from_genesis()
            .context("constructing pinned genesis")?
            .with_state_archive(Arc::new(self.clone())))
    }

    pub fn clear(&self) -> Result<()> {
        self.with_connection_mut(|connection| {
            let transaction = connection.transaction().context("starting chain clear")?;
            transaction
                .execute("DELETE FROM consensus_chain_blocks", [])
                .context("clearing blocks")?;
            transaction
                .execute("DELETE FROM consensus_chain_metadata", [])
                .context("clearing metadata")?;
            transaction
                .execute("DELETE FROM consensus_chain_checkpoints", [])
                .context("clearing checkpoints")?;
            transaction.commit().context("committing chain clear")
        })
    }

    fn put_checkpoint(&self, state: &ConsensusState) -> Result<()> {
        let bytes = encode_state(state);
        self.with_connection(|connection| {
            connection
                .execute(
                    "INSERT INTO consensus_chain_checkpoints(block_hash, height, state) \
                     VALUES(?1, ?2, ?3) ON CONFLICT(block_hash) DO UPDATE SET \
                     height = excluded.height, state = excluded.state",
                    params![state.tip.as_slice(), state.height, bytes],
                )
                .context("persisting checkpoint state")?;
            Ok(())
        })
    }

    fn get_checkpoint(&self, hash: &Hash) -> Result<Option<ConsensusState>> {
        let bytes = self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT state FROM consensus_chain_checkpoints WHERE block_hash = ?1",
                    [hash.as_slice()],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()
                .context("reading checkpoint state")
        })?;
        bytes.map(|bytes| decode_state(&bytes)).transpose()
    }

    fn retain_checkpoints(&self, keep: &dyn Fn(u64) -> bool) -> Result<()> {
        self.with_connection_mut(|connection| {
            let transaction = connection
                .transaction()
                .context("starting checkpoint trim")?;
            let heights = transaction
                .prepare("SELECT DISTINCT height FROM consensus_chain_checkpoints")?
                .query_map([], |row| row.get::<_, u64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("reading checkpoint heights")?;
            for height in heights.into_iter().filter(|height| !keep(*height)) {
                transaction.execute(
                    "DELETE FROM consensus_chain_checkpoints WHERE height = ?1",
                    [height],
                )?;
            }
            transaction.commit().context("committing checkpoint trim")
        })
    }

    fn with_connection<T>(&self, operation: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let connection = Connection::open(&self.path)
            .with_context(|| format!("opening chain database {}", self.path.display()))?;
        connection
            .execute_batch(SCHEMA)
            .context("ensuring chain schema")?;
        operation(&connection)
    }

    fn with_connection_mut<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T>,
    ) -> Result<T> {
        let mut connection = Connection::open(&self.path)
            .with_context(|| format!("opening chain database {}", self.path.display()))?;
        connection
            .execute_batch(SCHEMA)
            .context("ensuring chain schema")?;
        operation(&mut connection)
    }
}

impl ChainRepository for ConsensusChainStore {
    fn load(&self) -> Result<Option<Chain>> {
        ConsensusChainStore::load(self)
    }

    fn save(&self, chain: &Chain) -> Result<()> {
        ConsensusChainStore::save(self, chain)
    }

    fn new_chain(&self) -> Result<Chain> {
        ConsensusChainStore::new_chain(self)
    }
}

impl StateArchive for ConsensusChainStore {
    fn put(&self, state: &ConsensusState) {
        if let Err(error) = self.put_checkpoint(state) {
            eprintln!(
                "archiving checkpoint state {} failed: {error:#}",
                state.height
            );
        }
    }

    fn get(&self, hash: &Hash) -> Option<ConsensusState> {
        self.get_checkpoint(hash)
            .inspect_err(|error| eprintln!("reading checkpoint state failed: {error:#}"))
            .ok()
            .flatten()
    }

    fn retain(&self, keep: &dyn Fn(u64) -> bool) {
        if let Err(error) = self.retain_checkpoints(keep) {
            eprintln!("trimming checkpoint states failed: {error:#}");
        }
    }
}

fn save_chain(transaction: &Transaction<'_>, chain: &Chain) -> Result<()> {
    let genesis = chain
        .canonical_block_at_height(0)
        .context("chain has no genesis block")?;
    if genesis.hash() != GENESIS_HASH {
        bail!("refusing to persist a chain with an unknown genesis");
    }
    let common_height = stored_common_height(transaction, chain)?;
    if let Some(height) = common_height {
        transaction.execute(
            "DELETE FROM consensus_chain_blocks WHERE height > ?1",
            [height],
        )?;
    } else {
        transaction.execute("DELETE FROM consensus_chain_blocks", [])?;
    }
    for block in chain.canonical_blocks_after(common_height) {
        let block_json = serde_json::to_vec(block).context("serializing block")?;
        if block_json.len() > MAX_STORED_BLOCK_JSON_BYTES {
            bail!("serialized block exceeds storage decoding limit");
        }
        transaction
            .execute(
                "INSERT INTO consensus_chain_blocks(height, block_hash, block_json) VALUES(?1, ?2, ?3) \
                 ON CONFLICT(height) DO UPDATE SET block_hash = excluded.block_hash, \
                 block_json = excluded.block_json",
                params![block.header.height, block.hash().as_slice(), block_json],
            )
            .with_context(|| format!("persisting block {}", block.header.height))?;
    }
    let tip = chain.tip();
    transaction
        .execute(
            "INSERT INTO consensus_chain_metadata(id, network_id, genesis_hash, height, tip_hash) \
             VALUES(1, ?1, ?2, ?3, ?4) ON CONFLICT(id) DO UPDATE SET \
             network_id = excluded.network_id, genesis_hash = excluded.genesis_hash, \
             height = excluded.height, tip_hash = excluded.tip_hash",
            params![
                NETWORK_ID,
                GENESIS_HASH.as_slice(),
                tip.height,
                tip.tip.as_slice()
            ],
        )
        .context("persisting chain metadata")?;
    Ok(())
}

fn stored_common_height(transaction: &Transaction<'_>, chain: &Chain) -> Result<Option<u64>> {
    let mut statement = transaction
        .prepare(
            "SELECT height, block_hash FROM consensus_chain_blocks \
             WHERE height <= ?1 ORDER BY height DESC",
        )
        .context("preparing stored common-ancestor query")?;
    let rows = statement
        .query_map([chain.tip().height], |row| {
            Ok((row.get::<_, u64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .context("querying stored common ancestor")?;
    for row in rows {
        let (height, stored_hash) = row.context("reading stored common ancestor")?;
        if chain
            .canonical_block_at_height(height)
            .is_some_and(|block| stored_hash.as_slice() == block.hash())
        {
            return Ok(Some(height));
        }
    }
    Ok(None)
}

fn load_chain(connection: &Connection, mut chain: Chain) -> Result<Option<Chain>> {
    let metadata = connection
        .query_row(
            "SELECT network_id, genesis_hash, height, tip_hash FROM consensus_chain_metadata WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            },
        )
        .optional()
        .context("loading chain metadata")?;
    let Some((network, genesis_hash, stored_height, stored_tip)) = metadata else {
        return Ok(None);
    };
    if network != NETWORK_ID || genesis_hash.as_slice() != GENESIS_HASH {
        bail!("stored chain does not identify {NETWORK_ID}");
    }

    // The replay writes checkpoints through its own connection, which an open read would block.
    let blocks = read_blocks(connection)?;
    chain.expect_replay_to(stored_height);
    connection
        .execute("DELETE FROM consensus_chain_checkpoints", [])
        .context("clearing checkpoints")?;
    let mut count = 0u64;
    for block in blocks {
        let height = block.header.height;
        if height == 0 {
            if block.hash() != GENESIS_HASH {
                bail!("stored genesis does not match the pinned block");
            }
        } else {
            chain
                .submit(block, u64::MAX)
                .with_context(|| format!("validating stored block {height}"))?;
        }
        count = count.checked_add(1).context("block count overflow")?;
    }
    if count
        != stored_height
            .checked_add(1)
            .context("stored height overflow")?
        || chain.tip().height != stored_height
        || stored_tip.as_slice() != chain.tip().tip
    {
        bail!("stored metadata does not match replayed chain");
    }
    Ok(Some(chain))
}

/// Snapshots are fixed-width binary, because a full UTXO set in JSON would take 3.5 times the
/// space. The rows do not outlive the process, so the layout needs no version. A trailing SHA-256
/// covers what the header does not commit to, such as chainwork and timestamps.
fn encode_state(state: &ConsensusState) -> Vec<u8> {
    let mut out = Vec::with_capacity(256 + state.utxos.len() * 76 + state.tickets.len() * 120);
    let chainwork = state.chainwork.to_bytes_be();
    out.extend_from_slice(&state.height.to_le_bytes());
    out.extend_from_slice(&state.tip);
    out.extend_from_slice(&state.tip_timestamp.to_le_bytes());
    out.extend_from_slice(&state.tip_target);
    out.extend_from_slice(&state.genesis_timestamp.to_le_bytes());
    out.extend_from_slice(&state.ledger_parent_anchor);
    out.extend_from_slice(&state.ledger_blocks.to_le_bytes());
    out.extend_from_slice(&state.minted.to_le_bytes());
    out.extend_from_slice(&(chainwork.len() as u64).to_le_bytes());
    out.extend_from_slice(&chainwork);
    out.extend_from_slice(&(state.utxos.len() as u64).to_le_bytes());
    for (point, output) in &state.utxos {
        out.extend_from_slice(&point.transaction_id);
        out.extend_from_slice(&point.output_index.to_le_bytes());
        out.extend_from_slice(&output.owner);
        out.extend_from_slice(&output.value.to_le_bytes());
    }
    out.extend_from_slice(&(state.tickets.len() as u64).to_le_bytes());
    for ticket in state.tickets.values() {
        out.extend_from_slice(&ticket.id);
        out.extend_from_slice(&ticket.included_height.to_le_bytes());
        out.extend_from_slice(&ticket.budget.to_le_bytes());
        out.extend_from_slice(&ticket.duration.to_le_bytes());
        out.extend_from_slice(&ticket.mining_public_key);
        out.extend_from_slice(&ticket.lottery_seed);
    }
    out.extend_from_slice(&(state.heartbeats.len() as u64).to_le_bytes());
    for heartbeat in &state.heartbeats {
        out.extend_from_slice(&heartbeat.height.to_le_bytes());
        out.extend_from_slice(&heartbeat.hash);
        out.extend_from_slice(&heartbeat.parent_anchor);
    }
    out.extend_from_slice(&(state.recent_timestamps.len() as u64).to_le_bytes());
    for timestamp in &state.recent_timestamps {
        out.extend_from_slice(&timestamp.to_le_bytes());
    }
    let checksum = Sha256::digest(&out);
    out.extend_from_slice(&checksum);
    out
}

fn decode_state(bytes: &[u8]) -> Result<ConsensusState> {
    let body_length = bytes
        .len()
        .checked_sub(32)
        .context("checkpoint state is truncated")?;
    let (body, checksum) = bytes.split_at(body_length);
    ensure!(
        Sha256::digest(body).as_slice() == checksum,
        "checkpoint state fails its checksum"
    );
    let mut reader = SnapshotReader(body);
    let height = reader.u64()?;
    let tip = reader.array()?;
    let tip_timestamp = reader.u64()?;
    let tip_target = reader.array()?;
    let genesis_timestamp = reader.u64()?;
    let ledger_parent_anchor = reader.array()?;
    let ledger_blocks = reader.u64()?;
    let minted = reader.u64()?;
    let chainwork_length = reader.count(1)?;
    let chainwork = BigUint::from_bytes_be(reader.take(chainwork_length)?);
    let mut utxos = BTreeMap::new();
    for _ in 0..reader.count(76)? {
        let point = OutPoint {
            transaction_id: reader.array()?,
            output_index: u32::from_le_bytes(reader.array()?),
        };
        let output = Output {
            owner: reader.array()?,
            value: reader.u64()?,
        };
        utxos.insert(point, output);
    }
    let mut tickets = BTreeMap::new();
    for _ in 0..reader.count(120)? {
        let ticket = TrackedTicket {
            id: reader.array()?,
            included_height: reader.u64()?,
            budget: reader.u64()?,
            duration: reader.u64()?,
            mining_public_key: reader.array()?,
            lottery_seed: reader.array()?,
        };
        tickets.insert(ticket.id, ticket);
    }
    let mut heartbeats = Vec::new();
    for _ in 0..reader.count(72)? {
        heartbeats.push(Heartbeat {
            height: reader.u64()?,
            hash: reader.array()?,
            parent_anchor: reader.array()?,
        });
    }
    let mut recent_timestamps = Vec::new();
    for _ in 0..reader.count(8)? {
        recent_timestamps.push(reader.u64()?);
    }
    ensure!(reader.0.is_empty(), "checkpoint state has trailing bytes");
    Ok(ConsensusState {
        height,
        tip,
        tip_timestamp,
        tip_target,
        genesis_timestamp,
        utxos,
        tickets,
        heartbeats,
        ledger_parent_anchor,
        ledger_blocks,
        minted,
        chainwork,
        recent_timestamps,
    })
}

struct SnapshotReader<'a>(&'a [u8]);

impl<'a> SnapshotReader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        ensure!(length <= self.0.len(), "checkpoint state is truncated");
        let (taken, rest) = self.0.split_at(length);
        self.0 = rest;
        Ok(taken)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into()?)
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    /// A count of `item_size`-byte items, which must fit in what is left of the row.
    fn count(&mut self, item_size: usize) -> Result<usize> {
        let count = usize::try_from(self.u64()?)?;
        ensure!(
            count.saturating_mul(item_size) <= self.0.len(),
            "checkpoint state is truncated"
        );
        Ok(count)
    }
}

fn read_blocks(connection: &Connection) -> Result<Vec<HeartbeatBlock>> {
    let mut statement = connection
        .prepare(
            "SELECT height, block_hash, block_json FROM consensus_chain_blocks ORDER BY height ASC",
        )
        .context("preparing block replay")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .context("querying blocks")?;
    let mut blocks = Vec::new();
    for row in rows {
        let (height, stored_hash, block_json) = row.context("reading block row")?;
        if block_json.len() > MAX_STORED_BLOCK_JSON_BYTES {
            bail!("stored block {height} exceeds decoding limit");
        }
        let block: HeartbeatBlock = serde_json::from_slice(&block_json)
            .with_context(|| format!("decoding block {height}"))?;
        if block.header.height != height || stored_hash.as_slice() != block.hash() {
            bail!("stored block {height} has inconsistent identity");
        }
        blocks.push(block);
    }
    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::{GENESIS_TIMESTAMP, HEARTBEAT_TARGET_INTERVAL_SECS, mine_child};
    use tempfile::tempdir;

    fn two_block_chain(miner: u8) -> Chain {
        let mut chain = Chain::from_genesis().unwrap();
        let block = mine_child(
            chain.tip(),
            GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS,
            [miner; 32],
            Vec::new(),
            None,
        )
        .unwrap()
        .0;
        assert!(
            chain
                .submit(block, GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS,)
                .unwrap()
        );
        chain
    }

    fn append_block(chain: &mut Chain, miner: u8) {
        let timestamp =
            GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS * (chain.tip().height + 1);
        let block = mine_child(chain.tip(), timestamp, [miner; 32], Vec::new(), None)
            .unwrap()
            .0;
        assert!(chain.submit(block, timestamp).unwrap());
    }

    #[test]
    fn roundtrip_replays_consensus_instead_of_trusting_state() {
        let directory = tempdir().unwrap();
        let store = ConsensusChainStore::open(directory.path().join("chain.sqlite3")).unwrap();
        let chain = two_block_chain(21);
        store.save(&chain).unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.tip(), chain.tip());
        assert_eq!(loaded.canonical_blocks(), chain.canonical_blocks());
    }

    #[test]
    fn stored_blocks_load_whatever_the_clock_says() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConsensusChainStore::open(directory.path().join("chain.sqlite3")).unwrap();
        let mut chain = Chain::from_genesis().unwrap();
        // Accepted when the clock was ahead of where it is now.
        let timestamp = GENESIS_TIMESTAMP + 10 * 365 * 24 * 60 * 60;
        let block = mine_child(chain.tip(), timestamp, [26; 32], Vec::new(), None)
            .unwrap()
            .0;
        assert!(chain.submit(block, timestamp).unwrap());
        store.save(&chain).unwrap();

        assert_eq!(store.load().unwrap().unwrap().tip(), chain.tip());
    }

    #[test]
    fn save_replaces_an_abandoned_suffix_atomically() {
        let directory = tempdir().unwrap();
        let store = ConsensusChainStore::open(directory.path().join("chain.sqlite3")).unwrap();
        let left = two_block_chain(22);
        store.save(&left).unwrap();
        let right = two_block_chain(23);
        assert_ne!(left.tip().tip, right.tip().tip);
        store.save(&right).unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.tip().tip, right.tip().tip);
    }

    #[test]
    fn save_appends_without_rewriting_the_stored_prefix() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("chain.sqlite3");
        let store = ConsensusChainStore::open(&path).unwrap();
        let mut chain = two_block_chain(26);
        store.save(&chain).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                r#"
CREATE TRIGGER protect_chain_prefix_update
BEFORE UPDATE ON consensus_chain_blocks WHEN OLD.height <= 1
BEGIN SELECT RAISE(FAIL, 'stored chain prefix was rewritten'); END;
CREATE TRIGGER protect_chain_prefix_delete
BEFORE DELETE ON consensus_chain_blocks WHEN OLD.height <= 1
BEGIN SELECT RAISE(FAIL, 'stored chain prefix was deleted'); END;
"#,
            )
            .unwrap();

        append_block(&mut chain, 27);
        store.save(&chain).unwrap();

        assert_eq!(store.load().unwrap().unwrap().tip(), chain.tip());
    }

    fn checkpoint_count(path: &Path) -> u64 {
        Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM consensus_chain_checkpoints",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn populated_state(miner: u8) -> ConsensusState {
        let mut state = two_block_chain(miner).tip().clone();
        state.tickets.insert(
            [5; 32],
            TrackedTicket {
                id: [5; 32],
                included_height: 1,
                budget: 7,
                duration: 3,
                mining_public_key: [6; 32],
                lottery_seed: [7; 32],
            },
        );
        assert!(!state.utxos.is_empty() && !state.heartbeats.is_empty());
        state
    }

    #[test]
    fn checkpoint_states_survive_the_binary_roundtrip() {
        let directory = tempdir().unwrap();
        let store = ConsensusChainStore::open(directory.path().join("chain.sqlite3")).unwrap();
        let state = populated_state(28);

        store.put(&state);

        assert_eq!(store.get(&state.tip), Some(state));
        assert_eq!(store.get(&[9; 32]), None);
    }

    #[test]
    fn a_truncated_snapshot_is_refused_at_every_length() {
        let bytes = encode_state(&populated_state(31));
        for length in 0..bytes.len() {
            assert!(decode_state(&bytes[..length]).is_err(), "length {length}");
        }
        let mut flipped = bytes;
        flipped[60] ^= 1;
        assert!(decode_state(&flipped).is_err());
    }

    #[test]
    fn retain_removes_the_refused_heights_only() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("chain.sqlite3");
        let store = ConsensusChainStore::open(&path).unwrap();
        let mut chain = two_block_chain(32);
        store.put(chain.tip());
        append_block(&mut chain, 33);
        store.put(chain.tip());

        store.retain(&|height| height == 2);

        assert_eq!(checkpoint_count(&path), 1);
        assert!(store.get(&chain.tip().tip).is_some());
    }

    #[test]
    fn undecodable_checkpoints_count_as_missing() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("chain.sqlite3");
        let store = ConsensusChainStore::open(&path).unwrap();
        let chain = two_block_chain(29);
        store.put(chain.tip());
        Connection::open(&path)
            .unwrap()
            .execute("UPDATE consensus_chain_checkpoints SET state = X'FF'", [])
            .unwrap();

        assert_eq!(store.get(&chain.tip().tip), None);
    }

    #[test]
    fn a_new_chain_archives_checkpoints_and_loading_rederives_them() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("chain.sqlite3");
        let store = ConsensusChainStore::open(&path).unwrap();
        let retained = || store.new_chain().unwrap().with_state_retention(2, 4);
        let mut chain = retained();
        // The window trails the lower of the old and new tip, so height 4 leaves it at 8.
        for _ in 0..8 {
            append_block(&mut chain, 30);
        }
        store.save(&chain).unwrap();
        assert_eq!(checkpoint_count(&path), 1);

        let connection = Connection::open(&path).unwrap();
        connection
            .execute("UPDATE consensus_chain_checkpoints SET state = X'FF'", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO consensus_chain_checkpoints(block_hash, height, state) \
                 VALUES(?1, 4, X'FF')",
                [[8u8; 32].as_slice()],
            )
            .unwrap();

        let loaded = load_chain(&connection, retained()).unwrap().unwrap();

        assert_eq!(loaded.tip(), chain.tip());
        assert_eq!(checkpoint_count(&path), 1);
        let checkpoint = chain.canonical_block_at_height(4).unwrap();
        assert_eq!(
            store.get(&checkpoint.hash()).unwrap().tip,
            checkpoint.hash()
        );
    }

    #[test]
    fn corrupt_block_bytes_are_rejected_without_fallback() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("chain.sqlite3");
        let store = ConsensusChainStore::open(&path).unwrap();
        store.save(&two_block_chain(24)).unwrap();
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                "UPDATE consensus_chain_blocks SET block_json = X'FF' WHERE height = 1",
                [],
            )
            .unwrap();
        assert!(store.load().is_err());
    }

    #[test]
    fn clear_does_not_touch_unrelated_ui_tables() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("chain.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute("CREATE TABLE ui_marker(value INTEGER NOT NULL)", [])
            .unwrap();
        connection
            .execute("INSERT INTO ui_marker(value) VALUES(7)", [])
            .unwrap();
        drop(connection);
        let store = ConsensusChainStore::open(&path).unwrap();
        store.save(&two_block_chain(25)).unwrap();
        store.clear().unwrap();
        assert!(!store.contains_chain().unwrap());
        let connection = Connection::open(path).unwrap();
        let marker: u64 = connection
            .query_row("SELECT value FROM ui_marker", [], |row| row.get(0))
            .unwrap();
        assert_eq!(marker, 7);
    }
}
