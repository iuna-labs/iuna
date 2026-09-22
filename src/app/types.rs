use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::{
    Amount, Block, BurnBundle, ChainStatus, LaunchProfile, PreparedBlock, StratumMineTemplate,
    Transaction, Wallet,
};

#[derive(Clone, Debug)]
pub struct NodeConfig {
    pub wallet: Wallet,
    pub genesis_allocations: BTreeMap<String, Amount>,
    pub vdf_rounds: u64,
    pub burn_per_block: Amount,
    pub burn_fee: Amount,
    pub pow_mining_workers: u8,
    pub recovery_vdf_top_rank_percent: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeeEstimate {
    pub bytes: usize,
    pub fee: Amount,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct QuantumMigrationPreview {
    pub address: String,
    pub transaction_id: String,
    pub input_count: usize,
    pub remaining_legacy_utxos: usize,
    pub bytes: usize,
    pub fee: Amount,
    pub amount: Amount,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalMineJob {
    pub template: StratumMineTemplate,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GossipEnvelope {
    Hello(ProtocolHello),
    PeerStatus {
        height: u64,
        tip_hash: String,
        #[serde(default)]
        time_ms: u64,
    },
    ChainBootstrapRequest,
    ChainBootstrap(ChainBootstrap),
    BlockLocatorRequest {
        locator: Vec<String>,
        limit: usize,
    },
    BlockRangeRequest {
        from_height: u64,
        limit: usize,
    },
    BlockRequest {
        hashes: Vec<String>,
    },
    Inventory {
        blocks: Vec<BlockInventory>,
    },
    Transaction(Transaction),
    Transactions {
        transactions: Vec<Transaction>,
    },
    TransactionV2 {
        envelope: String,
    },
    TransactionsV2 {
        envelopes: Vec<String>,
    },
    BurnBundle(BurnBundle),
    BurnBundles {
        bundles: Vec<BurnBundle>,
    },
    BurnBundleRequest {
        height: u64,
        prev_hash: String,
        slots: Vec<u8>,
    },
    Block(Block),
    Blocks {
        blocks: Vec<Block>,
    },
    PeerAnnouncement {
        address: String,
        #[serde(default)]
        node_id: Option<String>,
    },
    PeerVerificationChallenge {
        address: String,
        nonce: String,
    },
    PeerVerificationResponse {
        address: String,
        nonce: String,
        node_id: String,
        signature: String,
    },
    PeerList {
        peers: Vec<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChainBootstrap {
    pub genesis_allocations: BTreeMap<String, Amount>,
    pub vdf_rounds: u64,
    pub launch_profile: LaunchProfile,
    pub genesis_block: Block,
    pub height: u64,
    pub tip_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProtocolHello {
    pub protocol_version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    pub network_id: String,
    pub genesis_hash: String,
    pub listen_addr: Option<String>,
    #[serde(default)]
    pub node_id: Option<String>,
    pub height: u64,
    pub tip_hash: String,
    #[serde(default)]
    pub time_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlockInventory {
    pub height: u64,
    pub hash: String,
}

#[cfg(test)]
mod protocol_hello_tests {
    use serde::Deserialize;

    use super::ProtocolHello;

    #[derive(Deserialize)]
    struct V0430ProtocolHello {
        protocol_version: u32,
        network_id: String,
    }

    #[test]
    fn legacy_hello_without_capabilities_remains_compatible() {
        let json = r#"{"protocol_version":2,"network_id":"test","genesis_hash":"genesis","listen_addr":null,"node_id":null,"height":0,"tip_hash":"tip","time_ms":1}"#;
        let hello: ProtocolHello = serde_json::from_str(json).unwrap();

        assert!(hello.capabilities.is_empty());
        assert!(
            !serde_json::to_string(&hello)
                .unwrap()
                .contains("capabilities")
        );
    }

    #[test]
    fn v0430_shape_ignores_new_capabilities_field() {
        let current = ProtocolHello {
            protocol_version: 2,
            capabilities: vec!["address-v1-read".to_string()],
            network_id: "iuna-mainnet-candidate".to_string(),
            genesis_hash: "00".repeat(32),
            listen_addr: None,
            node_id: None,
            height: 0,
            tip_hash: "00".repeat(32),
            time_ms: 1,
        };
        let legacy: V0430ProtocolHello =
            serde_json::from_str(&serde_json::to_string(&current).unwrap()).unwrap();

        assert_eq!(legacy.protocol_version, 2);
        assert_eq!(legacy.network_id, "iuna-mainnet-candidate");
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NodeStatus {
    pub app_version: String,
    pub wallet_address: String,
    pub wallet_receive_address: String,
    pub wallet_balance: Amount,
    pub wallet_locked: bool,
    pub quantum_migration: QuantumMigrationStatus,
    pub launch_profile: LaunchProfileStatus,
    pub mining: MiningStatus,
    pub stratum: StratumStatus,
    pub chain: ChainStatus,
    pub network_migration: NetworkMigrationStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QuantumMigrationStatus {
    pub active: bool,
    pub hybrid_address: Option<String>,
    pub legacy_balance: Amount,
    pub hybrid_balance: Amount,
    pub legacy_utxos: usize,
    pub migration_pending: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NetworkMigrationStatus {
    pub required: bool,
    pub from_network: Option<String>,
    pub to_network: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LaunchProfileStatus {
    pub profile_id: String,
    pub profile_hash: String,
    pub ticket_maturity_delay_heights: u64,
    pub ticket_expiry_window_heights: u64,
    pub mine_difficulty_bits: u32,
    pub burn_lineage_maturity_heights: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MiningStatus {
    pub automatic: bool,
    pub pow_mining_enabled: bool,
    pub pow_mining_workers: u8,
    pub max_pow_mining_workers: u8,
    pub burn_per_block: Amount,
    pub automatic_burn_fee: Amount,
    pub automatic_pow_mine_fee: Amount,
    pub last_auto_finalization_status: Option<String>,
    pub last_auto_pow_mine_anchor: Option<String>,
    pub last_auto_pow_mine_status: Option<String>,
    pub vdf_rounds: u64,
    pub vdf_target_block_ms: u64,
    pub current_leader: Option<String>,
    pub wallet_is_current_leader: bool,
    pub last_auto_burn_height: Option<u64>,
    pub recovery_vdf_top_rank_percent: u8,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StratumStatus {
    pub enabled: bool,
    pub listen_addr: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutoMineOutcome {
    pub pow_mined: Option<Transaction>,
    pub burned: Option<Transaction>,
    pub block: Option<Block>,
    pub skipped_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AutoMinePlan {
    pub pow_mined: Option<Transaction>,
    pub burned: Option<Transaction>,
    pub work: Option<PreparedBlock>,
    pub skipped_reason: Option<String>,
}
