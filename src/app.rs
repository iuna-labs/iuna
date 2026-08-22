use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use tokio::sync::Mutex;

use crate::domain::{
    Amount, BurnBundle, Ledger, MINE_ACTIONS_PER_ANCHOR_LIMIT, PreparedBlock, Transaction, run_vdf,
};

mod automatic_mining;
mod gossip;
mod helpers;
mod in_memory_network;
mod ledger_view;
mod node_lifecycle;
mod peer_book;
mod receive;
mod status;
mod types;
mod wallet;
pub use in_memory_network::InMemoryNetwork;
pub use peer_book::{PeerBook, PeerDirection, PeerInfo};
pub use types::{
    AutoMineOutcome, AutoMinePlan, BlockInventory, ExternalMineJob, FeeEstimate, GossipEnvelope,
    LaunchProfileStatus, MiningStatus, NodeConfig, NodeStatus, ProtocolHello, StratumStatus,
};
use wallet::NodeWallet;

pub type SharedNode = Arc<Mutex<NodeCore>>;
pub type SharedPeerBook = Arc<Mutex<PeerBook>>;

pub const DEFAULT_BURN_PER_BLOCK: Amount = 0;
pub const DEFAULT_VDF_ROUNDS: u32 = 67_000_000;
pub const PROTOCOL_VERSION: u32 = 2;
pub const MAINNET_CANDIDATE_NETWORK_ID: &str = "iuna-mainnet-candidate-v1";
pub const MAINNET_NETWORK_ID: &str = "iuna-mainnet-v1";
pub const NETWORK_ID: &str = MAINNET_CANDIDATE_NETWORK_ID;
pub const BLOCK_REQUEST_LIMIT: usize = 128;
pub const TRANSACTION_BATCH_LIMIT: usize = 128;
const IMPORT_REBROADCAST_LIMIT: usize = 128;
pub const PEER_MISBEHAVIOR_BAN_SCORE: u32 = 3;
pub const PEER_MISBEHAVIOR_BAN_MS: u64 = 10 * 60 * 1_000;
pub const PEER_CLOCK_OFFSET_ACCEPTANCE_MS: i64 = 10 * 60 * 1_000;
const PEER_CLOCK_OFFSET_STALE_MS: u64 = 20 * 60 * 1_000;
const AUTO_POW_NONCE_ATTEMPTS_PER_WORKER_TICK: u64 = 100_000;
const AUTO_PLAINTEXT_BURN_BEFORE_RECOVERY_MS: u64 = 60_000;
const BURN_BUNDLE_COLLECTION_MS: u64 = 30_000;
const MIN_AUTO_BLOCK_ANCHOR_BURN_AMOUNT: Amount = 1;
const AUTO_BLOCK_ANCHOR_BURN_FEE: Amount = 1;
static DEBUG_LOGGING: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
mod tests {
    use super::{
        BLOCK_REQUEST_LIMIT, DEFAULT_VDF_ROUNDS, MAINNET_CANDIDATE_NETWORK_ID, MAINNET_NETWORK_ID,
        NETWORK_ID, PROTOCOL_VERSION, TRANSACTION_BATCH_LIMIT,
    };

    #[test]
    fn mainnet_candidate_network_parameters_are_frozen() {
        assert_eq!(DEFAULT_VDF_ROUNDS, 67_000_000);
        assert_eq!(PROTOCOL_VERSION, 2);
        assert_eq!(MAINNET_CANDIDATE_NETWORK_ID, "iuna-mainnet-candidate-v1");
        assert_eq!(MAINNET_NETWORK_ID, "iuna-mainnet-v1");
        assert_ne!(MAINNET_CANDIDATE_NETWORK_ID, MAINNET_NETWORK_ID);
        assert_eq!(NETWORK_ID, MAINNET_CANDIDATE_NETWORK_ID);
        assert_eq!(BLOCK_REQUEST_LIMIT, 128);
        assert_eq!(TRANSACTION_BATCH_LIMIT, 128);
    }
}

pub fn set_debug_logging(enabled: bool) {
    DEBUG_LOGGING.store(enabled, Ordering::Relaxed);
}

pub fn debug_logging_enabled() -> bool {
    DEBUG_LOGGING.load(Ordering::Relaxed)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AutoPowMineCursor {
    anchor: String,
    salt: u64,
    next_nonce: u64,
    searched: u64,
}

#[derive(Clone, Debug)]
pub struct AutoPowMineJob {
    ledger: Ledger,
    recipient: String,
    anchor: String,
    salt: u64,
    start_nonce: u64,
    max_attempts: u64,
}

impl AutoPowMineJob {
    pub fn anchor(&self) -> &str {
        &self.anchor
    }

    pub fn search(self) -> Result<(Self, crate::domain::MineSearchOutcome)> {
        let outcome = self.ledger.search_mine(
            self.recipient.clone(),
            self.salt,
            self.start_nonce,
            self.max_attempts,
        )?;
        Ok((self, outcome))
    }
}

#[derive(Clone, Debug)]
pub struct NodeCore {
    wallet: NodeWallet,
    ledger: Ledger,
    automatic_mining_enabled: bool,
    pow_mining_enabled: bool,
    pow_mining_workers: u8,
    burn_per_block: Amount,
    burn_fee: Amount,
    recovery_vdf_top_rank_percent: u8,
    last_auto_burn_height: Option<u64>,
    last_auto_anchor_burn_height: Option<u64>,
    last_auto_finalization_status: Option<String>,
    last_auto_pow_mine_anchor: Option<String>,
    last_auto_pow_mine_status: Option<String>,
    auto_pow_mine_cursor: Option<AutoPowMineCursor>,
    burn_bundles: BTreeMap<(u64, u8, String), BurnBundle>,
    equivocated_burn_bundle_slots: BTreeSet<(u64, u8, String)>,
    burn_bundle_collection_started: Option<(u64, u64)>,
    local_block_anchor_burn: Option<(u64, Transaction)>,
    outbox: Vec<GossipEnvelope>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time is before unix epoch")
        .as_millis() as u64
}
