use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use tokio::sync::Mutex;

use crate::domain::{
    Amount, BurnBundle, Ledger, MINE_ACTIONS_PER_ANCHOR_LIMIT, PreparedBlock, Transaction,
    run_vdf_with_memory_limit,
};

mod automatic_mining;
mod consolidation;
#[cfg(test)]
pub(crate) use consolidation::ConsolidationKind;
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
use types::VdfSpeedSample;
pub use types::{
    AutoMineOutcome, AutoMinePlan, BlockInventory, ChainBootstrap, ChainSegmentSummary,
    ExternalMineJob, FeeEstimate, FundedWalletAddressStatus, GossipEnvelope, LaunchProfileStatus,
    MiningStatus, NetworkMigrationStatus, NodeConfig, NodeStatus, ProtocolHello,
    QuantumMigrationPreview, QuantumMigrationStatus, StratumStatus, VdfSpeedSource,
};
use wallet::NodeWallet;

pub type SharedNode = Arc<Mutex<NodeCore>>;
pub type SharedPeerBook = Arc<Mutex<PeerBook>>;

pub const DEFAULT_BURN_PER_BLOCK: Amount = 0;
pub const DEFAULT_VDF_ROUNDS: u32 = 67_000_000;
pub const PROTOCOL_VERSION: u32 = 2;
pub const MAX_PROTOCOL_CAPABILITIES: usize = 16;
pub const MAX_PROTOCOL_CAPABILITY_BYTES: usize = 64;
const MAX_GOSSIP_OUTBOX_BYTES: usize = 16 * 1024 * 1024;
const MAX_GOSSIP_OUTBOX_ENTRIES: usize = 1_024;
pub const CAPABILITY_ADDRESS_V1_READ: &str = "address-v1-read";
pub const CAPABILITY_CHAIN_SEGMENT_SYNC: &str = "chain-segment-sync";
pub const CAPABILITY_HYBRID_LINEAGE_IDENTITIES: &str = "hybrid-lineage-identities";
pub const CAPABILITY_HYBRID_REWARD_PAYOUTS: &str = "hybrid-reward-payouts";
pub const CAPABILITY_SIGNATURE_SCHEMES_V1: &str = "signature-schemes-v1";
pub const CAPABILITY_TRANSACTION_V2_AGGREGATED_AUTHORIZATIONS: &str =
    "transaction-v2-aggregated-authorizations";
pub const CAPABILITY_TRANSACTION_V2_BLOCKS: &str = "transaction-v2-blocks";
pub const CAPABILITY_TRANSACTION_V2_BURNS: &str = "transaction-v2-burns";
pub const CAPABILITY_TRANSACTION_V2_MEMPOOL: &str = "transaction-v2-mempool";
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
pub const PEER_GOOD_CONNECTION_MAX_AGE_MS: u64 = 20 * 60 * 1_000;
const AUTO_POW_NONCE_ATTEMPTS_PER_WORKER_TICK: u64 = 100_000;
const AUTO_PLAINTEXT_BURN_BEFORE_RECOVERY_MS: u64 = crate::domain::VDF_TARGET_BLOCK_MS / 10;
const BURN_BUNDLE_COLLECTION_MS: u64 = crate::domain::VDF_TARGET_BLOCK_MS / 20;
const MIN_AUTO_BLOCK_ANCHOR_BURN_AMOUNT: Amount = 1;
static DEBUG_LOGGING: AtomicBool = AtomicBool::new(false);

pub fn protocol_capabilities() -> Vec<String> {
    vec![
        CAPABILITY_ADDRESS_V1_READ.to_string(),
        CAPABILITY_CHAIN_SEGMENT_SYNC.to_string(),
        CAPABILITY_HYBRID_LINEAGE_IDENTITIES.to_string(),
        CAPABILITY_HYBRID_REWARD_PAYOUTS.to_string(),
        CAPABILITY_SIGNATURE_SCHEMES_V1.to_string(),
        CAPABILITY_TRANSACTION_V2_AGGREGATED_AUTHORIZATIONS.to_string(),
        CAPABILITY_TRANSACTION_V2_BLOCKS.to_string(),
        CAPABILITY_TRANSACTION_V2_BURNS.to_string(),
        CAPABILITY_TRANSACTION_V2_MEMPOOL.to_string(),
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
    if activation_is_next
        && !capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_TRANSACTION_V2_BURNS)
    {
        anyhow::bail!("peer lacks transaction-v2 burn capability near activation");
    }
    let hybrid_rewards_are_next = local_height.max(remote_height).saturating_add(1)
        >= crate::domain::HYBRID_REWARD_ACTIVATION_HEIGHT;
    if hybrid_rewards_are_next
        && !capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_HYBRID_REWARD_PAYOUTS)
    {
        anyhow::bail!("peer lacks hybrid reward payout capability near activation");
    }
    let hybrid_lineage_identities_are_next = local_height.max(remote_height).saturating_add(1)
        >= crate::domain::HYBRID_LINEAGE_IDENTITY_ACTIVATION_HEIGHT;
    if hybrid_lineage_identities_are_next
        && !capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_HYBRID_LINEAGE_IDENTITIES)
    {
        anyhow::bail!("peer lacks hybrid lineage identity capability near activation");
    }
    let aggregated_authorizations_are_next = local_height.max(remote_height).saturating_add(1)
        >= crate::domain::TRANSACTION_V2_AUTHORIZATION_AGGREGATION_ACTIVATION_HEIGHT;
    if aggregated_authorizations_are_next
        && !capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_TRANSACTION_V2_AGGREGATED_AUTHORIZATIONS)
    {
        anyhow::bail!(
            "peer lacks transaction-v2 aggregated authorization capability near activation"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        BLOCK_REQUEST_LIMIT, CAPABILITY_ADDRESS_V1_READ, CAPABILITY_CHAIN_SEGMENT_SYNC,
        CAPABILITY_HYBRID_LINEAGE_IDENTITIES, CAPABILITY_HYBRID_REWARD_PAYOUTS,
        CAPABILITY_SIGNATURE_SCHEMES_V1, CAPABILITY_TRANSACTION_V2_AGGREGATED_AUTHORIZATIONS,
        CAPABILITY_TRANSACTION_V2_BLOCKS, CAPABILITY_TRANSACTION_V2_BURNS,
        CAPABILITY_TRANSACTION_V2_MEMPOOL, DEFAULT_VDF_ROUNDS, MAINNET_CANDIDATE_GENESIS_HASH,
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
                CAPABILITY_CHAIN_SEGMENT_SYNC,
                CAPABILITY_HYBRID_LINEAGE_IDENTITIES,
                CAPABILITY_HYBRID_REWARD_PAYOUTS,
                CAPABILITY_SIGNATURE_SCHEMES_V1,
                CAPABILITY_TRANSACTION_V2_AGGREGATED_AUTHORIZATIONS,
                CAPABILITY_TRANSACTION_V2_BLOCKS,
                CAPABILITY_TRANSACTION_V2_BURNS,
                CAPABILITY_TRANSACTION_V2_MEMPOOL,
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
                &[
                    CAPABILITY_TRANSACTION_V2_BLOCKS.to_string(),
                    CAPABILITY_TRANSACTION_V2_BURNS.to_string(),
                ],
                2_999,
                2_998,
            )
            .is_ok()
        );
    }

    #[test]
    fn hybrid_reward_capability_is_required_when_activation_is_next() {
        let v2 = CAPABILITY_TRANSACTION_V2_BLOCKS.to_string();
        let burns = CAPABILITY_TRANSACTION_V2_BURNS.to_string();
        assert!(
            validate_transaction_v2_peer_capability(&[v2.clone(), burns.clone()], 3_748, 3_748)
                .is_ok()
        );
        assert!(
            validate_transaction_v2_peer_capability(&[v2.clone(), burns.clone()], 3_749, 3_748)
                .is_err()
        );
        assert!(
            validate_transaction_v2_peer_capability(
                &[v2, burns, CAPABILITY_HYBRID_REWARD_PAYOUTS.to_string(),],
                3_749,
                3_748,
            )
            .is_ok()
        );
    }

    #[test]
    fn aggregated_authorization_capability_is_required_at_height_4500() {
        let capabilities = vec![
            CAPABILITY_TRANSACTION_V2_BLOCKS.to_string(),
            CAPABILITY_TRANSACTION_V2_BURNS.to_string(),
            CAPABILITY_HYBRID_REWARD_PAYOUTS.to_string(),
        ];
        assert!(validate_transaction_v2_peer_capability(&capabilities, 4_498, 4_498).is_ok());
        assert!(validate_transaction_v2_peer_capability(&capabilities, 4_499, 4_498).is_err());

        let mut upgraded = capabilities;
        upgraded.push(CAPABILITY_TRANSACTION_V2_AGGREGATED_AUTHORIZATIONS.to_string());
        assert!(validate_transaction_v2_peer_capability(&upgraded, 4_499, 4_498).is_ok());
    }

    #[test]
    fn hybrid_lineage_identity_capability_is_required_at_height_4750() {
        let capabilities = vec![
            CAPABILITY_TRANSACTION_V2_BLOCKS.to_string(),
            CAPABILITY_TRANSACTION_V2_BURNS.to_string(),
            CAPABILITY_HYBRID_REWARD_PAYOUTS.to_string(),
            CAPABILITY_TRANSACTION_V2_AGGREGATED_AUTHORIZATIONS.to_string(),
        ];
        assert!(validate_transaction_v2_peer_capability(&capabilities, 4_748, 4_748).is_ok());
        assert!(validate_transaction_v2_peer_capability(&capabilities, 4_749, 4_748).is_err());

        let mut upgraded = capabilities;
        upgraded.push(CAPABILITY_HYBRID_LINEAGE_IDENTITIES.to_string());
        assert!(validate_transaction_v2_peer_capability(&upgraded, 4_749, 4_748).is_ok());
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
    work: crate::domain::MineSearchWork,
}

impl AutoPowMineJob {
    pub fn anchor(&self) -> &str {
        self.work.anchor()
    }

    pub fn search(self) -> Result<(Self, crate::domain::MineSearchOutcome)> {
        let outcome = self.work.search()?;
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
    vdf_memory_mib: u64,
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
    outbox: VecDeque<(GossipEnvelope, usize)>,
    outbox_bytes: usize,
    network_migration_from: Option<String>,
    vdf_speed_sample: Option<VdfSpeedSample>,
}

impl NodeCore {
    fn enqueue_gossip(&mut self, envelope: GossipEnvelope) {
        let encoded_bytes = gossip_envelope_size(&envelope).unwrap_or(usize::MAX);
        if encoded_bytes > MAX_GOSSIP_OUTBOX_BYTES {
            return;
        }
        while self.outbox.len() >= MAX_GOSSIP_OUTBOX_ENTRIES
            || self.outbox_bytes.saturating_add(encoded_bytes) > MAX_GOSSIP_OUTBOX_BYTES
        {
            let Some((_, removed_bytes)) = self.outbox.pop_front() else {
                break;
            };
            self.outbox_bytes = self.outbox_bytes.saturating_sub(removed_bytes);
        }
        self.outbox.push_back((envelope, encoded_bytes));
        self.outbox_bytes = self.outbox_bytes.saturating_add(encoded_bytes);
    }
}

fn gossip_envelope_size(envelope: &GossipEnvelope) -> Result<usize> {
    struct ByteCounter(usize);
    impl Write for ByteCounter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0 = self.0.saturating_add(buffer.len());
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut counter = ByteCounter(0);
    serde_json::to_writer(&mut counter, envelope)?;
    Ok(counter.0)
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time is before unix epoch")
        .as_millis() as u64
}
