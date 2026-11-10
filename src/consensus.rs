//! IUNA consensus.
//!
//! This module is deliberately self contained.  It is the consensus boundary used by the
//! node adapters: no wall clock, persistence, networking, UI or configuration types cross it.
//! All hashes use a length-prefixed canonical binary encoding and explicit domain separation.

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use num_bigint::BigUint;
use num_traits::{One, Zero};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type Amount = u64;
pub type Hash = [u8; 32];
pub type PublicKey = [u8; 32];
/// A domain-separated commitment to the Ed25519 public key that may spend an output.
/// The key itself is revealed only by the spending input.
pub type Address = [u8; 32];

pub const NETWORK_ID: &str = "iuna-mainnet-candidate";
pub const PROTOCOL_VERSION: u16 = 3;
pub const UNITS_PER_IUNA: Amount = 100_000_000;
pub const MAX_MONEY: Amount = 4_207_680_100_000_000;
pub const GENESIS_REWARD: Amount = UNITS_PER_IUNA;
pub const INITIAL_LEDGER_SUBSIDY: Amount = 50 * UNITS_PER_IUNA;
pub const LEDGER_HALVING_INTERVAL: u64 = 420_768;
pub const HEARTBEAT_TARGET_INTERVAL_SECS: u64 = 60;
/// 60 Heartbeats (one hour): after a tenfold hashrate jump the target is within a factor two in
/// under an hour, while per-block luck still moves it by only about one percent.
pub const ASERT_HALF_LIFE_SECS: i64 = 60 * HEARTBEAT_TARGET_INTERVAL_SECS as i64;
pub const MEDIAN_TIME_WINDOW: usize = 11;
/// Ten Heartbeat intervals of tolerated clock skew; manipulating it shifts one target by at most
/// 2^(600 / half-life), about 12%, which the following solvetime then reverses.
pub const MAX_FUTURE_TIME_SECS: u64 = 10 * 60;
pub const MAX_HEARTBEAT_BLOCK_BYTES: usize = 16 * 1024;
pub const MAX_LEDGER_SIDECAR_BYTES: usize = 512 * 1024;
pub const MAX_TRACKED_TICKETS: usize = 4_096;
pub const MAX_TICKET_DURATION: u64 = 1_440;
pub const LEDGER_TARGET_INTERVAL_HEARTBEATS: u64 = 10;
pub const TICKET_CONFIRMATIONS: u64 = 6;
pub const WINNER_WINDOW: u64 = 3;
pub const MIN_TICKET_BUDGET: Amount = 1_000_000;
pub const LEDGER_GENESIS_ANCHOR: Hash = *b"IUNA-LEDGER-GENESIS-ANCHOR-v1...";
pub const GENESIS_TIMESTAMP: u64 = 1_794_077_841;
/// RFC 8032 test-vector public key. Its secret is public, so anyone can spend the genesis output.
/// The candidate relies on that to fund the first ticket; a real mainnet needs a controlled key.
pub const GENESIS_MINER: PublicKey = [
    0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07, 0x3a,
    0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07, 0x51, 0x1a,
];
/// Proof-of-work limit and genesis target, like Bitcoin's `powLimit`/difficulty 1: the target can
/// never be easier, and ASERT raises difficulty from here. 26 leading zero bits (~2^26 hashes) is
/// about one 60-second Heartbeat for a single ~1 MH/s CPU thread; with less hashrate Heartbeats
/// are simply slower. Unit tests use a regtest-like 12-bit limit so they can mine many blocks.
#[cfg(not(any(test, feature = "e2e")))]
pub const MAX_TARGET: Hash = [
    0x00, 0x00, 0x00, 0x3f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
];
#[cfg(any(test, feature = "e2e"))]
pub const MAX_TARGET: Hash = [
    0x00, 0x0f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
];
/// Pinned like Bitcoin's genesis block, so nodes never mine it at startup. Re-pin with
/// [`search_genesis_nonce`] whenever a genesis field or [`MAX_TARGET`] changes.
#[cfg(not(any(test, feature = "e2e")))]
pub const GENESIS_NONCE: u64 = 44_762_545;
#[cfg(not(any(test, feature = "e2e")))]
pub const GENESIS_HASH: Hash = [
    0x00, 0x00, 0x00, 0x15, 0xff, 0x94, 0x62, 0x4a, 0x51, 0xc0, 0x5e, 0x7b, 0xf5, 0x3e, 0x0b, 0x83,
    0x7b, 0xaf, 0x10, 0x5d, 0x9c, 0xf2, 0x8d, 0x0a, 0xf4, 0x99, 0x55, 0xf1, 0xa8, 0xb2, 0x4f, 0xdb,
];
#[cfg(any(test, feature = "e2e"))]
pub const GENESIS_NONCE: u64 = 2_400;
#[cfg(any(test, feature = "e2e"))]
pub const GENESIS_HASH: Hash = [
    0x00, 0x09, 0x1d, 0x92, 0x9c, 0x05, 0x00, 0x40, 0x33, 0x7c, 0xf7, 0x2e, 0x17, 0xce, 0x02, 0x70,
    0xff, 0xd2, 0xeb, 0x2a, 0xf5, 0x70, 0xf6, 0xc4, 0x10, 0x5a, 0xb3, 0x5f, 0x72, 0xa4, 0x2e, 0x6f,
];

const ZERO_HASH: Hash = [0; 32];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConsensusError {
    Invalid(&'static str),
    Arithmetic,
}

impl std::fmt::Display for ConsensusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "{message}"),
            Self::Arithmetic => f.write_str("consensus arithmetic overflow or underflow"),
        }
    }
}

impl std::error::Error for ConsensusError {}

type Result<T> = std::result::Result<T, ConsensusError>;

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct OutPoint {
    pub transaction_id: Hash,
    pub output_index: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Output {
    pub owner: Address,
    pub value: Amount,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Input {
    pub previous_output: OutPoint,
    pub public_key: PublicKey,
    /// Canonical Ed25519 signature bytes. A vector is used because serde does not encode a
    /// portable fixed array larger than 32 bytes; validation requires exactly 64 bytes.
    pub signature: Vec<u8>,
}

/// The network a signature is valid on, so it cannot be replayed on a fork or relaunch that shares
/// the same outpoints.
#[derive(Clone, Copy)]
struct SigningScope<'a> {
    network_id: &'a str,
    genesis_hash: &'a Hash,
}

impl SigningScope<'static> {
    const NETWORK: Self = Self {
        network_id: NETWORK_ID,
        genesis_hash: &GENESIS_HASH,
    };
}

/// Input values are signed so a wallet that was told a wrong value by its node produces an invalid
/// signature, instead of silently paying the difference as a fee.
fn signing_digest(
    domain: &[u8],
    scope: SigningScope<'_>,
    unsigned: &[u8],
    input_values: &[Amount],
    input_index: usize,
) -> Result<Hash> {
    let mut out = Vec::new();
    put_bytes(&mut out, scope.network_id.as_bytes());
    out.extend_from_slice(scope.genesis_hash);
    out.extend_from_slice(unsigned);
    for value in input_values {
        put_u64(&mut out, *value);
    }
    put_u32(
        &mut out,
        u32::try_from(input_index).map_err(|_| ConsensusError::Arithmetic)?,
    );
    Ok(tagged_hash(domain, &out))
}

/// The values of `points` in `utxos`, in order, as signing digests need them.
pub fn input_values<'a>(
    utxos: &BTreeMap<OutPoint, Output>,
    points: impl IntoIterator<Item = &'a OutPoint>,
    missing: &'static str,
) -> Result<Vec<Amount>> {
    points
        .into_iter()
        .map(|point| {
            utxos
                .get(point)
                .map(|output| output.value)
                .ok_or(ConsensusError::Invalid(missing))
        })
        .collect()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Transaction {
    pub version: u16,
    pub inputs: Vec<Input>,
    pub outputs: Vec<Output>,
}

impl Transaction {
    pub fn id(&self) -> Hash {
        tagged_hash(b"IUNA-TX-v1", &self.canonical(true))
    }

    /// `input_values` holds the value of each spent output, in input order.
    pub fn signing_digest(&self, input_index: usize, input_values: &[Amount]) -> Result<Hash> {
        if input_index >= self.inputs.len() || input_values.len() != self.inputs.len() {
            return Err(ConsensusError::Invalid(
                "transaction signing input does not match its inputs",
            ));
        }
        signing_digest(
            b"IUNA-TX-SIGN-v1",
            SigningScope::NETWORK,
            &self.canonical(false),
            input_values,
            input_index,
        )
    }

    pub fn canonical_size(&self) -> usize {
        self.canonical(true).len()
    }

    fn canonical(&self, include_signatures: bool) -> Vec<u8> {
        let mut out = Vec::new();
        put_u16(&mut out, self.version);
        put_u32(&mut out, self.inputs.len() as u32);
        for input in &self.inputs {
            put_outpoint(&mut out, &input.previous_output);
            out.extend_from_slice(&input.public_key);
            if include_signatures {
                put_bytes(&mut out, &input.signature);
            }
        }
        put_u32(&mut out, self.outputs.len() as u32);
        for output in &self.outputs {
            put_output(&mut out, output);
        }
        out
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TicketInput {
    pub previous_output: OutPoint,
    pub public_key: PublicKey,
    pub signature: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TicketTransaction {
    pub version: u16,
    pub inputs: Vec<TicketInput>,
    pub budget: Amount,
    pub duration: u64,
    pub mining_public_key: PublicKey,
    pub lottery_seed: Hash,
    pub change: Option<Output>,
}

impl TicketTransaction {
    pub fn id(&self) -> Hash {
        tagged_hash(b"IUNA-TICKET-v1", &self.canonical(true))
    }

    /// `input_values` holds the value of each spent output, in input order.
    pub fn signing_digest(&self, input_index: usize, input_values: &[Amount]) -> Result<Hash> {
        if input_index >= self.inputs.len() || input_values.len() != self.inputs.len() {
            return Err(ConsensusError::Invalid(
                "ticket signing input does not match its inputs",
            ));
        }
        signing_digest(
            b"IUNA-TICKET-SIGN-v1",
            SigningScope::NETWORK,
            &self.canonical(false),
            input_values,
            input_index,
        )
    }

    pub fn canonical_size(&self) -> usize {
        self.canonical(true).len()
    }

    fn canonical(&self, include_signatures: bool) -> Vec<u8> {
        let mut out = Vec::new();
        put_u16(&mut out, self.version);
        put_u32(&mut out, self.inputs.len() as u32);
        for input in &self.inputs {
            put_outpoint(&mut out, &input.previous_output);
            out.extend_from_slice(&input.public_key);
            if include_signatures {
                put_bytes(&mut out, &input.signature);
            }
        }
        put_u64(&mut out, self.budget);
        put_u64(&mut out, self.duration);
        out.extend_from_slice(&self.mining_public_key);
        out.extend_from_slice(&self.lottery_seed);
        match &self.change {
            Some(change) => {
                out.push(1);
                put_output(&mut out, change);
            }
            None => out.push(0),
        }
        out
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CandidateHeader {
    pub version: u16,
    pub lottery_heartbeat: Hash,
    pub lottery_height: u64,
    pub parent_anchor: Hash,
    pub ticket_id: Hash,
    pub slot: u64,
    pub lottery_output: Hash,
    pub transactions_root: Hash,
    /// Ed25519 signature by the winning ticket's mining key. The canonical deadline fallback is
    /// deliberately unsigned; every non-empty candidate must carry exactly one valid signature.
    pub signature: Vec<u8>,
}

impl CandidateHeader {
    pub fn hash(&self) -> Hash {
        tagged_hash(b"IUNA-LEDGER-CANDIDATE-v2", &self.canonical(true))
    }

    pub fn signing_digest(&self) -> Hash {
        let mut out = Vec::new();
        put_bytes(&mut out, NETWORK_ID.as_bytes());
        out.extend_from_slice(&GENESIS_HASH);
        out.extend_from_slice(&self.canonical(false));
        tagged_hash(b"IUNA-LEDGER-CANDIDATE-SIGN-v1", &out)
    }

    fn canonical(&self, include_signature: bool) -> Vec<u8> {
        let mut out = Vec::new();
        put_u16(&mut out, self.version);
        out.extend_from_slice(&self.lottery_heartbeat);
        put_u64(&mut out, self.lottery_height);
        out.extend_from_slice(&self.parent_anchor);
        out.extend_from_slice(&self.ticket_id);
        put_u64(&mut out, self.slot);
        out.extend_from_slice(&self.lottery_output);
        out.extend_from_slice(&self.transactions_root);
        if include_signature {
            put_bytes(&mut out, &self.signature);
        }
        out
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Sidecar {
    pub header: CandidateHeader,
    pub transactions: Vec<Transaction>,
}

impl Sidecar {
    pub fn canonical_size(&self) -> usize {
        self.canonical().len()
    }

    fn canonical(&self) -> Vec<u8> {
        let mut out = self.header.canonical(true);
        put_u32(&mut out, self.transactions.len() as u32);
        for transaction in &self.transactions {
            put_bytes(&mut out, &transaction.canonical(true));
        }
        out
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HeartbeatHeader {
    pub protocol_version: u16,
    pub network: String,
    pub height: u64,
    pub parent: Option<Hash>,
    pub timestamp: u64,
    pub target: Hash,
    pub nonce: u64,
    pub miner_public_key: PublicKey,
    pub tickets_root: Hash,
    pub candidate_hash: Option<Hash>,
    pub post_state_root: Hash,
    pub payout_commitment: Hash,
}

impl HeartbeatHeader {
    pub fn hash(&self) -> Hash {
        tagged_hash(b"IUNA-HEARTBEAT-v1", &self.canonical())
    }

    fn canonical(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_u16(&mut out, self.protocol_version);
        put_bytes(&mut out, self.network.as_bytes());
        put_u64(&mut out, self.height);
        match self.parent {
            Some(parent) => {
                out.push(1);
                out.extend_from_slice(&parent);
            }
            None => out.push(0),
        }
        put_u64(&mut out, self.timestamp);
        out.extend_from_slice(&self.target);
        put_u64(&mut out, self.nonce);
        out.extend_from_slice(&self.miner_public_key);
        out.extend_from_slice(&self.tickets_root);
        match self.candidate_hash {
            Some(hash) => {
                out.push(1);
                out.extend_from_slice(&hash);
            }
            None => out.push(0),
        }
        out.extend_from_slice(&self.post_state_root);
        out.extend_from_slice(&self.payout_commitment);
        out
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HeartbeatBlock {
    pub header: HeartbeatHeader,
    pub tickets: Vec<TicketTransaction>,
    pub sidecar: Option<Sidecar>,
}

impl HeartbeatBlock {
    pub fn hash(&self) -> Hash {
        self.header.hash()
    }

    pub fn canonical_size(&self) -> usize {
        let mut bytes = self.header.canonical();
        put_u32(&mut bytes, self.tickets.len() as u32);
        for ticket in &self.tickets {
            put_bytes(&mut bytes, &ticket.canonical(true));
        }
        bytes.len()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrackedTicket {
    pub id: Hash,
    pub included_height: u64,
    pub budget: Amount,
    pub duration: u64,
    pub mining_public_key: PublicKey,
    pub lottery_seed: Hash,
}

impl TrackedTicket {
    pub fn lifecycle_length(&self) -> u64 {
        ticket_lifecycle_length(self.duration)
    }

    pub fn payout_at(&self, offset: u64) -> Result<Amount> {
        if offset >= self.lifecycle_length() {
            return Err(ConsensusError::Invalid("ticket payout offset out of range"));
        }
        let length = self.lifecycle_length();
        let quotient = self.budget / length;
        let remainder = self.budget % length;
        Ok(quotient + u64::from(offset < remainder))
    }

    pub fn remaining_after(&self, processed: u64) -> Result<Amount> {
        if processed > self.lifecycle_length() {
            return Err(ConsensusError::Invalid(
                "ticket processed count out of range",
            ));
        }
        let quotient = self.budget / self.lifecycle_length();
        let remainder = self.budget % self.lifecycle_length();
        let paid = checked_add(checked_mul(quotient, processed)?, processed.min(remainder))?;
        self.budget
            .checked_sub(paid)
            .ok_or(ConsensusError::Arithmetic)
    }

    pub fn slot_at(&self, height: u64) -> Option<u64> {
        let first = self.included_height.checked_add(TICKET_CONFIRMATIONS)?;
        let slot = height.checked_sub(first)?;
        (slot < self.duration).then_some(slot)
    }

    pub fn weight_at(&self, height: u64) -> Option<Amount> {
        let slot = self.slot_at(height)?;
        let quotient = self.budget / self.duration;
        let remainder = self.budget % self.duration;
        Some(quotient + u64::from(slot < remainder))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Heartbeat {
    pub height: u64,
    pub hash: Hash,
    pub parent_anchor: Hash,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConsensusState {
    pub height: u64,
    pub tip: Hash,
    pub tip_timestamp: u64,
    pub tip_target: Hash,
    pub genesis_timestamp: u64,
    pub utxos: BTreeMap<OutPoint, Output>,
    pub tickets: BTreeMap<Hash, TrackedTicket>,
    pub heartbeats: Vec<Heartbeat>,
    pub ledger_parent_anchor: Hash,
    pub ledger_blocks: u64,
    pub minted: Amount,
    pub chainwork: BigUint,
    pub recent_timestamps: Vec<u64>,
}

/// Full post-states kept below the active tip. Older ones are rebuilt on demand.
const STATE_WINDOW: u64 = 256;
/// Closest spacing of the older states kept as replay starting points. See `checkpoint_kept`.
const STATE_CHECKPOINT_INTERVAL: u64 = 256;

/// Memory for fork blocks waiting to prove their work. A heavier branch that needs more than this
/// cannot activate, but the limit has to hold against blocks that cost next to nothing to mine.
const MAX_DEFERRED_BYTES: usize = 256 * 1024 * 1024;
/// Hashes of heavy branches that failed validation, which must not earn a second replay for free.
const MAX_REJECTED_BLOCKS: usize = 1_024;

/// Whether the checkpoint at `height` is still kept below `tip`. The allowed spacing doubles each
/// time a checkpoint's age does, so checkpoints sit at 1/2, 3/4, 7/8 of the chain and their count
/// grows with log2 of its height. They never move: as the chain grows, every other one is dropped.
/// A rebuild therefore costs about as many blocks as its distance to the tip, which is the same
/// order as the reorg that asks for it. `interval` must be a power of two.
fn checkpoint_kept(height: u64, tip: u64, interval: u64) -> bool {
    let age = tip.saturating_sub(height);
    let spacing = age.checked_ilog2().map_or(1, |exponent| 1 << exponent);
    height.is_multiple_of(spacing.max(interval))
}

/// Where `Chain` puts checkpoint states once they leave memory. Implementations report their own
/// failures: a lost checkpoint only means a longer replay from the one before it.
pub trait StateArchive: std::fmt::Debug + Send + Sync {
    fn put(&self, state: &ConsensusState);
    fn get(&self, hash: &Hash) -> Option<ConsensusState>;
    /// Drop every checkpoint whose height `keep` refuses.
    fn retain(&self, keep: &dyn Fn(u64) -> bool);
}

/// A block on a fork whose parent state is not at hand. Rebuilding that state is the expensive
/// part of validation, so it waits until the branch has more work than the active one.
#[derive(Clone, Debug)]
struct DeferredBlock {
    block: HeartbeatBlock,
    chainwork: BigUint,
}

/// In-memory chain index. Persistence and P2P are adapters around this type. Blocks are kept for
/// the whole chain. Post-states stay in memory for recent blocks only, because each one is a full
/// UTXO set. Older checkpoints move to the archive, or stay here without one. Every other state
/// is a deterministic replay away, so a higher-work branch from any depth still switches
/// Heartbeat, Ledger, tickets, payouts and UTXOs atomically.
#[derive(Clone, Debug)]
pub struct Chain {
    blocks: BTreeMap<Hash, HeartbeatBlock>,
    states: BTreeMap<Hash, ConsensusState>,
    active_tip: Hash,
    state_window: u64,
    state_checkpoint_interval: u64,
    archive: Option<Arc<dyn StateArchive>>,
    replay_height: u64,
    chainwork: BTreeMap<Hash, BigUint>,
    deferred: BTreeMap<Hash, DeferredBlock>,
    deferred_bytes: usize,
    rejected: BTreeSet<Hash>,
}

impl Chain {
    pub fn from_genesis() -> Result<Self> {
        let (genesis, state) = pinned_genesis()?;
        Ok(Self {
            chainwork: BTreeMap::from([(state.tip, state.chainwork.clone())]),
            blocks: BTreeMap::from([(genesis.hash(), genesis)]),
            states: BTreeMap::from([(state.tip, state)]),
            active_tip: GENESIS_HASH,
            state_window: STATE_WINDOW,
            state_checkpoint_interval: STATE_CHECKPOINT_INTERVAL,
            archive: None,
            replay_height: 0,
            deferred: BTreeMap::new(),
            deferred_bytes: 0,
            rejected: BTreeSet::new(),
        })
    }

    pub fn with_state_archive(mut self, archive: Arc<dyn StateArchive>) -> Self {
        self.archive = Some(archive);
        self
    }

    /// Announce the height a replay will reach, so it skips checkpoints that would not survive it.
    pub fn expect_replay_to(&mut self, height: u64) {
        self.replay_height = height;
    }

    #[cfg(test)]
    pub(crate) fn with_state_retention(mut self, window: u64, checkpoint_interval: u64) -> Self {
        self.state_window = window;
        self.state_checkpoint_interval = checkpoint_interval;
        self
    }

    pub fn tip(&self) -> &ConsensusState {
        self.states
            .get(&self.active_tip)
            .expect("active tip is always indexed")
    }

    /// The post-state of an indexed block, rebuilt from the nearest kept ancestor when needed.
    pub fn state(&self, hash: &Hash) -> Option<Cow<'_, ConsensusState>> {
        let mut missing = Vec::new();
        let mut cursor = *hash;
        let kept = loop {
            if let Some(state) = self.states.get(&cursor) {
                break Cow::Borrowed(state);
            }
            let block = self.blocks.get(&cursor)?;
            if let Some(state) = self.archived_state(block) {
                break Cow::Owned(state);
            }
            missing.push(block);
            cursor = block.header.parent?;
        };
        let mut state = kept;
        for block in missing.into_iter().rev() {
            state = self.child_state(&state, block)?;
        }
        Some(state)
    }

    /// A row that disagrees with its header is passed over like a missing one. Genesis always
    /// stays in memory, which leaves replay a starting point.
    fn archived_state(&self, block: &HeartbeatBlock) -> Option<ConsensusState> {
        let tip = self.tip().height;
        if !checkpoint_kept(block.header.height, tip, self.state_checkpoint_interval) {
            return None;
        }
        let hash = block.hash();
        let state = self.archive.as_ref()?.get(&hash)?;
        (state.tip == hash
            && state.height == block.header.height
            && state.state_root() == block.header.post_state_root)
            .then_some(state)
    }

    /// A state that is at hand without replay, for checks that must stay cheap for strangers.
    pub fn kept_state(&self, hash: &Hash) -> Option<&ConsensusState> {
        self.states.get(hash)
    }

    /// The post-state of `block` from its parent's state. Walking a range this way replays each
    /// block once, where calling `state` per block could replay a whole checkpoint interval.
    pub fn child_state(
        &self,
        parent: &ConsensusState,
        block: &HeartbeatBlock,
    ) -> Option<Cow<'_, ConsensusState>> {
        match self.states.get(&block.hash()) {
            Some(state) => Some(Cow::Borrowed(state)),
            None => apply_block(parent, block, u64::MAX).ok().map(Cow::Owned),
        }
    }

    pub fn block(&self, hash: &Hash) -> Option<&HeartbeatBlock> {
        self.blocks.get(hash)
    }

    /// Validate and index a block. Returns true only when strict additional chainwork makes this
    /// branch active; equal-work branches remain available but never replace the current tip.
    /// A block whose parent state would need a replay is held back until its branch outweighs the
    /// active one, so the cost of a deep replay falls on whoever asks for it.
    pub fn submit(&mut self, block: HeartbeatBlock, now: u64) -> Result<bool> {
        let parent_hash = block
            .header
            .parent
            .ok_or(ConsensusError::Invalid("cannot submit a second genesis"))?;
        let hash = block.hash();
        if self.blocks.contains_key(&hash) || self.deferred.contains_key(&hash) {
            return Ok(false);
        }
        if self.rejected.contains(&hash) {
            return Err(ConsensusError::Invalid("block was already rejected"));
        }
        // Headers are enough for these checks, so a peer cannot get past them on a self-chosen
        // target or without any work at all.
        check_proof_of_work(&block.header)?;
        if block.header.target != self.child_target(&parent_hash)? {
            return Err(ConsensusError::Invalid("incorrect ASERT target"));
        }
        if self.states.contains_key(&parent_hash) {
            self.index_block(block, now)
        } else {
            self.check_deferrable(&block, now)?;
            self.defer_block(block)
        }
    }

    /// What needs no state. After this the hash pins the whole body, so the copy kept is the only
    /// one there is, and a block that fails later will fail for everyone. A block ahead of the
    /// clock stops here, before it can be remembered as invalid.
    fn check_deferrable(&self, block: &HeartbeatBlock, now: u64) -> Result<()> {
        let header = &block.header;
        let parent = header
            .parent
            .and_then(|hash| self.known_header(&hash))
            .ok_or(ConsensusError::Invalid("unknown block parent"))?;
        if header.protocol_version != PROTOCOL_VERSION || header.network != NETWORK_ID {
            return Err(ConsensusError::Invalid("wrong protocol or network"));
        }
        if header.height != next_height(parent.height)? {
            return Err(ConsensusError::Invalid("block is not a direct child"));
        }
        if header.timestamp > now.saturating_add(MAX_FUTURE_TIME_SECS) {
            return Err(ConsensusError::Invalid(
                "timestamp outside consensus bounds",
            ));
        }
        check_body_commitments(block)
    }

    fn defer_block(&mut self, block: HeartbeatBlock) -> Result<bool> {
        let hash = block.hash();
        let parent = block
            .header
            .parent
            .ok_or(ConsensusError::Invalid("cannot submit a second genesis"))?;
        let parent_work = self
            .deferred
            .get(&parent)
            .map(|parent| &parent.chainwork)
            .or_else(|| self.chainwork.get(&parent))
            .ok_or(ConsensusError::Invalid("unknown block parent"))?;
        let chainwork = parent_work + block_work(&block.header.target)?;
        let heavier = chainwork > self.tip().chainwork;
        self.deferred_bytes += deferred_size(&block);
        self.deferred
            .insert(hash, DeferredBlock { block, chainwork });
        if heavier {
            return self.activate_deferred(hash);
        }
        if self.deferred_bytes > MAX_DEFERRED_BYTES {
            // An honest branch comes back through sync; a flood has nothing left to build on.
            self.deferred.clear();
            self.deferred_bytes = 0;
        }
        Ok(false)
    }

    fn activate_deferred(&mut self, tip: Hash) -> Result<bool> {
        let mut branch = Vec::new();
        let mut cursor = Some(tip);
        while let Some(deferred) = cursor.and_then(|hash| self.deferred.remove(&hash)) {
            self.deferred_bytes -= deferred_size(&deferred.block);
            cursor = deferred.block.header.parent;
            branch.push(deferred.block);
        }
        let branch_tip = branch.first().map_or(0, |block| block.header.height);
        let mut activated = false;
        for block in branch.into_iter().rev() {
            let hash = block.hash();
            // The clock was checked on arrival. Looking again would let a step back in time count
            // as invalid, which `rejected` would then remember.
            match self.index_block(block, u64::MAX) {
                Ok(true) => activated = true,
                // Without this a deep branch would hold every full state until it activates. The
                // window follows the branch, which may be about to become the tip.
                Ok(false) => self.prune_states(self.tip().height.min(branch_tip), &hash),
                Err(error) => {
                    if self.rejected.len() == MAX_REJECTED_BLOCKS {
                        self.rejected.clear();
                    }
                    self.rejected.insert(hash);
                    // Whatever else waits here may build on the block that just failed.
                    self.deferred.clear();
                    self.deferred_bytes = 0;
                    // An activated block must still reach the repository.
                    return if activated { Ok(true) } else { Err(error) };
                }
            }
        }
        Ok(activated)
    }

    fn index_block(&mut self, block: HeartbeatBlock, now: u64) -> Result<bool> {
        let hash = block.hash();
        let parent_hash = block
            .header
            .parent
            .ok_or(ConsensusError::Invalid("cannot submit a second genesis"))?;
        let parent = self
            .state(&parent_hash)
            .ok_or(ConsensusError::Invalid("unknown block parent"))?;
        let state = apply_block(&parent, &block, now)?;
        drop(parent);
        let previous = self.tip();
        let activates = state.chainwork > previous.chainwork;
        let lowest_tip = previous.height.min(state.height);
        self.blocks.insert(hash, block);
        self.chainwork.insert(hash, state.chainwork.clone());
        self.states.insert(hash, state);
        if activates {
            self.active_tip = hash;
            self.prune_states(lowest_tip, &hash);
        }
        Ok(activates)
    }

    fn known_header(&self, hash: &Hash) -> Option<&HeartbeatHeader> {
        self.blocks
            .get(hash)
            .or_else(|| self.deferred.get(hash).map(|deferred| &deferred.block))
            .map(|block| &block.header)
    }

    fn child_target(&self, parent_hash: &Hash) -> Result<Hash> {
        let parent = self
            .known_header(parent_hash)
            .ok_or(ConsensusError::Invalid("unknown block parent"))?;
        let previous = parent
            .parent
            .and_then(|hash| self.known_header(&hash))
            .map(|header| header.timestamp);
        next_target(&parent.target, previous, parent.timestamp)
    }

    /// Blocks on `tip`'s branch that the active chain does not contain, oldest first. Called with
    /// the previous tip after a reorg, these are the disconnected blocks.
    pub fn blocks_off_active_chain(&self, tip: &Hash) -> Vec<&HeartbeatBlock> {
        let mut off_chain = Vec::new();
        let (mut branch, mut active) = (self.blocks.get(tip), self.blocks.get(&self.active_tip));
        while let (Some(left), Some(right)) = (branch, active) {
            if left.hash() == right.hash() {
                break;
            }
            if left.header.height >= right.header.height {
                off_chain.push(left);
                branch = left
                    .header
                    .parent
                    .and_then(|parent| self.blocks.get(&parent));
            }
            if right.header.height >= left.header.height {
                active = right
                    .header
                    .parent
                    .and_then(|parent| self.blocks.get(&parent));
            }
        }
        off_chain.reverse();
        off_chain
    }

    /// `lowest_tip` covers both the new and the previous tip, so `rollback_submission` keeps its
    /// state and `tip` always finds the active one. `newest` is the next block's parent.
    fn prune_states(&mut self, lowest_tip: u64, newest: &Hash) {
        let window_start = lowest_tip.saturating_sub(self.state_window);
        let tip = lowest_tip.max(self.replay_height);
        let interval = self.state_checkpoint_interval;
        let archive = self.archive.as_deref();
        let mut archived = false;
        self.states.retain(|hash, state| {
            if hash == newest || state.height >= window_start || state.height == 0 {
                return true;
            }
            if !checkpoint_kept(state.height, tip, interval) {
                return false;
            }
            let Some(archive) = archive else {
                return true;
            };
            archive.put(state);
            archived = true;
            false
        });
        if let Some(archive) = archive.filter(|_| archived) {
            archive.retain(&|height| checkpoint_kept(height, tip, interval));
        }
    }

    /// Undo a block that was just indexed when durable publication of its active tip failed.
    ///
    /// The application calls this only for a newly activated block. Keeping the rollback local
    /// avoids cloning every historical block and post-state before each submission.
    pub(crate) fn rollback_submission(&mut self, hash: Hash, previous_tip: Hash) {
        self.blocks.remove(&hash);
        self.states.remove(&hash);
        self.chainwork.remove(&hash);
        self.active_tip = previous_tip;
    }

    pub fn canonical_block_at_height(&self, height: u64) -> Option<&HeartbeatBlock> {
        let mut cursor = self.active_tip;
        loop {
            let block = self.blocks.get(&cursor)?;
            match block.header.height.cmp(&height) {
                std::cmp::Ordering::Equal => return Some(block),
                std::cmp::Ordering::Less => return None,
                std::cmp::Ordering::Greater => cursor = block.header.parent?,
            }
        }
    }

    /// Return only the canonical suffix above `height`, in ascending order.
    pub fn canonical_blocks_after(&self, height: Option<u64>) -> Vec<&HeartbeatBlock> {
        let mut blocks = Vec::new();
        let mut cursor = self.active_tip;
        loop {
            let block = self
                .blocks
                .get(&cursor)
                .expect("canonical block is indexed");
            if height.is_some_and(|height| block.header.height <= height) {
                break;
            }
            blocks.push(block);
            let Some(parent) = block.header.parent else {
                break;
            };
            cursor = parent;
        }
        blocks.reverse();
        blocks
    }

    pub fn canonical_blocks(&self) -> Vec<&HeartbeatBlock> {
        self.canonical_blocks_after(None)
    }
}

fn deferred_size(block: &HeartbeatBlock) -> usize {
    block.canonical_size()
        + block
            .sidecar
            .as_ref()
            .map_or(0, |sidecar| sidecar.canonical_size())
}

impl ConsensusState {
    pub fn state_root(&self) -> Hash {
        let mut out = Vec::new();
        put_u64(&mut out, self.height);
        put_u64(&mut out, self.minted);
        put_u64(&mut out, self.ledger_blocks);
        // Tip hashes, timestamps, chainwork and the current Ledger anchor are derived chain indexes.
        // They are deliberately not part of the UTXO state commitment: an included sidecar's
        // next anchor is the hash of the header that contains this root, so committing that anchor
        // here would create a hash cycle.
        put_u32(&mut out, self.utxos.len() as u32);
        for (point, output) in &self.utxos {
            put_outpoint(&mut out, point);
            put_output(&mut out, output);
        }
        put_u32(&mut out, self.tickets.len() as u32);
        for ticket in self.tickets.values() {
            out.extend_from_slice(&ticket.id);
            put_u64(&mut out, ticket.included_height);
            put_u64(&mut out, ticket.budget);
            put_u64(&mut out, ticket.duration);
            out.extend_from_slice(&ticket.mining_public_key);
            out.extend_from_slice(&ticket.lottery_seed);
        }
        tagged_hash(b"IUNA-STATE-v1", &out)
    }

    pub fn monetary_total(&self) -> Result<Amount> {
        let mut total = 0u64;
        for output in self.utxos.values() {
            total = checked_add(total, output.value)?;
        }
        for ticket in self.tickets.values() {
            let processed = self
                .height
                .checked_sub(ticket.included_height)
                .and_then(|offset| offset.checked_add(1))
                .ok_or(ConsensusError::Arithmetic)?;
            total = checked_add(total, ticket.remaining_after(processed)?)?;
        }
        Ok(total)
    }

    pub fn median_time_past(&self) -> Result<u64> {
        let mut timestamps = self.recent_timestamps.clone();
        timestamps.sort_unstable();
        timestamps
            .get(timestamps.len() / 2)
            .copied()
            .ok_or(ConsensusError::Invalid("missing median-time-past context"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Winner {
    pub ticket_id: Hash,
    pub slot: u64,
    pub output: Hash,
    pub weight: Amount,
    pub heartbeat: Hash,
    pub heartbeat_height: u64,
    pub parent_anchor: Hash,
    pub mining_public_key: PublicKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InclusionWinners {
    /// The exact deadline winner that the next heartbeat must include, when present.
    pub required: Option<Winner>,
    /// Every winner whose three-heartbeat inclusion window is still open.
    pub available: Vec<Winner>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PayoutBreakdown {
    pub subsidy: Amount,
    pub winner_fees: Amount,
    pub heartbeat_fees: Amount,
    pub inclusion_fees: Amount,
    pub expenditure: Amount,
}

impl PayoutBreakdown {
    pub fn heartbeat_total(&self) -> Result<Amount> {
        checked_add(
            checked_add(self.heartbeat_fees, self.inclusion_fees)?,
            self.expenditure,
        )
    }

    fn canonical(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_u64(&mut out, self.subsidy);
        put_u64(&mut out, self.winner_fees);
        put_u64(&mut out, self.heartbeat_fees);
        put_u64(&mut out, self.inclusion_fees);
        put_u64(&mut out, self.expenditure);
        out
    }
}

/// Replays the deterministic transition for an already-known block and returns the exact payout
/// amounts committed by its header. Adapters use this instead of duplicating reward arithmetic.
pub fn block_payouts(parent: &ConsensusState, block: &HeartbeatBlock) -> Result<PayoutBreakdown> {
    transition(parent, block).map(|(_, payouts)| payouts)
}

pub fn subsidy(ledger_blocks_before: u64, minted: Amount) -> Result<Amount> {
    if minted > MAX_MONEY {
        return Err(ConsensusError::Invalid("minted supply exceeds MAX_MONEY"));
    }
    let remaining = MAX_MONEY - minted;
    if remaining == 0 {
        return Ok(0);
    }
    let era = ledger_blocks_before / LEDGER_HALVING_INTERVAL;
    let scheduled = if era < 33 {
        INITIAL_LEDGER_SUBSIDY >> era
    } else {
        0
    };
    Ok(if scheduled == 0 {
        remaining
    } else {
        scheduled.min(remaining)
    })
}

pub fn minimum_ticket_budget(duration: u64, next_reward: Amount) -> Result<Amount> {
    if !(1..=MAX_TICKET_DURATION).contains(&duration) {
        return Err(ConsensusError::Invalid("ticket duration out of range"));
    }
    let numerator = checked_mul(next_reward, duration + 9)?;
    let denominator = LEDGER_TARGET_INTERVAL_HEARTBEATS
        .checked_mul(MAX_TRACKED_TICKETS as u64)
        .ok_or(ConsensusError::Arithmetic)?;
    let capacity_price = numerator
        .checked_add(denominator - 1)
        .ok_or(ConsensusError::Arithmetic)?
        / denominator;
    Ok(MIN_TICKET_BUDGET.max(capacity_price))
}

pub fn lottery_winners(state: &ConsensusState, heartbeat: &Heartbeat) -> Result<Vec<Winner>> {
    let active = state
        .tickets
        .values()
        .filter_map(|ticket| {
            ticket
                .weight_at(heartbeat.height)
                .map(|weight| (ticket, weight))
        })
        .collect::<Vec<_>>();
    let total_weight = active
        .iter()
        .try_fold(0u64, |sum, (_, weight)| checked_add(sum, *weight))?;
    if total_weight > MAX_MONEY {
        return Err(ConsensusError::Invalid(
            "active lottery weight exceeds MAX_MONEY",
        ));
    }
    if total_weight == 0 {
        return Ok(Vec::new());
    }
    let denominator =
        BigUint::from(LEDGER_TARGET_INTERVAL_HEARTBEATS) * BigUint::from(total_weight);
    let two_256 = BigUint::one() << 256usize;
    let mut winners = Vec::new();
    for (ticket, weight) in active {
        let slot = ticket
            .slot_at(heartbeat.height)
            .ok_or(ConsensusError::Invalid("active ticket is missing its slot"))?;
        let mut lottery_preimage = Vec::new();
        lottery_preimage.extend_from_slice(&heartbeat.hash);
        lottery_preimage.extend_from_slice(&heartbeat.parent_anchor);
        lottery_preimage.extend_from_slice(&ticket.id);
        put_u64(&mut lottery_preimage, slot);
        lottery_preimage.extend_from_slice(&ticket.lottery_seed);
        let output = tagged_hash(b"IUNA-LEDGER-LOT-v1", &lottery_preimage);
        let threshold = (&two_256 * BigUint::from(weight)) / &denominator;
        if BigUint::from_bytes_be(&output) < threshold {
            winners.push(Winner {
                ticket_id: ticket.id,
                slot,
                output,
                weight,
                heartbeat: heartbeat.hash,
                heartbeat_height: heartbeat.height,
                parent_anchor: heartbeat.parent_anchor,
                mining_public_key: ticket.mining_public_key,
            });
        }
    }
    Ok(winners)
}

pub fn inclusion_winners(
    parent: &ConsensusState,
    inclusion_height: u64,
) -> Result<InclusionWinners> {
    if inclusion_height != next_height(parent.height)? {
        return Err(ConsensusError::Invalid(
            "winner selection requested for non-child height",
        ));
    }
    let mut available = Vec::new();
    for heartbeat in &parent.heartbeats {
        if heartbeat.height >= inclusion_height
            || heartbeat.height.saturating_add(WINNER_WINDOW) < inclusion_height
            || heartbeat.parent_anchor != parent.ledger_parent_anchor
        {
            continue;
        }
        available.extend(lottery_winners(parent, heartbeat)?);
    }
    let deadline_height = inclusion_height.saturating_sub(WINNER_WINDOW);
    let required = available
        .iter()
        .filter(|winner| winner.heartbeat_height == deadline_height)
        .min_by(|left, right| compare_winners(left, right))
        .cloned();
    available.sort_by(|left, right| {
        left.heartbeat_height
            .cmp(&right.heartbeat_height)
            .then_with(|| compare_winners(left, right))
    });
    Ok(InclusionWinners {
        required,
        available,
    })
}

/// Validates a signed, non-empty Ledger candidate for one of the winners that may be included in
/// the next Heartbeat. Deadline fallbacks are constructed by Heartbeat miners and therefore do
/// not enter the candidate pool.
pub fn validate_pending_candidate(state: &ConsensusState, sidecar: &Sidecar) -> Result<Amount> {
    let inclusion_height = next_height(state.height)?;
    let selection = inclusion_winners(state, inclusion_height)?;
    if selection.required.is_some() {
        return Err(ConsensusError::Invalid(
            "deadline accepts only the canonical empty fallback",
        ));
    }
    if sidecar.transactions.is_empty() {
        return Err(ConsensusError::Invalid(
            "signed Ledger candidate must contain transactions",
        ));
    }
    check_sidecar_commitments(sidecar)?;
    let winner = eligible_sidecar_winner(state, sidecar, selection.available)?;
    verify_signature(
        &winner.mining_public_key,
        &sidecar.header.signing_digest(),
        &sidecar.header.signature,
    )?;
    let mut snapshot = state.clone();
    apply_ledger_batch(&mut snapshot, &sidecar.transactions)
}

pub fn validate_pending_transaction(
    state: &ConsensusState,
    transaction: &Transaction,
) -> Result<Amount> {
    validate_pending_transactions(state, std::slice::from_ref(transaction))
}

pub fn validate_pending_transactions(
    state: &ConsensusState,
    transactions: &[Transaction],
) -> Result<Amount> {
    let mut snapshot = state.clone();
    apply_ledger_batch(&mut snapshot, transactions)
}

pub fn validate_pending_ticket(
    state: &ConsensusState,
    ticket: &TicketTransaction,
) -> Result<Amount> {
    validate_pending_tickets(state, std::slice::from_ref(ticket))
}

pub fn validate_pending_tickets(
    state: &ConsensusState,
    tickets: &[TicketTransaction],
) -> Result<Amount> {
    let height = state
        .height
        .checked_add(1)
        .ok_or(ConsensusError::Arithmetic)?;
    let mut snapshot = state.clone();
    snapshot.height = height;
    let mut payouts = PayoutBreakdown::default();
    apply_ticket_batch(&mut snapshot, height, tickets, &mut payouts)?;
    Ok(payouts.inclusion_fees)
}

/// Heartbeats over which a ticket of `duration` slots pays out its budget, inclusion block first.
pub fn ticket_lifecycle_length(duration: u64) -> u64 {
    duration + 9
}

/// Splits a sidecar's summed fees into the Heartbeat miner's and the Ledger winner's share. The
/// split applies to the block total, so an odd unit goes to the winner once per block.
pub fn split_transaction_fees(total: Amount) -> (Amount, Amount) {
    let heartbeat = total / 2;
    (heartbeat, total - heartbeat)
}

/// The winner `block`'s sidecar settles to, if it has one.
pub fn sidecar_winner(parent: &ConsensusState, block: &HeartbeatBlock) -> Result<Option<Winner>> {
    validate_sidecar_selection(parent, block)
}

/// How many new tickets a child of `state` can add before `transition` rejects it for exceeding
/// `MAX_TRACKED_TICKETS`. Template selection must respect this, or assembly fails outright.
pub fn tracked_ticket_capacity(state: &ConsensusState) -> Result<usize> {
    let height = next_height(state.height)?;
    let mut retained = 0usize;
    for ticket in state.tickets.values() {
        let offset = height
            .checked_sub(ticket.included_height)
            .ok_or(ConsensusError::Arithmetic)?;
        if offset.checked_add(1).ok_or(ConsensusError::Arithmetic)? != ticket.lifecycle_length() {
            retained += 1;
        }
    }
    Ok(MAX_TRACKED_TICKETS.saturating_sub(retained))
}

/// Inclusion fee of `ticket` against `utxos`, or `None` if an input is missing or the inputs do
/// not cover budget and change.
pub fn ticket_fee(
    utxos: &BTreeMap<OutPoint, Output>,
    ticket: &TicketTransaction,
) -> Option<Amount> {
    let inputs = ticket.inputs.iter().try_fold(0u64, |total, input| {
        total.checked_add(utxos.get(&input.previous_output)?.value)
    })?;
    let change = ticket.change.as_ref().map_or(0, |output| output.value);
    inputs.checked_sub(change)?.checked_sub(ticket.budget)
}

/// The state `block`'s tickets are validated against: its parent plus the sidecar transactions
/// and settlement outputs. Ticket inputs may spend outputs created at that stage of the block.
pub fn ticket_input_state(
    parent: &ConsensusState,
    block: &HeartbeatBlock,
) -> Result<ConsensusState> {
    apply_ledger_stage(parent, block).map(|(state, _)| state)
}

pub fn expected_target(state: &ConsensusState, candidate_height: u64) -> Result<Hash> {
    if candidate_height != next_height(state.height)? {
        return Err(ConsensusError::Invalid(
            "target requested for non-child height",
        ));
    }
    let [.., tip] = state.recent_timestamps[..] else {
        return Err(ConsensusError::Invalid("missing target context"));
    };
    let previous = state.recent_timestamps.iter().rev().nth(1).copied();
    next_target(&state.tip_target, previous, tip)
}

/// The target of a child of the block with `tip_target` and timestamp `tip`, whose own parent has
/// timestamp `previous`.
fn next_target(tip_target: &Hash, previous: Option<u64>, tip: u64) -> Result<Hash> {
    // Relative ASERT: each child scales its parent's target by 2^((solvetime - 60) / half-life).
    // Without clamping this telescopes to absolute ASERT, but it carries no schedule debt from an
    // old genesis timestamp, so a network that was idle for days does not burst-mine to catch up.
    // 16 fractional bits and the BCH integer polynomial keep implementations bit-for-bit
    // deterministic. The child header commits a target derived solely from parent context; its
    // own timestamp first influences the following block, so miners cannot choose their target.
    let Some(previous) = previous else {
        // Block 1 has no parent solvetime and keeps the genesis target.
        return Ok(*tip_target);
    };
    let solvetime = i128::from(tip) - i128::from(previous);
    let drift = solvetime - i128::from(HEARTBEAT_TARGET_INTERVAL_SECS);
    // Round to nearest: always flooring would bias every step harder and drift the mean
    // Heartbeat interval above 60 seconds.
    let half_life = i128::from(ASERT_HALF_LIFE_SECS);
    let exponent = (drift * 65_536 + half_life / 2).div_euclid(half_life);
    let shifts = exponent.div_euclid(65_536);
    let fraction = exponent.rem_euclid(65_536) as u128;
    let fraction_squared = fraction * fraction;
    let fraction_cubed = fraction_squared * fraction;
    let factor = 65_536u128
        + ((195_766_423_245_049u128 * fraction
            + 971_821_376u128 * fraction_squared
            + 5_127u128 * fraction_cubed
            + (1u128 << 47))
            >> 48);
    let mut target = BigUint::from_bytes_be(tip_target) * BigUint::from(factor);
    // A 256-bit shift already passes MAX_TARGET, so this changes no result, only the allocation
    // that an unvalidated deferred timestamp could otherwise ask for.
    let shifts = shifts.min(256);
    if shifts < 0 {
        target >>= usize::try_from(-shifts).map_err(|_| ConsensusError::Arithmetic)?;
    } else {
        target <<= usize::try_from(shifts).map_err(|_| ConsensusError::Arithmetic)?;
    }
    target >>= 16usize;
    let maximum = BigUint::from_bytes_be(&MAX_TARGET);
    if target > maximum {
        target = maximum;
    }
    if target.is_zero() {
        target = BigUint::one();
    }
    biguint_hash(&target)
}

pub fn block_work(target: &Hash) -> Result<BigUint> {
    let target = BigUint::from_bytes_be(target);
    let maximum = (BigUint::one() << 256usize) - BigUint::one();
    if target.is_zero() || target > maximum {
        return Err(ConsensusError::Invalid("target out of range"));
    }
    Ok((&maximum / (target + BigUint::one())) + BigUint::one())
}

/// Construct and validate the special height-zero block.  The caller mines the returned header
/// by varying its nonce and then calls this function once more with that nonce.
pub fn validate_genesis(block: &HeartbeatBlock, now: u64) -> Result<ConsensusState> {
    if block.header.protocol_version != PROTOCOL_VERSION
        || block.header.network != NETWORK_ID
        || block.header.height != 0
        || block.header.parent.is_some()
        || block.header.timestamp != GENESIS_TIMESTAMP
        || block.header.miner_public_key != GENESIS_MINER
        || block.header.target != MAX_TARGET
        || block.header.timestamp > now.saturating_add(MAX_FUTURE_TIME_SECS)
        || !block.tickets.is_empty()
        || block.sidecar.is_some()
    {
        return Err(ConsensusError::Invalid("invalid genesis fields"));
    }
    if block.header.tickets_root != merkle_root(&[]) || block.header.candidate_hash.is_some() {
        return Err(ConsensusError::Invalid("invalid genesis commitments"));
    }
    let genesis_outpoint = OutPoint {
        transaction_id: tagged_hash(b"IUNA-GENESIS-PAYOUT-v1", &block.header.miner_public_key),
        output_index: 0,
    };
    let mut state = ConsensusState {
        height: 0,
        tip: ZERO_HASH,
        tip_timestamp: block.header.timestamp,
        tip_target: block.header.target,
        genesis_timestamp: block.header.timestamp,
        utxos: BTreeMap::from([(
            genesis_outpoint,
            Output {
                owner: address_from_public_key(&block.header.miner_public_key),
                value: GENESIS_REWARD,
            },
        )]),
        tickets: BTreeMap::new(),
        heartbeats: Vec::new(),
        ledger_parent_anchor: LEDGER_GENESIS_ANCHOR,
        ledger_blocks: 0,
        minted: GENESIS_REWARD,
        chainwork: block_work(&block.header.target)?,
        recent_timestamps: vec![block.header.timestamp],
    };
    if block.header.payout_commitment
        != tagged_hash(b"IUNA-HEARTBEAT-PAYOUT-v1", &GENESIS_REWARD.to_be_bytes())
    {
        return Err(ConsensusError::Invalid("invalid genesis payout commitment"));
    }
    // The tip is excluded while constructing the post-state to avoid a hash cycle.
    if block.header.post_state_root != state.state_root() {
        return Err(ConsensusError::Invalid("invalid genesis state root"));
    }
    let hash = block.hash();
    if hash != GENESIS_HASH {
        return Err(ConsensusError::Invalid(
            "genesis hash does not identify this network",
        ));
    }
    if BigUint::from_bytes_be(&hash) > BigUint::from_bytes_be(&block.header.target) {
        return Err(ConsensusError::Invalid(
            "genesis proof of work does not meet target",
        ));
    }
    state.tip = hash;
    state.heartbeats.push(Heartbeat {
        height: 0,
        hash,
        parent_anchor: LEDGER_GENESIS_ANCHOR,
    });
    Ok(state)
}

fn genesis_template(timestamp: u64, miner_public_key: PublicKey) -> Result<HeartbeatBlock> {
    let payout_commitment = tagged_hash(b"IUNA-HEARTBEAT-PAYOUT-v1", &GENESIS_REWARD.to_be_bytes());
    let mut block = HeartbeatBlock {
        header: HeartbeatHeader {
            protocol_version: PROTOCOL_VERSION,
            network: NETWORK_ID.to_owned(),
            height: 0,
            parent: None,
            timestamp,
            target: MAX_TARGET,
            nonce: 0,
            miner_public_key,
            tickets_root: merkle_root(&[]),
            candidate_hash: None,
            post_state_root: ZERO_HASH,
            payout_commitment,
        },
        tickets: Vec::new(),
        sidecar: None,
    };
    let genesis_outpoint = OutPoint {
        transaction_id: tagged_hash(b"IUNA-GENESIS-PAYOUT-v1", &miner_public_key),
        output_index: 0,
    };
    let state = ConsensusState {
        height: 0,
        tip: ZERO_HASH,
        tip_timestamp: timestamp,
        tip_target: MAX_TARGET,
        genesis_timestamp: timestamp,
        utxos: BTreeMap::from([(
            genesis_outpoint,
            Output {
                owner: address_from_public_key(&miner_public_key),
                value: GENESIS_REWARD,
            },
        )]),
        tickets: BTreeMap::new(),
        heartbeats: Vec::new(),
        ledger_parent_anchor: LEDGER_GENESIS_ANCHOR,
        ledger_blocks: 0,
        minted: GENESIS_REWARD,
        chainwork: block_work(&MAX_TARGET)?,
        recent_timestamps: vec![timestamp],
    };
    block.header.post_state_root = state.state_root();
    Ok(block)
}

pub fn pinned_genesis() -> Result<(HeartbeatBlock, ConsensusState)> {
    let mut block = genesis_template(GENESIS_TIMESTAMP, GENESIS_MINER)?;
    block.header.nonce = GENESIS_NONCE;
    let state = validate_genesis(&block, GENESIS_TIMESTAMP)?;
    Ok((block, state))
}

/// Search the genesis nonce from scratch. Only needed to re-pin
/// [`GENESIS_NONCE`] and [`GENESIS_HASH`] after a parameter change.
pub fn search_genesis_nonce(workers: usize) -> Result<(u64, Hash)> {
    let mut block = genesis_template(GENESIS_TIMESTAMP, GENESIS_MINER)?;
    if !mine_nonce_with_workers(
        &mut block.header,
        workers.max(1),
        &AtomicBool::new(false),
        None,
    )? {
        return Err(ConsensusError::Arithmetic);
    }
    Ok((block.header.nonce, block.hash()))
}

/// The one check that needs no chain state, so it can reject junk before anything expensive runs.
/// Hashes are big-endian, so comparing bytes compares values.
pub fn check_proof_of_work(header: &HeartbeatHeader) -> Result<()> {
    if header.target > MAX_TARGET || header.hash() > header.target {
        return Err(ConsensusError::Invalid(
            "proof of work does not meet target",
        ));
    }
    Ok(())
}

/// Whether `header` is a child of `parent` with the target consensus demands. Together with
/// `check_proof_of_work` this makes a header cost real work at the chain's current difficulty.
pub fn check_child_header(parent: &ConsensusState, header: &HeartbeatHeader) -> Result<()> {
    if header.height != next_height(parent.height)? || header.parent != Some(parent.tip) {
        return Err(ConsensusError::Invalid("block is not a direct child"));
    }
    if header.target != expected_target(parent, header.height)? {
        return Err(ConsensusError::Invalid("incorrect ASERT target"));
    }
    Ok(())
}

pub fn apply_block(
    parent: &ConsensusState,
    block: &HeartbeatBlock,
    now: u64,
) -> Result<ConsensusState> {
    check_proof_of_work(&block.header)?;
    validate_header_shell(parent, block, now)?;
    let (mut state, payouts) = transition(parent, block)?;
    if block.header.payout_commitment != tagged_hash(b"IUNA-PAYOUTS-v1", &payouts.canonical()) {
        return Err(ConsensusError::Invalid("invalid payout commitment"));
    }
    if block.header.post_state_root != state.state_root() {
        return Err(ConsensusError::Invalid("invalid post-state root"));
    }
    let hash = block.hash();
    state.tip = hash;
    if block.sidecar.is_some() {
        state.ledger_parent_anchor = hash;
    }
    state.heartbeats.push(Heartbeat {
        height: block.header.height,
        hash,
        parent_anchor: state.ledger_parent_anchor,
    });
    state
        .heartbeats
        .retain(|heartbeat| heartbeat.height.saturating_add(WINNER_WINDOW) >= block.header.height);
    Ok(state)
}

/// Build all deterministic commitments and search the PoW nonce.  Production miners can
/// use the same commitments with a parallel nonce search; this helper is also the canonical test
/// fixture builder.
pub fn mine_child(
    parent: &ConsensusState,
    timestamp: u64,
    miner_public_key: PublicKey,
    tickets: Vec<TicketTransaction>,
    sidecar: Option<Sidecar>,
) -> Result<(HeartbeatBlock, ConsensusState)> {
    mine_child_with_workers(parent, timestamp, miner_public_key, tickets, sidecar, 1)
}

pub fn mine_child_with_workers(
    parent: &ConsensusState,
    timestamp: u64,
    miner_public_key: PublicKey,
    tickets: Vec<TicketTransaction>,
    sidecar: Option<Sidecar>,
    workers: usize,
) -> Result<(HeartbeatBlock, ConsensusState)> {
    let never = AtomicBool::new(false);
    mine_child_until(
        parent,
        timestamp,
        miner_public_key,
        tickets,
        sidecar,
        workers,
        &never,
    )?
    .ok_or(ConsensusError::Arithmetic)
}

/// Like [`mine_child_with_workers`], but returns `Ok(None)` as soon as `stop` is set, so a
/// miner can abandon stale work when a peer extends the tip or the template should be refreshed.
pub fn mine_child_until(
    parent: &ConsensusState,
    timestamp: u64,
    miner_public_key: PublicKey,
    tickets: Vec<TicketTransaction>,
    sidecar: Option<Sidecar>,
    workers: usize,
    stop: &AtomicBool,
) -> Result<Option<(HeartbeatBlock, ConsensusState)>> {
    mine_child_until_with_counter(
        parent,
        timestamp,
        miner_public_key,
        tickets,
        sidecar,
        workers,
        stop,
        None,
    )
}

/// Like [`mine_child_until`], while adding every attempted nonce to `hashes`. The counter is
/// observational only and cannot affect block construction or consensus validation.
#[allow(clippy::too_many_arguments)]
pub fn mine_child_until_counted(
    parent: &ConsensusState,
    timestamp: u64,
    miner_public_key: PublicKey,
    tickets: Vec<TicketTransaction>,
    sidecar: Option<Sidecar>,
    workers: usize,
    stop: &AtomicBool,
    hashes: &AtomicU64,
) -> Result<Option<(HeartbeatBlock, ConsensusState)>> {
    mine_child_until_with_counter(
        parent,
        timestamp,
        miner_public_key,
        tickets,
        sidecar,
        workers,
        stop,
        Some(hashes),
    )
}

#[allow(clippy::too_many_arguments)]
fn mine_child_until_with_counter(
    parent: &ConsensusState,
    timestamp: u64,
    miner_public_key: PublicKey,
    tickets: Vec<TicketTransaction>,
    sidecar: Option<Sidecar>,
    workers: usize,
    stop: &AtomicBool,
    hashes: Option<&AtomicU64>,
) -> Result<Option<(HeartbeatBlock, ConsensusState)>> {
    if workers == 0 {
        return Err(ConsensusError::Invalid(
            "mining requires at least one worker",
        ));
    }
    let height = next_height(parent.height)?;
    let ticket_ids = tickets
        .iter()
        .map(TicketTransaction::id)
        .collect::<Vec<_>>();
    let mut block = HeartbeatBlock {
        header: HeartbeatHeader {
            protocol_version: PROTOCOL_VERSION,
            network: NETWORK_ID.to_owned(),
            height,
            parent: Some(parent.tip),
            timestamp,
            target: expected_target(parent, height)?,
            nonce: 0,
            miner_public_key,
            tickets_root: merkle_root(&ticket_ids),
            candidate_hash: sidecar.as_ref().map(|sidecar| sidecar.header.hash()),
            post_state_root: ZERO_HASH,
            payout_commitment: ZERO_HASH,
        },
        tickets,
        sidecar,
    };
    // Timestamp and all structural checks are deterministic. `timestamp` is also a safe `now`
    // value for locally constructed templates.
    validate_header_shell(parent, &block, timestamp)?;
    let (state, payouts) = transition(parent, &block)?;
    block.header.post_state_root = state.state_root();
    block.header.payout_commitment = tagged_hash(b"IUNA-PAYOUTS-v1", &payouts.canonical());
    if !mine_nonce_with_workers(&mut block.header, workers, stop, hashes)? {
        return Ok(None);
    }
    let state = apply_block(parent, &block, timestamp)?;
    Ok(Some((block, state)))
}

fn transition(
    parent: &ConsensusState,
    block: &HeartbeatBlock,
) -> Result<(ConsensusState, PayoutBreakdown)> {
    let block_height = block.header.height;
    let (mut state, mut payouts) = apply_ledger_stage(parent, block)?;

    apply_ticket_batch(&mut state, block_height, &block.tickets, &mut payouts)?;
    apply_lifecycle_payouts(&mut state, block_height, &mut payouts)?;
    credit_heartbeat_payout(
        &mut state,
        parent.tip,
        block_height,
        block.header.miner_public_key,
        block.sidecar.as_ref().map(|sidecar| sidecar.header.hash()),
        &payouts,
    )?;

    if state.tickets.len() > MAX_TRACKED_TICKETS {
        return Err(ConsensusError::Invalid("tracked ticket cap exceeded"));
    }
    if state.monetary_total()? != state.minted {
        return Err(ConsensusError::Invalid("monetary invariant violated"));
    }
    if state.minted > MAX_MONEY {
        return Err(ConsensusError::Invalid("money supply exceeds MAX_MONEY"));
    }

    state.tip_timestamp = block.header.timestamp;
    state.tip_target = block.header.target;
    state.recent_timestamps.push(block.header.timestamp);
    if state.recent_timestamps.len() > MEDIAN_TIME_WINDOW {
        state.recent_timestamps.remove(0);
    }
    state.chainwork += block_work(&block.header.target)?;
    if block.sidecar.is_some() {
        // The including Heartbeat hash is filled in only after the state root and PoW are verified.
        state.ledger_parent_anchor = ZERO_HASH;
    }
    Ok((state, payouts))
}

/// First stage of `transition`: the child-height state with the sidecar and settlement applied.
fn apply_ledger_stage(
    parent: &ConsensusState,
    block: &HeartbeatBlock,
) -> Result<(ConsensusState, PayoutBreakdown)> {
    let block_height = block.header.height;
    let selected_winner = validate_sidecar_selection(parent, block)?;
    let mut state = parent.clone();
    // The transition below is evaluated at the child height.  In particular, ticket
    // `remaining` and the monetary invariant must include the current lifecycle slice.
    state.height = block_height;
    let mut payouts = PayoutBreakdown::default();

    if let Some((sidecar, winner)) = block.sidecar.as_ref().zip(selected_winner.as_ref()) {
        let transaction_fees = apply_ledger_batch(&mut state, &sidecar.transactions)?;
        (payouts.heartbeat_fees, payouts.winner_fees) = split_transaction_fees(transaction_fees);
        payouts.subsidy = subsidy(state.ledger_blocks, state.minted)?;
        state.minted = checked_add(state.minted, payouts.subsidy)?;
        state.ledger_blocks = state
            .ledger_blocks
            .checked_add(1)
            .ok_or(ConsensusError::Arithmetic)?;
        credit_settlement_outputs(
            &mut state,
            parent.tip,
            block_height,
            sidecar.header.hash(),
            winner,
            block.header.miner_public_key,
            &payouts,
        )?;
    }
    Ok((state, payouts))
}

/// Returns `Ok(false)` when `stop` was set before a valid nonce was found.
fn mine_nonce_with_workers(
    header: &mut HeartbeatHeader,
    workers: usize,
    stop: &AtomicBool,
    hashes: Option<&AtomicU64>,
) -> Result<bool> {
    let template = header.clone();
    let target = header.target;
    let found = Arc::new(AtomicBool::new(false));
    let result = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for worker in 0..workers {
            let mut candidate = template.clone();
            let found = Arc::clone(&found);
            handles.push(scope.spawn(move || {
                let mut nonce = worker as u64;
                let stride = workers as u64;
                let mut unreported_hashes = 0_u64;
                loop {
                    if found.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
                        if let Some(hashes) = hashes {
                            hashes.fetch_add(unreported_hashes, Ordering::Relaxed);
                        }
                        return None;
                    }
                    candidate.nonce = nonce;
                    unreported_hashes += 1;
                    if candidate.hash() <= target {
                        if let Some(hashes) = hashes {
                            hashes.fetch_add(unreported_hashes, Ordering::Relaxed);
                        }
                        found.store(true, Ordering::Relaxed);
                        return Some(candidate);
                    }
                    if unreported_hashes == 1_024 {
                        if let Some(hashes) = hashes {
                            hashes.fetch_add(unreported_hashes, Ordering::Relaxed);
                        }
                        unreported_hashes = 0;
                    }
                    let Some(next_nonce) = nonce.checked_add(stride) else {
                        if let Some(hashes) = hashes {
                            hashes.fetch_add(unreported_hashes, Ordering::Relaxed);
                        }
                        return None;
                    };
                    nonce = next_nonce;
                }
            }));
        }
        handles
            .into_iter()
            .find_map(|handle| handle.join().ok().flatten())
    });
    let Some(mined) = result else {
        if stop.load(Ordering::Relaxed) {
            return Ok(false);
        }
        return Err(ConsensusError::Arithmetic);
    };
    *header = mined;
    Ok(true)
}

fn validate_header_shell(parent: &ConsensusState, block: &HeartbeatBlock, now: u64) -> Result<()> {
    let header = &block.header;
    if header.protocol_version != PROTOCOL_VERSION || header.network != NETWORK_ID {
        return Err(ConsensusError::Invalid("wrong protocol or network"));
    }
    if header.height != next_height(parent.height)? || header.parent != Some(parent.tip) {
        return Err(ConsensusError::Invalid("block is not a direct child"));
    }
    if header.timestamp <= parent.median_time_past()?
        || header.timestamp > now.saturating_add(MAX_FUTURE_TIME_SECS)
    {
        return Err(ConsensusError::Invalid(
            "timestamp outside consensus bounds",
        ));
    }
    if header.target != expected_target(parent, header.height)? {
        return Err(ConsensusError::Invalid("incorrect ASERT target"));
    }
    check_body_commitments(block)
}

/// Whether the body is the one the header commits to. Block IDs cover the header alone, so without
/// this anyone could attach another body to an honest block's hash. The order checks belong here:
/// `merkle_root` repeats the last node of an odd level, so a list and the same list with its tail
/// repeated share a root, and only strictly ascending IDs leave a single body per root.
fn check_body_commitments(block: &HeartbeatBlock) -> Result<()> {
    let header = &block.header;
    if block.canonical_size() > MAX_HEARTBEAT_BLOCK_BYTES {
        return Err(ConsensusError::Invalid("Heartbeat block exceeds 16 KiB"));
    }
    if block
        .sidecar
        .as_ref()
        .is_some_and(|sidecar| sidecar.canonical().len() > MAX_LEDGER_SIDECAR_BYTES)
    {
        return Err(ConsensusError::Invalid("Ledger sidecar exceeds 512 KiB"));
    }
    let ticket_ids = block
        .tickets
        .iter()
        .map(TicketTransaction::id)
        .collect::<Vec<_>>();
    ensure_canonical_order(
        ticket_ids.iter().copied(),
        "mining tickets are not in canonical order",
    )?;
    if header.tickets_root != merkle_root(&ticket_ids) {
        return Err(ConsensusError::Invalid("incorrect ticket Merkle root"));
    }
    if header.candidate_hash != block.sidecar.as_ref().map(|sidecar| sidecar.header.hash()) {
        return Err(ConsensusError::Invalid("candidate commitment mismatch"));
    }
    if let Some(sidecar) = &block.sidecar {
        check_sidecar_commitments(sidecar)?;
    }
    Ok(())
}

fn check_sidecar_commitments(sidecar: &Sidecar) -> Result<()> {
    if sidecar.canonical().len() > MAX_LEDGER_SIDECAR_BYTES {
        return Err(ConsensusError::Invalid("Ledger sidecar exceeds 512 KiB"));
    }
    let transaction_ids = sidecar
        .transactions
        .iter()
        .map(Transaction::id)
        .collect::<Vec<_>>();
    ensure_canonical_order(
        transaction_ids.iter().copied(),
        "Ledger transactions are not in canonical order",
    )?;
    if sidecar.header.transactions_root != merkle_root(&transaction_ids) {
        return Err(ConsensusError::Invalid("invalid candidate header"));
    }
    Ok(())
}

fn validate_sidecar_selection(
    parent: &ConsensusState,
    block: &HeartbeatBlock,
) -> Result<Option<Winner>> {
    let selection = inclusion_winners(parent, block.header.height)?;

    let Some(sidecar) = &block.sidecar else {
        if selection.required.is_none() {
            return Ok(None);
        }
        return Err(ConsensusError::Invalid(
            "deadline winner requires a sidecar",
        ));
    };
    if sidecar.header.version != PROTOCOL_VERSION
        || sidecar.header.parent_anchor != parent.ledger_parent_anchor
    {
        return Err(ConsensusError::Invalid("invalid candidate header"));
    }
    let winner = eligible_sidecar_winner(parent, sidecar, selection.available)?;
    match selection.required {
        Some(required) if required != winner => {
            return Err(ConsensusError::Invalid(
                "candidate does not use mandatory deadline winner",
            ));
        }
        Some(_) => {
            if !sidecar.transactions.is_empty() || !sidecar.header.signature.is_empty() {
                return Err(ConsensusError::Invalid(
                    "deadline fallback must be empty and unsigned",
                ));
            }
        }
        None => {
            if sidecar.transactions.is_empty() {
                return Err(ConsensusError::Invalid(
                    "pre-deadline candidate must contain transactions",
                ));
            }
            verify_signature(
                &winner.mining_public_key,
                &sidecar.header.signing_digest(),
                &sidecar.header.signature,
            )?;
        }
    }
    Ok(Some(winner))
}

fn eligible_sidecar_winner(
    parent: &ConsensusState,
    sidecar: &Sidecar,
    available: Vec<Winner>,
) -> Result<Winner> {
    if sidecar.header.version != PROTOCOL_VERSION
        || sidecar.header.parent_anchor != parent.ledger_parent_anchor
    {
        return Err(ConsensusError::Invalid("invalid candidate header"));
    }
    available
        .into_iter()
        .find(|winner| {
            winner.ticket_id == sidecar.header.ticket_id
                && winner.slot == sidecar.header.slot
                && winner.output == sidecar.header.lottery_output
                && winner.heartbeat == sidecar.header.lottery_heartbeat
                && winner.heartbeat_height == sidecar.header.lottery_height
                && winner.parent_anchor == sidecar.header.parent_anchor
        })
        .ok_or(ConsensusError::Invalid(
            "candidate does not reference an eligible winner",
        ))
}

fn compare_winners(left: &Winner, right: &Winner) -> std::cmp::Ordering {
    let left_cross = BigUint::from_bytes_be(&left.output) * BigUint::from(right.weight);
    let right_cross = BigUint::from_bytes_be(&right.output) * BigUint::from(left.weight);
    left_cross
        .cmp(&right_cross)
        .then_with(|| left.ticket_id.cmp(&right.ticket_id))
}

fn apply_ledger_batch(state: &mut ConsensusState, transactions: &[Transaction]) -> Result<Amount> {
    ensure_canonical_order(
        transactions.iter().map(Transaction::id),
        "Ledger transactions are not in canonical order",
    )?;
    let snapshot = state.utxos.clone();
    let mut spent = BTreeSet::new();
    let mut created = Vec::new();
    let mut fees = 0u64;
    for transaction in transactions {
        validate_transaction_shape(transaction)?;
        let id = transaction.id();
        let values = input_values(
            &snapshot,
            transaction
                .inputs
                .iter()
                .map(|input| &input.previous_output),
            "missing Ledger input",
        )?;
        let mut inputs = 0u64;
        for (index, input) in transaction.inputs.iter().enumerate() {
            if !spent.insert(input.previous_output.clone()) {
                return Err(ConsensusError::Invalid("duplicate input in Ledger batch"));
            }
            let previous = snapshot
                .get(&input.previous_output)
                .ok_or(ConsensusError::Invalid("missing Ledger input"))?;
            if previous.owner != address_from_public_key(&input.public_key) {
                return Err(ConsensusError::Invalid("Ledger input owner mismatch"));
            }
            verify_signature(
                &input.public_key,
                &transaction.signing_digest(index, &values)?,
                &input.signature,
            )?;
            inputs = checked_add(inputs, previous.value)?;
        }
        let outputs = transaction.outputs.iter().try_fold(0u64, |sum, output| {
            if output.value == 0 {
                return Err(ConsensusError::Invalid("zero-valued transaction output"));
            }
            checked_add(sum, output.value)
        })?;
        fees = checked_add(
            fees,
            inputs
                .checked_sub(outputs)
                .ok_or(ConsensusError::Invalid("transaction creates value"))?,
        )?;
        for (index, output) in transaction.outputs.iter().enumerate() {
            created.push((
                OutPoint {
                    transaction_id: id,
                    output_index: index as u32,
                },
                output.clone(),
            ));
        }
    }
    for point in spent {
        state.utxos.remove(&point);
    }
    for (point, output) in created {
        if state.utxos.insert(point, output).is_some() {
            return Err(ConsensusError::Invalid("duplicate transaction output ID"));
        }
    }
    Ok(fees)
}

fn apply_ticket_batch(
    state: &mut ConsensusState,
    height: u64,
    tickets: &[TicketTransaction],
    payouts: &mut PayoutBreakdown,
) -> Result<()> {
    ensure_canonical_order(
        tickets.iter().map(TicketTransaction::id),
        "mining tickets are not in canonical order",
    )?;
    let snapshot = state.utxos.clone();
    let next_reward = subsidy(state.ledger_blocks, state.minted)?;
    let mut spent = BTreeSet::new();
    let mut changes = Vec::new();
    let mut records = Vec::new();
    for ticket in tickets {
        if ticket.version != PROTOCOL_VERSION || ticket.inputs.is_empty() {
            return Err(ConsensusError::Invalid("invalid ticket shape"));
        }
        if ticket.budget < minimum_ticket_budget(ticket.duration, next_reward)? {
            return Err(ConsensusError::Invalid(
                "ticket budget below consensus minimum",
            ));
        }
        if ticket
            .change
            .as_ref()
            .is_some_and(|output| output.value == 0)
        {
            return Err(ConsensusError::Invalid("zero-valued ticket change"));
        }
        let id = ticket.id();
        let values = input_values(
            &snapshot,
            ticket.inputs.iter().map(|input| &input.previous_output),
            "missing ticket input",
        )?;
        for (index, input) in ticket.inputs.iter().enumerate() {
            if !spent.insert(input.previous_output.clone()) {
                return Err(ConsensusError::Invalid("duplicate input in ticket batch"));
            }
            let previous = snapshot
                .get(&input.previous_output)
                .ok_or(ConsensusError::Invalid("missing ticket input"))?;
            if previous.owner != address_from_public_key(&input.public_key) {
                return Err(ConsensusError::Invalid("ticket input owner mismatch"));
            }
            verify_signature(
                &input.public_key,
                &ticket.signing_digest(index, &values)?,
                &input.signature,
            )?;
        }
        let fee = ticket_fee(&snapshot, ticket).ok_or(ConsensusError::Invalid(
            "ticket inputs do not cover budget and change",
        ))?;
        if fee == 0 {
            return Err(ConsensusError::Invalid(
                "ticket inclusion fee must be positive",
            ));
        }
        payouts.inclusion_fees = checked_add(payouts.inclusion_fees, fee)?;
        if let Some(change) = &ticket.change {
            changes.push((
                OutPoint {
                    transaction_id: id,
                    output_index: 0,
                },
                change.clone(),
            ));
        }
        records.push(TrackedTicket {
            id,
            included_height: height,
            budget: ticket.budget,
            duration: ticket.duration,
            mining_public_key: ticket.mining_public_key,
            lottery_seed: ticket.lottery_seed,
        });
    }
    for point in spent {
        state.utxos.remove(&point);
    }
    for (point, output) in changes {
        if state.utxos.insert(point, output).is_some() {
            return Err(ConsensusError::Invalid("duplicate ticket change ID"));
        }
    }
    for record in records {
        if state.tickets.insert(record.id, record).is_some() {
            return Err(ConsensusError::Invalid("duplicate tracked ticket ID"));
        }
    }
    Ok(())
}

fn apply_lifecycle_payouts(
    state: &mut ConsensusState,
    height: u64,
    payouts: &mut PayoutBreakdown,
) -> Result<()> {
    let mut completed = Vec::new();
    for ticket in state.tickets.values() {
        let offset = height
            .checked_sub(ticket.included_height)
            .ok_or(ConsensusError::Arithmetic)?;
        if offset < ticket.lifecycle_length() {
            payouts.expenditure = checked_add(payouts.expenditure, ticket.payout_at(offset)?)?;
        }
        if offset.checked_add(1).ok_or(ConsensusError::Arithmetic)? == ticket.lifecycle_length() {
            if ticket.remaining_after(ticket.lifecycle_length())? != 0 {
                return Err(ConsensusError::Invalid("completed ticket retains value"));
            }
            completed.push(ticket.id);
        }
    }
    for id in completed {
        state.tickets.remove(&id);
    }
    Ok(())
}

fn credit_settlement_outputs(
    state: &mut ConsensusState,
    parent: Hash,
    height: u64,
    candidate_hash: Hash,
    winner: &Winner,
    heartbeat_miner: PublicKey,
    payouts: &PayoutBreakdown,
) -> Result<()> {
    let mut preimage = Vec::new();
    preimage.extend_from_slice(&parent);
    put_u64(&mut preimage, height);
    preimage.extend_from_slice(&candidate_hash);
    preimage.extend_from_slice(&winner.ticket_id);
    preimage.extend_from_slice(&heartbeat_miner);
    preimage.extend_from_slice(&payouts.canonical());
    let settlement = tagged_hash(b"IUNA-LEDGER-SETTLEMENT-v1", &preimage);
    if payouts.subsidy > 0 {
        state.utxos.insert(
            OutPoint {
                transaction_id: settlement,
                output_index: 0,
            },
            Output {
                owner: address_from_public_key(&winner.mining_public_key),
                value: payouts.subsidy,
            },
        );
    }
    if payouts.winner_fees > 0 {
        state.utxos.insert(
            OutPoint {
                transaction_id: settlement,
                output_index: 1,
            },
            Output {
                owner: address_from_public_key(&winner.mining_public_key),
                value: payouts.winner_fees,
            },
        );
    }
    Ok(())
}

fn credit_heartbeat_payout(
    state: &mut ConsensusState,
    parent: Hash,
    height: u64,
    miner: PublicKey,
    candidate_hash: Option<Hash>,
    payouts: &PayoutBreakdown,
) -> Result<()> {
    let total = payouts.heartbeat_total()?;
    if total == 0 {
        return Ok(());
    }
    let mut preimage = Vec::new();
    preimage.extend_from_slice(&parent);
    put_u64(&mut preimage, height);
    preimage.extend_from_slice(&miner);
    preimage.extend_from_slice(&candidate_hash.unwrap_or(ZERO_HASH));
    preimage.extend_from_slice(&payouts.canonical());
    let id = tagged_hash(b"IUNA-HEARTBEAT-PAYOUT-v1", &preimage);
    if state
        .utxos
        .insert(
            OutPoint {
                transaction_id: id,
                output_index: 0,
            },
            Output {
                owner: address_from_public_key(&miner),
                value: total,
            },
        )
        .is_some()
    {
        return Err(ConsensusError::Invalid("duplicate Heartbeat payout ID"));
    }
    Ok(())
}

fn validate_transaction_shape(transaction: &Transaction) -> Result<()> {
    if transaction.version != PROTOCOL_VERSION
        || transaction.inputs.is_empty()
        || transaction.outputs.is_empty()
    {
        return Err(ConsensusError::Invalid("invalid transaction shape"));
    }
    Ok(())
}

fn verify_signature(public_key: &PublicKey, message: &[u8], signature: &[u8]) -> Result<()> {
    let key = VerifyingKey::from_bytes(public_key)
        .map_err(|_| ConsensusError::Invalid("invalid Ed25519 public key"))?;
    let signature = Signature::from_slice(signature)
        .map_err(|_| ConsensusError::Invalid("invalid Ed25519 signature length"))?;
    key.verify(message, &signature)
        .map_err(|_| ConsensusError::Invalid("invalid Ed25519 signature"))
}

pub fn merkle_root(leaves: &[Hash]) -> Hash {
    if leaves.is_empty() {
        return tagged_hash(b"IUNA-MERKLE-EMPTY-v1", &[]);
    }
    let mut level = leaves
        .iter()
        .map(|leaf| tagged_hash(b"IUNA-MERKLE-LEAF-v1", leaf))
        .collect::<Vec<_>>();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().expect("non-empty Merkle level"));
        }
        level = level
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let mut value = [0u8; 64];
                value[..32].copy_from_slice(&pair[0]);
                value[32..].copy_from_slice(&pair[1]);
                tagged_hash(b"IUNA-MERKLE-NODE-v1", &value)
            })
            .collect();
    }
    level[0]
}

pub fn tagged_hash(domain: &[u8], bytes: &[u8]) -> Hash {
    let mut hasher = Sha256::new();
    put_bytes_to_hasher(&mut hasher, domain);
    put_bytes_to_hasher(&mut hasher, bytes);
    hasher.finalize().into()
}

/// Commits an output to an Ed25519 key without publishing that key before it is spent.
pub fn address_from_public_key(public_key: &PublicKey) -> Address {
    tagged_hash(b"IUNA-ADDRESS-v1", public_key)
}

fn put_bytes_to_hasher(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn checked_add(left: u64, right: u64) -> Result<u64> {
    left.checked_add(right).ok_or(ConsensusError::Arithmetic)
}

fn checked_mul(left: u64, right: u64) -> Result<u64> {
    left.checked_mul(right).ok_or(ConsensusError::Arithmetic)
}

fn next_height(height: u64) -> Result<u64> {
    height.checked_add(1).ok_or(ConsensusError::Arithmetic)
}

fn biguint_hash(value: &BigUint) -> Result<Hash> {
    let bytes = value.to_bytes_be();
    if bytes.len() > 32 {
        return Err(ConsensusError::Arithmetic);
    }
    let mut hash = [0u8; 32];
    hash[32 - bytes.len()..].copy_from_slice(&bytes);
    Ok(hash)
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_u32(out, bytes.len() as u32);
    out.extend_from_slice(bytes);
}

fn put_outpoint(out: &mut Vec<u8>, point: &OutPoint) {
    out.extend_from_slice(&point.transaction_id);
    put_u32(out, point.output_index);
}

fn put_output(out: &mut Vec<u8>, output: &Output) {
    out.extend_from_slice(&output.owner);
    put_u64(out, output.value);
}

fn ensure_canonical_order(ids: impl IntoIterator<Item = Hash>, error: &'static str) -> Result<()> {
    let mut previous = None;
    for id in ids {
        if previous.is_some_and(|previous| previous >= id) {
            return Err(ConsensusError::Invalid(error));
        }
        previous = Some(id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const GENESIS_SECRET: [u8; 32] = [
        0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c,
        0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03, 0x1c, 0xae,
        0x7f, 0x60,
    ];

    fn ticket(budget: Amount, duration: u64, included_height: u64) -> TrackedTicket {
        TrackedTicket {
            id: [7; 32],
            included_height,
            budget,
            duration,
            mining_public_key: [8; 32],
            lottery_seed: [9; 32],
        }
    }

    fn extend(chain: &mut Chain, parent: &ConsensusState, miner: u8) -> ConsensusState {
        let timestamp = parent.tip_timestamp + HEARTBEAT_TARGET_INTERVAL_SECS;
        let (block, state) = mine_child(parent, timestamp, [miner; 32], Vec::new(), None).unwrap();
        chain.submit(block, timestamp).unwrap();
        state
    }

    #[test]
    fn pruned_states_are_rebuilt_exactly_and_old_forks_still_activate() {
        let mut chain = Chain::from_genesis().unwrap();
        chain.state_window = 2;
        chain.state_checkpoint_interval = 4;
        let mut history = vec![chain.tip().clone()];
        for _ in 0..7 {
            let tip = chain.tip().clone();
            history.push(extend(&mut chain, &tip, 31));
        }

        let mut kept = chain
            .states
            .values()
            .map(|state| state.height)
            .collect::<Vec<_>>();
        kept.sort_unstable();
        assert_eq!(kept, [0, 4, 5, 6, 7]);
        for state in &history {
            assert_eq!(chain.state(&state.tip).unwrap().as_ref(), state);
        }
        assert!(chain.state(&[1; 32]).is_none());

        // Heights 2 and 3 are gone, and a heavier branch can still fork from below them.
        let mut branch = history[1].clone();
        for _ in 0..7 {
            branch = extend(&mut chain, &branch, 32);
        }
        assert_eq!(chain.tip().tip, branch.tip);
        assert_eq!(chain.canonical_blocks().len(), 9);
    }

    fn kept_checkpoints(tip: u64) -> Vec<u64> {
        (1..=tip / STATE_CHECKPOINT_INTERVAL)
            .map(|index| index * STATE_CHECKPOINT_INTERVAL)
            .filter(|height| checkpoint_kept(*height, tip, STATE_CHECKPOINT_INTERVAL))
            .collect()
    }

    #[test]
    fn checkpoints_halve_towards_genesis() {
        let older: Vec<u64> = kept_checkpoints(1 << 20)
            .into_iter()
            .take_while(|height| *height <= 7 << 17)
            .collect();
        assert_eq!(older, [1 << 19, 3 << 18, 7 << 17]);

        for (tip, most) in [(1_000, 1), (10_000, 5), (525_600, 11), (10_512_000, 15)] {
            assert!(kept_checkpoints(tip).len() <= most + 8, "tip {tip}");
        }
    }

    #[test]
    fn a_dropped_checkpoint_never_returns_and_a_rebuild_stays_near_its_depth() {
        for tip in (0..12_000u64).step_by(61) {
            let (now, later) = (kept_checkpoints(tip), kept_checkpoints(tip + 61));
            assert!(
                later
                    .iter()
                    .all(|height| now.contains(height) || *height > tip)
            );

            for height in (0..tip).step_by(97) {
                let start = now.iter().rev().find(|kept| **kept <= height).unwrap_or(&0);
                assert!(height - start <= 2 * (tip - height) + STATE_CHECKPOINT_INTERVAL);
            }
        }
    }

    #[derive(Debug, Default)]
    struct MemoryArchive(std::sync::Mutex<BTreeMap<Hash, ConsensusState>>);

    impl StateArchive for MemoryArchive {
        fn put(&self, state: &ConsensusState) {
            self.0.lock().unwrap().insert(state.tip, state.clone());
        }

        fn get(&self, hash: &Hash) -> Option<ConsensusState> {
            self.0.lock().unwrap().get(hash).cloned()
        }

        fn retain(&self, keep: &dyn Fn(u64) -> bool) {
            self.0.lock().unwrap().retain(|_, state| keep(state.height));
        }
    }

    fn with_bad_root(parent: &ConsensusState, miner: u8) -> HeartbeatBlock {
        let timestamp = parent.tip_timestamp + HEARTBEAT_TARGET_INTERVAL_SECS;
        let (mut block, _) = mine_child(parent, timestamp, [miner; 32], Vec::new(), None).unwrap();
        block.header.post_state_root = [1; 32];
        while check_proof_of_work(&block.header).is_err() {
            block.header.nonce += 1;
        }
        block
    }

    #[test]
    fn a_deep_fork_block_costs_no_replay_until_its_branch_outweighs_the_tip() {
        let mut chain = Chain::from_genesis()
            .unwrap()
            .with_state_archive(Arc::new(UntouchableArchive))
            .with_state_retention(2, 4);
        let mut history = vec![chain.tip().clone()];
        for _ in 0..8 {
            let tip = chain.tip().clone();
            history.push(extend(&mut chain, &tip, 43));
        }
        let block = with_bad_root(&history[4], 44);
        let timestamp = block.header.timestamp;
        let hash = block.hash();

        assert_eq!(chain.submit(block.clone(), timestamp), Ok(false));
        assert_eq!(chain.submit(block, timestamp), Ok(false));

        assert!(chain.block(&hash).is_none());
        assert_eq!(chain.tip().tip, history[8].tip);
    }

    fn eight_blocks() -> (Chain, Vec<ConsensusState>) {
        let mut chain = Chain::from_genesis().unwrap().with_state_retention(2, 4);
        let mut history = vec![chain.tip().clone()];
        for _ in 0..8 {
            let tip = chain.tip().clone();
            history.push(extend(&mut chain, &tip, 49));
        }
        (chain, history)
    }

    fn solved(mut block: HeartbeatBlock) -> HeartbeatBlock {
        while check_proof_of_work(&block.header).is_err() {
            block.header.nonce += 1;
        }
        block
    }

    #[test]
    fn extreme_timestamps_clamp_the_target_without_a_huge_shift() {
        assert_eq!(
            next_target(&MAX_TARGET, Some(0), u64::MAX).unwrap(),
            MAX_TARGET
        );
        // One half-life late doubles the target, well below the clamp.
        let mut one = [0; 32];
        one[31] = 1;
        let doubled = next_target(
            &one,
            Some(0),
            HEARTBEAT_TARGET_INTERVAL_SECS + ASERT_HALF_LIFE_SECS as u64,
        );
        assert_eq!(
            BigUint::from_bytes_be(&doubled.unwrap()),
            BigUint::from(2u8)
        );
    }

    #[test]
    fn a_deferred_block_cannot_carry_a_timestamp_past_the_clock() {
        let (mut chain, history) = eight_blocks();
        let now = history[1].tip_timestamp + HEARTBEAT_TARGET_INTERVAL_SECS;
        let (mut block, _) = mine_child(&history[1], now, [50; 32], Vec::new(), None).unwrap();
        let honest = block.clone();
        block.header.timestamp = u64::MAX;
        let block = solved(block);

        assert_eq!(
            chain.submit(block, now),
            Err(ConsensusError::Invalid(
                "timestamp outside consensus bounds"
            ))
        );
        // Refused for now, not for good: the same goes for a node whose clock lags.
        let early = now - MAX_FUTURE_TIME_SECS - 1;
        assert!(chain.submit(honest.clone(), early).is_err());
        assert_eq!(chain.submit(honest, now), Ok(false));
    }

    #[test]
    fn another_body_on_an_honest_hash_cannot_take_its_place() {
        let (mut chain, history) = eight_blocks();
        let now = history[1].tip_timestamp + HEARTBEAT_TARGET_INTERVAL_SECS;
        let (honest, _) = mine_child(&history[1], now, [51; 32], Vec::new(), None).unwrap();
        let mut mangled = honest.clone();
        mangled.tickets.push(TicketTransaction {
            version: PROTOCOL_VERSION,
            inputs: Vec::new(),
            budget: 1,
            duration: 1,
            mining_public_key: [1; 32],
            lottery_seed: [2; 32],
            change: None,
        });
        assert_eq!(mangled.hash(), honest.hash());

        assert_eq!(
            chain.submit(mangled, now),
            Err(ConsensusError::Invalid("incorrect ticket Merkle root"))
        );
        assert_eq!(chain.submit(honest, now), Ok(false));
    }

    fn sorted_by<T>(mut items: Vec<T>, id: impl Fn(&T) -> Hash) -> Vec<T> {
        items.sort_by_key(|item| id(item));
        items
    }

    #[test]
    fn a_repeated_ticket_tail_shares_the_root_but_not_the_block() {
        let tickets = sorted_by(
            (0..3u8)
                .map(|seed| TicketTransaction {
                    version: PROTOCOL_VERSION,
                    inputs: Vec::new(),
                    budget: 1,
                    duration: 1,
                    mining_public_key: [1; 32],
                    lottery_seed: [seed; 32],
                    change: None,
                })
                .collect(),
            TicketTransaction::id,
        );
        let (_, genesis) = pinned_genesis().unwrap();
        let (mut honest, _) =
            mine_child(&genesis, GENESIS_TIMESTAMP + 60, [56; 32], Vec::new(), None).unwrap();
        honest.header.tickets_root = merkle_root(
            &tickets
                .iter()
                .map(TicketTransaction::id)
                .collect::<Vec<_>>(),
        );
        honest.tickets = tickets;
        assert_eq!(check_body_commitments(&honest), Ok(()));

        let mut mangled = honest.clone();
        mangled.tickets.push(mangled.tickets[2].clone());
        let ids = mangled
            .tickets
            .iter()
            .map(TicketTransaction::id)
            .collect::<Vec<_>>();
        assert_eq!(merkle_root(&ids), honest.header.tickets_root);

        assert_eq!(
            check_body_commitments(&mangled),
            Err(ConsensusError::Invalid(
                "mining tickets are not in canonical order"
            ))
        );
    }

    #[test]
    fn a_repeated_transaction_tail_shares_the_root_but_not_the_block() {
        let transactions = sorted_by(
            (1..=3u64)
                .map(|value| Transaction {
                    version: PROTOCOL_VERSION,
                    inputs: Vec::new(),
                    outputs: vec![Output {
                        owner: [1; 32],
                        value,
                    }],
                })
                .collect(),
            Transaction::id,
        );
        let sidecar = Sidecar {
            header: CandidateHeader {
                version: PROTOCOL_VERSION,
                lottery_heartbeat: [0; 32],
                lottery_height: 0,
                parent_anchor: [0; 32],
                ticket_id: [0; 32],
                slot: 0,
                lottery_output: [0; 32],
                transactions_root: merkle_root(
                    &transactions.iter().map(Transaction::id).collect::<Vec<_>>(),
                ),
                signature: Vec::new(),
            },
            transactions,
        };
        let (_, genesis) = pinned_genesis().unwrap();
        let (mut honest, _) =
            mine_child(&genesis, GENESIS_TIMESTAMP + 60, [57; 32], Vec::new(), None).unwrap();
        honest.header.candidate_hash = Some(sidecar.header.hash());
        honest.sidecar = Some(sidecar);
        assert_eq!(check_body_commitments(&honest), Ok(()));

        let mut mangled = honest.clone();
        let body = mangled.sidecar.as_mut().unwrap();
        body.transactions.push(body.transactions[2].clone());

        assert_eq!(
            check_body_commitments(&mangled),
            Err(ConsensusError::Invalid(
                "Ledger transactions are not in canonical order"
            ))
        );
    }

    #[test]
    fn a_clock_stepping_back_does_not_fail_a_buffered_block() {
        let (mut chain, history) = eight_blocks();
        let mut branch = history[1].clone();
        for _ in 0..6 {
            branch = extend(&mut chain, &branch, 58);
        }
        let now = branch.tip_timestamp;
        let ahead = now + MAX_FUTURE_TIME_SECS;
        let (ahead_block, ahead_state) =
            mine_child(&branch, ahead, [58; 32], Vec::new(), None).unwrap();
        assert_eq!(chain.submit(ahead_block, now), Ok(false));

        let (child, state) =
            mine_child(&ahead_state, now + 61, [58; 32], Vec::new(), None).unwrap();

        assert_eq!(chain.submit(child, now - 30), Ok(true));
        assert_eq!(chain.tip().tip, state.tip);
    }

    #[test]
    fn the_old_tip_survives_a_rollback_of_a_deep_activation() {
        let mut chain = Chain::from_genesis().unwrap().with_state_retention(0, 4);
        let mut history = vec![chain.tip().clone()];
        for _ in 0..3 {
            let tip = chain.tip().clone();
            history.push(extend(&mut chain, &tip, 52));
        }
        let old_tip = history[3].tip;
        let mut branch = history[1].clone();
        for _ in 0..3 {
            branch = extend(&mut chain, &branch, 53);
        }
        assert_eq!(chain.tip().tip, branch.tip);

        chain.rollback_submission(branch.tip, old_tip);

        assert_eq!(chain.tip().tip, old_tip);
    }

    #[test]
    fn a_failed_activation_leaves_nothing_behind_to_build_on() {
        let (mut chain, history) = eight_blocks();
        let mut branch = history[1].clone();
        for _ in 0..7 {
            branch = extend(&mut chain, &branch, 54);
        }
        let side = solved(
            mine_child(
                &history[1],
                branch.tip_timestamp,
                [55; 32],
                Vec::new(),
                None,
            )
            .unwrap()
            .0,
        );
        let timestamp = side.header.timestamp;
        assert_eq!(chain.submit(side, timestamp), Ok(false));

        assert!(
            chain
                .submit(with_bad_root(&branch, 54), timestamp + 60)
                .is_err()
        );

        assert!(chain.deferred.is_empty());
        assert_eq!(chain.deferred_bytes, 0);
    }

    #[test]
    fn a_heavy_invalid_branch_is_refused_once_and_never_replayed_again() {
        let mut chain = Chain::from_genesis().unwrap().with_state_retention(2, 4);
        let mut history = vec![chain.tip().clone()];
        for _ in 0..8 {
            let tip = chain.tip().clone();
            history.push(extend(&mut chain, &tip, 45));
        }
        // Valid up to the block before the last, which only then tips the balance.
        let mut branch = history[1].clone();
        for _ in 0..7 {
            branch = extend(&mut chain, &branch, 46);
        }
        let bad = with_bad_root(&branch, 46);
        let timestamp = bad.header.timestamp;
        let tip = history[8].tip;
        assert!(chain.submit(bad.clone(), timestamp).is_err());
        assert_eq!(chain.tip().tip, tip);
        assert!(chain.deferred.is_empty());
        assert!(chain.states.values().all(|state| state.height != 2));

        assert_eq!(
            chain.submit(bad, timestamp),
            Err(ConsensusError::Invalid("block was already rejected"))
        );
    }

    #[test]
    fn a_full_deferred_buffer_empties_and_later_children_are_unknown() {
        let mut chain = Chain::from_genesis().unwrap().with_state_retention(2, 4);
        let mut history = vec![chain.tip().clone()];
        for _ in 0..8 {
            let tip = chain.tip().clone();
            history.push(extend(&mut chain, &tip, 47));
        }
        chain.deferred_bytes = MAX_DEFERRED_BYTES;
        let timestamp = history[1].tip_timestamp + HEARTBEAT_TARGET_INTERVAL_SECS;
        let (block, state) =
            mine_child(&history[1], timestamp, [48; 32], Vec::new(), None).unwrap();

        assert_eq!(chain.submit(block, timestamp), Ok(false));
        assert!(chain.deferred.is_empty());
        assert_eq!(chain.deferred_bytes, 0);

        let timestamp = state.tip_timestamp + HEARTBEAT_TARGET_INTERVAL_SECS;
        let (child, _) = mine_child(&state, timestamp, [48; 32], Vec::new(), None).unwrap();
        assert_eq!(
            chain.submit(child, timestamp),
            Err(ConsensusError::Invalid("unknown block parent"))
        );
    }

    /// Proves that a code path never gets as far as reading a checkpoint.
    #[derive(Debug)]
    struct UntouchableArchive;

    impl StateArchive for UntouchableArchive {
        fn put(&self, _: &ConsensusState) {}

        fn get(&self, _: &Hash) -> Option<ConsensusState> {
            panic!("the archive was read");
        }

        fn retain(&self, _: &dyn Fn(u64) -> bool) {}
    }

    #[test]
    fn a_wrong_target_is_rejected_from_the_headers_before_any_replay() {
        let mut chain = Chain::from_genesis()
            .unwrap()
            .with_state_archive(Arc::new(UntouchableArchive))
            .with_state_retention(2, 4);
        let mut history = vec![chain.tip().clone()];
        for _ in 0..8 {
            let tip = chain.tip().clone();
            history.push(extend(&mut chain, &tip, 40));
        }
        let parent = &history[4];
        let timestamp = parent.tip_timestamp + HEARTBEAT_TARGET_INTERVAL_SECS;
        let (mut block, _) = mine_child(parent, timestamp, [41; 32], Vec::new(), None).unwrap();
        block.header.target = [0xff; 32];
        block.header.target[..3].copy_from_slice(&[0, 0, 0x0f]);
        while check_proof_of_work(&block.header).is_err() {
            block.header.nonce += 1;
        }

        assert_eq!(
            chain.submit(block, timestamp),
            Err(ConsensusError::Invalid("incorrect ASERT target"))
        );
    }

    fn archived_chain(archive: &Arc<MemoryArchive>) -> (Chain, Vec<ConsensusState>) {
        let mut chain = Chain::from_genesis()
            .unwrap()
            .with_state_archive(archive.clone())
            .with_state_retention(2, 4);
        let mut history = vec![chain.tip().clone()];
        for _ in 0..8 {
            let tip = chain.tip().clone();
            history.push(extend(&mut chain, &tip, 37));
        }
        (chain, history)
    }

    #[test]
    fn checkpoints_leave_memory_for_the_archive_and_still_serve_old_forks() {
        let archive = Arc::new(MemoryArchive::default());
        let (mut chain, history) = archived_chain(&archive);

        let kept = chain
            .states
            .values()
            .map(|state| state.height)
            .collect::<BTreeSet<_>>();
        assert_eq!(kept, BTreeSet::from([0, 5, 6, 7, 8]));
        assert_eq!(
            archive.0.lock().unwrap().keys().collect::<Vec<_>>(),
            [&history[4].tip]
        );
        for state in &history {
            assert_eq!(chain.state(&state.tip).unwrap().as_ref(), state);
        }

        let mut branch = history[4].clone();
        for _ in 0..5 {
            branch = extend(&mut chain, &branch, 38);
        }
        assert_eq!(chain.tip().tip, branch.tip);
        assert!(chain.deferred.is_empty());
        assert_eq!(chain.deferred_bytes, 0);
    }

    #[test]
    fn a_lost_or_damaged_checkpoint_only_means_a_longer_replay() {
        let archive = Arc::new(MemoryArchive::default());
        let (chain, history) = archived_chain(&archive);
        let checkpoint = history[4].tip;

        archive
            .0
            .lock()
            .unwrap()
            .get_mut(&checkpoint)
            .unwrap()
            .minted += 1;
        assert_eq!(chain.state(&checkpoint).unwrap().as_ref(), &history[4]);

        archive.0.lock().unwrap().clear();
        assert_eq!(chain.state(&checkpoint).unwrap().as_ref(), &history[4]);
    }

    #[test]
    fn a_block_without_work_is_rejected_before_the_parent_is_looked_at() {
        let mut chain = Chain::from_genesis().unwrap();
        let timestamp = chain.tip().tip_timestamp + HEARTBEAT_TARGET_INTERVAL_SECS;
        let (mut block, _) =
            mine_child(chain.tip(), timestamp, [34; 32], Vec::new(), None).unwrap();
        block.header.parent = Some([7; 32]);
        while block.header.hash() <= block.header.target {
            block.header.nonce += 1;
        }
        // An unknown parent would report itself first if the work were checked later.
        assert_eq!(
            chain.submit(block.clone(), timestamp),
            Err(ConsensusError::Invalid(
                "proof of work does not meet target"
            ))
        );
        block.header.target = [0xff; 32];
        assert_eq!(
            chain.submit(block, timestamp),
            Err(ConsensusError::Invalid(
                "proof of work does not meet target"
            ))
        );
    }

    #[test]
    fn disconnected_blocks_are_listed_oldest_first() {
        let mut chain = Chain::from_genesis().unwrap();
        let genesis = chain.tip().clone();
        let left_one = extend(&mut chain, &genesis, 35);
        let left_two = extend(&mut chain, &left_one, 35);
        assert!(chain.blocks_off_active_chain(&left_two.tip).is_empty());

        let right_one = extend(&mut chain, &genesis, 36);
        let right_two = extend(&mut chain, &right_one, 36);
        let right_three = extend(&mut chain, &right_two, 36);
        assert_eq!(chain.tip().tip, right_three.tip);
        let heights = chain
            .blocks_off_active_chain(&left_two.tip)
            .iter()
            .map(|block| block.header.height)
            .collect::<Vec<_>>();
        assert_eq!(heights, [1, 2]);
    }

    #[test]
    fn known_blocks_are_not_validated_again() {
        let mut chain = Chain::from_genesis().unwrap();
        let timestamp = chain.tip().tip_timestamp + HEARTBEAT_TARGET_INTERVAL_SECS;
        let (block, _) = mine_child(chain.tip(), timestamp, [33; 32], Vec::new(), None).unwrap();
        assert!(chain.submit(block.clone(), timestamp).unwrap());
        // `now = 0` would fail the timestamp check if the block were validated again.
        assert!(!chain.submit(block, 0).unwrap());
    }

    #[test]
    fn tracked_ticket_capacity_frees_slots_completing_at_the_next_height() {
        let mut state = Chain::from_genesis().unwrap().tip().clone();
        state.height = 100;
        for index in 0..MAX_TRACKED_TICKETS as u64 {
            let mut tracked = ticket(1_000, 10, 100);
            tracked.id[..8].copy_from_slice(&index.to_be_bytes());
            state.tickets.insert(tracked.id, tracked);
        }
        assert_eq!(tracked_ticket_capacity(&state).unwrap(), 0);

        let first = *state.tickets.keys().next().unwrap();
        let completing = state.tickets.get_mut(&first).unwrap();
        completing.included_height = 101 + 1 - completing.lifecycle_length();
        assert_eq!(tracked_ticket_capacity(&state).unwrap(), 1);
    }

    #[test]
    fn transaction_fee_split_gives_the_odd_unit_to_the_winner() {
        assert_eq!(split_transaction_fees(0), (0, 0));
        assert_eq!(split_transaction_fees(1), (0, 1));
        assert_eq!(split_transaction_fees(7), (3, 4));
    }

    #[test]
    fn ticket_fee_requires_known_inputs_that_cover_budget_and_change() {
        let point = OutPoint {
            transaction_id: [1; 32],
            output_index: 0,
        };
        let mut ticket = TicketTransaction {
            version: PROTOCOL_VERSION,
            inputs: vec![TicketInput {
                previous_output: point.clone(),
                public_key: [2; 32],
                signature: vec![0; 64],
            }],
            budget: 70,
            duration: 10,
            mining_public_key: [3; 32],
            lottery_seed: [4; 32],
            change: Some(Output {
                owner: [2; 32],
                value: 20,
            }),
        };
        let mut utxos = BTreeMap::new();
        assert_eq!(ticket_fee(&utxos, &ticket), None);
        utxos.insert(
            point,
            Output {
                owner: [2; 32],
                value: 100,
            },
        );
        assert_eq!(ticket_fee(&utxos, &ticket), Some(10));
        ticket.budget = 90;
        assert_eq!(ticket_fee(&utxos, &ticket), None);
    }

    #[test]
    fn frozen_parameters_match_the_protocol() {
        assert_eq!(NETWORK_ID, "iuna-mainnet-candidate");
        assert_eq!(UNITS_PER_IUNA, 100_000_000);
        assert_eq!(MAX_MONEY, 4_207_680_100_000_000);
        assert_eq!(HEARTBEAT_TARGET_INTERVAL_SECS, 60);
        assert_eq!(LEDGER_TARGET_INTERVAL_HEARTBEATS, 10);
        assert_eq!(MAX_HEARTBEAT_BLOCK_BYTES, 16 * 1024);
        assert_eq!(MAX_LEDGER_SIDECAR_BYTES, 512 * 1024);
        assert_eq!(MAX_TRACKED_TICKETS, 4_096);
        assert_eq!(MAX_TICKET_DURATION, 1_440);
    }

    #[test]
    fn parallel_nonce_workers_mine_a_valid_heartbeat() {
        let (_, genesis) = pinned_genesis().unwrap();
        let timestamp = genesis
            .tip_timestamp
            .saturating_add(HEARTBEAT_TARGET_INTERVAL_SECS);
        let (block, state) =
            mine_child_with_workers(&genesis, timestamp, [42; 32], Vec::new(), None, 4).unwrap();

        assert!(block.hash() <= block.header.target);
        assert_eq!(state.height, 1);
        assert_eq!(state.tip, block.hash());
    }

    #[test]
    fn transaction_lists_require_strict_canonical_id_order() {
        assert!(ensure_canonical_order([[1; 32], [2; 32]], "order").is_ok());
        assert_eq!(
            ensure_canonical_order([[2; 32], [1; 32]], "order"),
            Err(ConsensusError::Invalid("order"))
        );
        assert_eq!(
            ensure_canonical_order([[1; 32], [1; 32]], "order"),
            Err(ConsensusError::Invalid("order"))
        );
    }

    #[test]
    fn expenditure_and_weight_distributions_are_exact() {
        for budget in [1, 2, 17, 1_000_001, MAX_MONEY] {
            for duration in [1, 2, 7, MAX_TICKET_DURATION] {
                let ticket = ticket(budget, duration, 10);
                let payout_sum = (0..ticket.lifecycle_length())
                    .map(|offset| ticket.payout_at(offset).unwrap())
                    .sum::<u64>();
                let weight_sum = (0..duration)
                    .map(|slot| ticket.weight_at(10 + TICKET_CONFIRMATIONS + slot).unwrap())
                    .sum::<u64>();
                assert_eq!(payout_sum, budget);
                assert_eq!(weight_sum, budget);
                assert_eq!(
                    ticket.remaining_after(ticket.lifecycle_length()).unwrap(),
                    0
                );
            }
        }
    }

    #[test]
    fn subsidy_respects_halvings_and_supply_cap() {
        assert_eq!(subsidy(0, GENESIS_REWARD).unwrap(), INITIAL_LEDGER_SUBSIDY);
        assert_eq!(
            subsidy(LEDGER_HALVING_INTERVAL, GENESIS_REWARD).unwrap(),
            INITIAL_LEDGER_SUBSIDY / 2
        );
        assert_eq!(
            subsidy(33 * LEDGER_HALVING_INTERVAL, MAX_MONEY - 7).unwrap(),
            7
        );
        assert_eq!(subsidy(34 * LEDGER_HALVING_INTERVAL, MAX_MONEY).unwrap(), 0);
    }

    #[test]
    fn capacity_price_rounds_up() {
        let duration = MAX_TICKET_DURATION;
        let expected = (INITIAL_LEDGER_SUBSIDY * (duration + 9))
            .div_ceil(LEDGER_TARGET_INTERVAL_HEARTBEATS * MAX_TRACKED_TICKETS as u64);
        assert_eq!(
            minimum_ticket_budget(duration, INITIAL_LEDGER_SUBSIDY).unwrap(),
            expected
        );
    }

    #[test]
    fn normalized_winner_comparison_is_exact() {
        let base = Winner {
            ticket_id: [2; 32],
            slot: 0,
            output: [0; 32],
            weight: 1,
            heartbeat: [3; 32],
            heartbeat_height: 1,
            parent_anchor: [4; 32],
            mining_public_key: [5; 32],
        };
        let mut left = base.clone();
        left.output[31] = 4;
        left.weight = 2;
        let mut right = base;
        right.ticket_id = [1; 32];
        right.output[31] = 6;
        right.weight = 2;
        assert_eq!(compare_winners(&left, &right), std::cmp::Ordering::Less);
    }

    #[test]
    fn block_work_is_exact_and_non_modular() {
        let work = block_work(&MAX_TARGET).unwrap();
        assert!(work > BigUint::zero());
        assert_eq!(
            block_work(&[0; 32]),
            Err(ConsensusError::Invalid("target out of range"))
        );
    }

    #[test]
    fn height_overflow_is_rejected_without_panicking() {
        let (_, mut state) = pinned_genesis().unwrap();
        state.height = u64::MAX;

        assert_eq!(expected_target(&state, 0), Err(ConsensusError::Arithmetic));
        assert_eq!(
            inclusion_winners(&state, 0),
            Err(ConsensusError::Arithmetic)
        );
        assert_eq!(
            mine_child(&state, state.tip_timestamp, [3; 32], Vec::new(), None),
            Err(ConsensusError::Arithmetic)
        );
    }

    #[test]
    fn mined_empty_chain_preserves_supply_and_chainwork() {
        let (genesis, genesis_state) = pinned_genesis().unwrap();
        assert_eq!(genesis.header.network, NETWORK_ID);
        assert_eq!(genesis_state.monetary_total().unwrap(), GENESIS_REWARD);
        let (_, child_state) = mine_child(
            &genesis_state,
            genesis.header.timestamp + HEARTBEAT_TARGET_INTERVAL_SECS,
            [43; 32],
            Vec::new(),
            None,
        )
        .unwrap();
        assert_eq!(child_state.height, 1);
        assert_eq!(child_state.minted, GENESIS_REWARD);
        assert_eq!(child_state.monetary_total().unwrap(), GENESIS_REWARD);
        assert!(child_state.chainwork > genesis_state.chainwork);
    }

    #[test]
    fn equal_work_keeps_tip_and_next_block_resolves_fork() {
        let mut chain = Chain::from_genesis().unwrap();
        let genesis = chain.tip().clone();
        let timestamp = GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS;
        let (left, _) = mine_child(&genesis, timestamp, [2; 32], Vec::new(), None).unwrap();
        let (right, right_state) =
            mine_child(&genesis, timestamp, [3; 32], Vec::new(), None).unwrap();
        assert!(chain.submit(left.clone(), timestamp).unwrap());
        assert!(!chain.submit(right, timestamp).unwrap());
        assert_eq!(chain.tip().tip, left.hash());

        let (right_child, _) = mine_child(
            &right_state,
            timestamp + HEARTBEAT_TARGET_INTERVAL_SECS,
            [3; 32],
            Vec::new(),
            None,
        )
        .unwrap();
        assert!(
            chain
                .submit(right_child, timestamp + HEARTBEAT_TARGET_INTERVAL_SECS)
                .unwrap()
        );
        assert_eq!(chain.tip().height, 2);
        assert_eq!(chain.canonical_blocks().len(), 3);
    }

    #[test]
    fn signed_ticket_locks_budget_and_pays_inclusion_slice() {
        let (_, genesis) = pinned_genesis().unwrap();
        let signing_key = SigningKey::from_bytes(&GENESIS_SECRET);
        assert_eq!(signing_key.verifying_key().to_bytes(), GENESIS_MINER);
        let genesis_point = genesis.utxos.keys().next().unwrap().clone();
        let budget = minimum_ticket_budget(1, INITIAL_LEDGER_SUBSIDY).unwrap();
        let fee = 1;
        let mut ticket = TicketTransaction {
            version: PROTOCOL_VERSION,
            inputs: vec![TicketInput {
                previous_output: genesis_point,
                public_key: GENESIS_MINER,
                signature: Vec::new(),
            }],
            budget,
            duration: 1,
            mining_public_key: [9; 32],
            lottery_seed: [10; 32],
            change: Some(Output {
                owner: address_from_public_key(&GENESIS_MINER),
                value: GENESIS_REWARD - budget - fee,
            }),
        };
        ticket.inputs[0].signature = signing_key
            .sign(&ticket.signing_digest(0, &[GENESIS_REWARD]).unwrap())
            .to_bytes()
            .to_vec();
        let (_, state) = mine_child(
            &genesis,
            GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS,
            [11; 32],
            vec![ticket],
            None,
        )
        .unwrap();
        assert_eq!(state.tickets.len(), 1);
        let tracked = state.tickets.values().next().unwrap();
        assert_eq!(
            tracked.remaining_after(1).unwrap(),
            budget - tracked.payout_at(0).unwrap()
        );
        assert_eq!(state.monetary_total().unwrap(), state.minted);
    }

    #[test]
    fn signature_domains_separate_payments_from_tickets() {
        let point = OutPoint {
            transaction_id: [3; 32],
            output_index: 4,
        };
        let transaction = Transaction {
            version: PROTOCOL_VERSION,
            inputs: vec![Input {
                previous_output: point.clone(),
                public_key: [5; 32],
                signature: Vec::new(),
            }],
            outputs: vec![Output {
                owner: [6; 32],
                value: 7,
            }],
        };
        let ticket = TicketTransaction {
            version: PROTOCOL_VERSION,
            inputs: vec![TicketInput {
                previous_output: point,
                public_key: [5; 32],
                signature: Vec::new(),
            }],
            budget: 7,
            duration: 1,
            mining_public_key: [6; 32],
            lottery_seed: [8; 32],
            change: None,
        };
        assert_ne!(
            transaction.signing_digest(0, &[7]).unwrap(),
            ticket.signing_digest(0, &[7]).unwrap()
        );
    }

    #[test]
    fn signatures_commit_to_the_network_and_the_spent_values() {
        let digest = |scope, values: &[Amount]| {
            signing_digest(b"IUNA-TX-SIGN-v1", scope, b"unsigned", values, 0).unwrap()
        };
        let scoped = digest(SigningScope::NETWORK, &[100]);
        assert_ne!(scoped, digest(SigningScope::NETWORK, &[10]));
        assert_ne!(
            scoped,
            digest(
                SigningScope {
                    network_id: "iuna-mainnet",
                    genesis_hash: &GENESIS_HASH,
                },
                &[100],
            )
        );
        assert_ne!(
            scoped,
            digest(
                SigningScope {
                    network_id: NETWORK_ID,
                    genesis_hash: &[0; 32],
                },
                &[100],
            )
        );
    }

    #[test]
    fn a_signature_over_a_wrong_input_value_is_rejected() {
        let (_, genesis) = pinned_genesis().unwrap();
        let signing_key = SigningKey::from_bytes(&GENESIS_SECRET);
        let point = genesis.utxos.keys().next().unwrap().clone();
        let mut transaction = Transaction {
            version: PROTOCOL_VERSION,
            inputs: vec![Input {
                previous_output: point,
                public_key: GENESIS_MINER,
                signature: Vec::new(),
            }],
            outputs: vec![Output {
                owner: [41; 32],
                value: 1,
            }],
        };
        let sign = |transaction: &mut Transaction, value| {
            transaction.inputs[0].signature = signing_key
                .sign(&transaction.signing_digest(0, &[value]).unwrap())
                .to_bytes()
                .to_vec();
        };

        // A lying node reported 2, so the wallet would pay 1 as fee and lose the rest.
        sign(&mut transaction, 2);
        assert!(validate_pending_transaction(&genesis, &transaction).is_err());

        sign(&mut transaction, GENESIS_REWARD);
        assert!(validate_pending_transaction(&genesis, &transaction).is_ok());

        let mut raw_key_owner = genesis.clone();
        raw_key_owner.utxos.values_mut().next().unwrap().owner = GENESIS_MINER;
        assert_eq!(
            validate_pending_transaction(&raw_key_owner, &transaction),
            Err(ConsensusError::Invalid("Ledger input owner mismatch"))
        );
    }

    #[test]
    fn deadline_winner_requires_the_exact_fallback_sidecar() {
        let mut tracked = ticket(MIN_TICKET_BUDGET, 2, 1);
        let deadline_heartbeat = Heartbeat {
            height: 7,
            hash: [12; 32],
            parent_anchor: LEDGER_GENESIS_ANCHOR,
        };
        let younger_heartbeat = Heartbeat {
            height: 8,
            hash: [15; 32],
            parent_anchor: LEDGER_GENESIS_ANCHOR,
        };
        let mut state = ConsensusState {
            height: 9,
            tip: [13; 32],
            tip_timestamp: GENESIS_TIMESTAMP + 90,
            tip_target: MAX_TARGET,
            genesis_timestamp: GENESIS_TIMESTAMP,
            utxos: BTreeMap::new(),
            tickets: BTreeMap::new(),
            heartbeats: vec![deadline_heartbeat.clone(), younger_heartbeat.clone()],
            ledger_parent_anchor: LEDGER_GENESIS_ANCHOR,
            ledger_blocks: 0,
            minted: 0,
            chainwork: BigUint::zero(),
            recent_timestamps: vec![GENESIS_TIMESTAMP + 90],
        };
        let (winner, younger_winner) = (0u32..10_000)
            .find_map(|seed| {
                tracked.lottery_seed[28..].copy_from_slice(&seed.to_be_bytes());
                state.tickets = BTreeMap::from([(tracked.id, tracked.clone())]);
                let winner = lottery_winners(&state, &deadline_heartbeat)
                    .unwrap()
                    .into_iter()
                    .next()?;
                let younger = lottery_winners(&state, &younger_heartbeat)
                    .unwrap()
                    .into_iter()
                    .next()?;
                Some((winner, younger))
            })
            .expect("one ticket wins two consecutive lotteries");
        let header = HeartbeatHeader {
            protocol_version: PROTOCOL_VERSION,
            network: NETWORK_ID.to_owned(),
            height: 10,
            parent: Some(state.tip),
            timestamp: GENESIS_TIMESTAMP + 100,
            target: MAX_TARGET,
            nonce: 0,
            miner_public_key: [14; 32],
            tickets_root: merkle_root(&[]),
            candidate_hash: None,
            post_state_root: ZERO_HASH,
            payout_commitment: ZERO_HASH,
        };
        let missing = HeartbeatBlock {
            header: header.clone(),
            tickets: Vec::new(),
            sidecar: None,
        };
        assert_eq!(
            validate_sidecar_selection(&state, &missing),
            Err(ConsensusError::Invalid(
                "deadline winner requires a sidecar"
            ))
        );

        let sidecar_for = |winner: &Winner| Sidecar {
            header: CandidateHeader {
                version: PROTOCOL_VERSION,
                lottery_heartbeat: winner.heartbeat,
                lottery_height: winner.heartbeat_height,
                parent_anchor: winner.parent_anchor,
                ticket_id: winner.ticket_id,
                slot: winner.slot,
                lottery_output: winner.output,
                transactions_root: merkle_root(&[]),
                signature: Vec::new(),
            },
            transactions: Vec::new(),
        };
        let younger_sidecar = sidecar_for(&younger_winner);
        let mut younger_header = header.clone();
        younger_header.candidate_hash = Some(younger_sidecar.header.hash());
        let wrong_winner = HeartbeatBlock {
            header: younger_header,
            tickets: Vec::new(),
            sidecar: Some(younger_sidecar),
        };
        assert_eq!(
            validate_sidecar_selection(&state, &wrong_winner),
            Err(ConsensusError::Invalid(
                "candidate does not use mandatory deadline winner"
            ))
        );

        let sidecar = sidecar_for(&winner);
        let mut included_header = header;
        included_header.candidate_hash = Some(sidecar.header.hash());
        let included = HeartbeatBlock {
            header: included_header,
            tickets: Vec::new(),
            sidecar: Some(sidecar),
        };
        assert_eq!(
            validate_sidecar_selection(&state, &included),
            Ok(Some(winner))
        );

        let mut signed_fallback = included;
        let sidecar = signed_fallback.sidecar.as_mut().unwrap();
        sidecar.header.signature = vec![0; 64];
        signed_fallback.header.candidate_hash = Some(sidecar.header.hash());
        assert_eq!(
            validate_sidecar_selection(&state, &signed_fallback),
            Err(ConsensusError::Invalid(
                "deadline fallback must be empty and unsigned"
            ))
        );
    }

    #[test]
    fn nonempty_candidate_requires_the_winning_ticket_signature_before_deadline() {
        let candidate_key = SigningKey::from_bytes(&[27; 32]);
        let spending_key = SigningKey::from_bytes(&GENESIS_SECRET);
        let point = OutPoint {
            transaction_id: [31; 32],
            output_index: 0,
        };
        let mut transaction = Transaction {
            version: PROTOCOL_VERSION,
            inputs: vec![Input {
                previous_output: point.clone(),
                public_key: spending_key.verifying_key().to_bytes(),
                signature: Vec::new(),
            }],
            outputs: vec![Output {
                owner: [32; 32],
                value: 900,
            }],
        };
        transaction.inputs[0].signature = spending_key
            .sign(&transaction.signing_digest(0, &[1_000]).unwrap())
            .to_bytes()
            .to_vec();

        let lottery_heartbeat = Heartbeat {
            height: 7,
            hash: [33; 32],
            parent_anchor: LEDGER_GENESIS_ANCHOR,
        };
        let mut tracked = ticket(MIN_TICKET_BUDGET, 2, 1);
        tracked.mining_public_key = candidate_key.verifying_key().to_bytes();
        let mut state = ConsensusState {
            height: 8,
            tip: [34; 32],
            tip_timestamp: GENESIS_TIMESTAMP + 80,
            tip_target: MAX_TARGET,
            genesis_timestamp: GENESIS_TIMESTAMP,
            utxos: BTreeMap::from([(
                point,
                Output {
                    owner: address_from_public_key(&spending_key.verifying_key().to_bytes()),
                    value: 1_000,
                },
            )]),
            tickets: BTreeMap::new(),
            heartbeats: vec![lottery_heartbeat.clone()],
            ledger_parent_anchor: LEDGER_GENESIS_ANCHOR,
            ledger_blocks: 0,
            minted: 1_000 + tracked.budget,
            chainwork: BigUint::zero(),
            recent_timestamps: vec![GENESIS_TIMESTAMP + 80],
        };
        let winner = (0u32..10_000)
            .find_map(|seed| {
                tracked.lottery_seed[28..].copy_from_slice(&seed.to_be_bytes());
                state.tickets = BTreeMap::from([(tracked.id, tracked.clone())]);
                lottery_winners(&state, &lottery_heartbeat)
                    .unwrap()
                    .into_iter()
                    .next()
            })
            .expect("the ticket should win a pre-deadline lottery");
        let mut sidecar = Sidecar {
            header: CandidateHeader {
                version: PROTOCOL_VERSION,
                lottery_heartbeat: winner.heartbeat,
                lottery_height: winner.heartbeat_height,
                parent_anchor: winner.parent_anchor,
                ticket_id: winner.ticket_id,
                slot: winner.slot,
                lottery_output: winner.output,
                transactions_root: merkle_root(&[transaction.id()]),
                signature: Vec::new(),
            },
            transactions: vec![transaction],
        };

        assert!(validate_pending_candidate(&state, &sidecar).is_err());
        sidecar.header.signature = candidate_key
            .sign(&sidecar.header.signing_digest())
            .to_bytes()
            .to_vec();
        assert_eq!(validate_pending_candidate(&state, &sidecar), Ok(100));

        sidecar.header.signature[0] ^= 1;
        assert!(validate_pending_candidate(&state, &sidecar).is_err());
    }

    /// The browser wallet signs Ledger transactions itself; this vector is generated by
    /// `wallet/wallet-core.js` and must stay byte-for-byte valid for the Rust validator.
    #[test]
    fn browser_wallet_vector_matches_rust_signing_and_verification() {
        let vector: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/vectors/browser_wallet_transaction.json"
        ))
        .unwrap();
        let transaction: Transaction =
            serde_json::from_value(vector["transaction"].clone()).unwrap();
        let signing_key = SigningKey::from_bytes(&[0; 32]);
        let public_key = signing_key.verifying_key().to_bytes();
        let hex = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        assert_eq!(vector["public_key_hex"], hex(&public_key));
        assert_eq!(
            vector["address_hex"],
            hex(&address_from_public_key(&public_key))
        );
        assert_eq!(transaction.version, PROTOCOL_VERSION);
        let values = serde_json::from_value::<Vec<Amount>>(vector["input_values"].clone()).unwrap();
        let genesis: Hash = (0..32)
            .map(|index| {
                let digits = &vector["genesis_hash"].as_str().unwrap()[index * 2..index * 2 + 2];
                u8::from_str_radix(digits, 16).unwrap()
            })
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        for (index, input) in transaction.inputs.iter().enumerate() {
            // Test builds use a fast genesis; the wallet signs for the pinned network.
            let digest = signing_digest(
                b"IUNA-TX-SIGN-v1",
                SigningScope {
                    network_id: NETWORK_ID,
                    genesis_hash: &genesis,
                },
                &transaction.canonical(false),
                &values,
                index,
            )
            .unwrap();
            assert_eq!(vector["signing_digests"][index], hex(&digest));
            assert_eq!(input.public_key, public_key);
            verify_signature(&input.public_key, &digest, &input.signature).unwrap();
        }
    }

    fn target_after_solvetime(parent_target: &BigUint, solvetime: u64) -> BigUint {
        let (_, mut state) = pinned_genesis().unwrap();
        state.height = 10;
        state.tip_target = biguint_hash(parent_target).unwrap();
        state.recent_timestamps = vec![GENESIS_TIMESTAMP, GENESIS_TIMESTAMP + solvetime];
        state.tip_timestamp = GENESIS_TIMESTAMP + solvetime;
        BigUint::from_bytes_be(&expected_target(&state, 11).unwrap())
    }

    #[test]
    fn relative_asert_steers_each_target_towards_sixty_second_heartbeats() {
        let parent = BigUint::from_bytes_be(&MAX_TARGET) >> 20usize;

        assert_eq!(target_after_solvetime(&parent, 60), parent);
        assert!(target_after_solvetime(&parent, 1) < parent);
        assert!(target_after_solvetime(&parent, 600) > parent);
        // One half-life of excess solvetime doubles the target, one half-life short halves it.
        let half_life = ASERT_HALF_LIFE_SECS as u64;
        assert_eq!(
            target_after_solvetime(&parent, 60 + half_life),
            &parent * 2u8
        );
        let (_, mut state) = pinned_genesis().unwrap();
        state.height = 10;
        state.tip_target = biguint_hash(&parent).unwrap();
        state.recent_timestamps = vec![GENESIS_TIMESTAMP + half_life, GENESIS_TIMESTAMP + 60];
        assert_eq!(
            BigUint::from_bytes_be(&expected_target(&state, 11).unwrap()),
            &parent >> 1usize
        );
        // Long idle periods only relax the target to the maximum; they create no backlog.
        assert_eq!(
            target_after_solvetime(&parent, 30 * 24 * 60 * 60),
            BigUint::from_bytes_be(&MAX_TARGET)
        );
    }

    #[test]
    fn block_one_keeps_the_genesis_target() {
        let (_, genesis) = pinned_genesis().unwrap();
        assert_eq!(expected_target(&genesis, 1).unwrap(), genesis.tip_target);
    }

    #[test]
    fn nonce_search_stops_when_requested() {
        let (_, mut genesis) = pinned_genesis().unwrap();
        // An unreachable target forces the search to run until it is stopped.
        genesis.tip_target = biguint_hash(&BigUint::one()).unwrap();
        let stop = AtomicBool::new(true);
        let mined = mine_child_until(
            &genesis,
            genesis.tip_timestamp + 60,
            [3; 32],
            Vec::new(),
            None,
            2,
            &stop,
        )
        .unwrap();
        assert!(mined.is_none());
    }

    #[test]
    fn counted_nonce_search_reports_actual_attempts() {
        let (_, genesis) = pinned_genesis().unwrap();
        let stop = AtomicBool::new(false);
        let hashes = AtomicU64::new(0);
        let mined = mine_child_until_counted(
            &genesis,
            genesis.tip_timestamp + 60,
            [3; 32],
            Vec::new(),
            None,
            2,
            &stop,
            &hashes,
        )
        .unwrap();
        assert!(mined.is_some());
        assert!(hashes.load(Ordering::Relaxed) > 0);
    }
}
