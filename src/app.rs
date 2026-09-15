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
mod consolidation;
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
    AutoMineOutcome, AutoMinePlan, BlockInventory, ChainBootstrap, ExternalMineJob, FeeEstimate,
    GossipEnvelope, LaunchProfileStatus, MiningStatus, NetworkMigrationStatus, NodeConfig,
    NodeStatus, ProtocolHello, StratumStatus,
};
use wallet::NodeWallet;

pub type SharedNode = Arc<Mutex<NodeCore>>;
pub type SharedPeerBook = Arc<Mutex<PeerBook>>;

pub const DEFAULT_BURN_PER_BLOCK: Amount = 0;
pub const DEFAULT_VDF_ROUNDS: u32 = 67_000_000;
pub const PROTOCOL_VERSION: u32 = 2;
pub const MAX_PROTOCOL_CAPABILITIES: usize = 16;
pub const MAX_PROTOCOL_CAPABILITY_BYTES: usize = 64;
pub const CAPABILITY_ADDRESS_V1_READ: &str = "address-v1-read";
pub const CAPABILITY_SIGNATURE_SCHEMES_V1: &str = "signature-schemes-v1";
pub const CAPABILITY_TRANSACTION_V2_BLOCKS: &str = "transaction-v2-blocks";
pub const MAINNET_CANDIDATE_NETWORK_ID: &str = "iuna-mainnet-candidate";
pub const MAINNET_CANDIDATE_GENESIS_HASH: &str =
    "3d677cd7ced1c04d3a276cbee7ea38076e34ac65f18a2c9b8286a4872d986a9a";
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
const AUTO_PLAINTEXT_BURN_BEFORE_RECOVERY_MS: u64 = crate::domain::VDF_TARGET_BLOCK_MS / 10;
const BURN_BUNDLE_COLLECTION_MS: u64 = crate::domain::VDF_TARGET_BLOCK_MS / 20;
const MIN_AUTO_BLOCK_ANCHOR_BURN_AMOUNT: Amount = 1;
static DEBUG_LOGGING: AtomicBool = AtomicBool::new(false);

pub fn protocol_capabilities() -> Vec<String> {
    vec![
        CAPABILITY_ADDRESS_V1_READ.to_string(),
        CAPABILITY_SIGNATURE_SCHEMES_V1.to_string(),
        CAPABILITY_TRANSACTION_V2_BLOCKS.to_string(),
    ]
}

pub fn validate_protocol_capabilities(capabilities: &[String]) -> Result<()> {
    if capabilities.len() > MAX_PROTOCOL_CAPABILITIES {
        anyhow::bail!("peer advertises too many protocol capabilities");
    }
    let mut previous: Option<&str> = None;
    for capability in capabilities {
        if capability.is_empty()
            || capability.len() > MAX_PROTOCOL_CAPABILITY_BYTES
            || !capability
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            anyhow::bail!("peer advertises an invalid protocol capability");
        }
        if previous.is_some_and(|previous| previous >= capability.as_str()) {
            anyhow::bail!("peer protocol capabilities must be sorted and unique");
        }
        previous = Some(capability);
    }
    Ok(())
}

pub fn validate_transaction_v2_peer_capability(
    capabilities: &[String],
    local_height: u64,
    remote_height: u64,
) -> Result<()> {
    let activation_is_next =
        crate::domain::transaction_v2_is_active(local_height.max(remote_height).saturating_add(1));
    if activation_is_next
        && !capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_TRANSACTION_V2_BLOCKS)
    {
        anyhow::bail!("peer lacks transaction-v2 block capability near activation");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        BLOCK_REQUEST_LIMIT, CAPABILITY_ADDRESS_V1_READ, CAPABILITY_SIGNATURE_SCHEMES_V1,
        CAPABILITY_TRANSACTION_V2_BLOCKS, DEFAULT_VDF_ROUNDS, MAINNET_CANDIDATE_GENESIS_HASH,
        MAINNET_CANDIDATE_NETWORK_ID, MAINNET_NETWORK_ID, MAX_PROTOCOL_CAPABILITIES, NETWORK_ID,
        PROTOCOL_VERSION, TRANSACTION_BATCH_LIMIT, protocol_capabilities, validate_network_genesis,
        validate_protocol_capabilities, validate_transaction_v2_peer_capability,
    };

    #[test]
    fn mainnet_candidate_network_parameters_are_frozen() {
        assert_eq!(DEFAULT_VDF_ROUNDS, 67_000_000);
        assert_eq!(PROTOCOL_VERSION, 2);
        assert_eq!(MAINNET_CANDIDATE_NETWORK_ID, "iuna-mainnet-candidate");
        assert_eq!(MAINNET_CANDIDATE_GENESIS_HASH.len(), 64);
        assert_eq!(MAINNET_NETWORK_ID, "iuna-mainnet-v1");
        assert_ne!(MAINNET_CANDIDATE_NETWORK_ID, MAINNET_NETWORK_ID);
        assert_eq!(NETWORK_ID, MAINNET_CANDIDATE_NETWORK_ID);
        assert_eq!(BLOCK_REQUEST_LIMIT, 128);
        assert_eq!(TRANSACTION_BATCH_LIMIT, 128);
    }

    #[test]
    fn candidate_genesis_is_pinned_while_local_profiles_remain_unpinned() {
        assert!(
            validate_network_genesis(MAINNET_CANDIDATE_NETWORK_ID, MAINNET_CANDIDATE_GENESIS_HASH)
                .is_ok()
        );
        assert!(validate_network_genesis(MAINNET_CANDIDATE_NETWORK_ID, &"0".repeat(64)).is_err());
        assert!(validate_network_genesis("iuna-local-testnet-v1", &"0".repeat(64)).is_ok());
    }

    #[test]
    fn current_protocol_capabilities_are_stable_and_valid() {
        let capabilities = protocol_capabilities();
        assert_eq!(
            capabilities,
            [
                CAPABILITY_ADDRESS_V1_READ,
                CAPABILITY_SIGNATURE_SCHEMES_V1,
                CAPABILITY_TRANSACTION_V2_BLOCKS,
            ]
        );
        validate_protocol_capabilities(&capabilities).unwrap();
        validate_protocol_capabilities(&[]).unwrap();
    }

    #[test]
    fn malformed_protocol_capabilities_fail_closed() {
        assert!(validate_protocol_capabilities(&["UPPERCASE".to_string()]).is_err());
        assert!(
            validate_protocol_capabilities(&["duplicate".to_string(), "duplicate".to_string()])
                .is_err()
        );
        assert!(
            validate_protocol_capabilities(&["z-last".to_string(), "a-first".to_string()]).is_err()
        );
        assert!(
            validate_protocol_capabilities(&vec![
                "capability".to_string();
                MAX_PROTOCOL_CAPABILITIES + 1
            ])
            .is_err()
        );
    }

    #[test]
    fn transaction_v2_block_capability_is_required_when_activation_is_next() {
        assert!(validate_transaction_v2_peer_capability(&[], 2_998, 2_998).is_ok());
        assert!(validate_transaction_v2_peer_capability(&[], 2_999, 2_998).is_err());
        assert!(
            validate_transaction_v2_peer_capability(
                &[CAPABILITY_TRANSACTION_V2_BLOCKS.to_string()],
                2_999,
                2_998,
            )
            .is_ok()
        );
    }
}

pub fn validate_network_genesis(profile_id: &str, genesis_hash: &str) -> Result<()> {
    if profile_id == MAINNET_CANDIDATE_NETWORK_ID && genesis_hash != MAINNET_CANDIDATE_GENESIS_HASH
    {
        anyhow::bail!(
            "mainnet-candidate genesis {genesis_hash} does not match pinned genesis {MAINNET_CANDIDATE_GENESIS_HASH}"
        );
    }
    Ok(())
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
    network_migration_from: Option<String>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time is before unix epoch")
        .as_millis() as u64
}
