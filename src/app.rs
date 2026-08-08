use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::adapters::config_store::{
    DEFAULT_POW_MINING_WORKERS, MAX_POW_MINING_WORKERS, clamp_pow_mining_workers,
};
use crate::domain::{
    Amount, BlindedReveal, BlindedTransaction, Block, BuiltBlindedTransaction, BurnLeaderRank,
    ChainSnapshot, ChainStatus, DEFAULT_FEE_PER_BYTE, DEFAULT_TRANSACTION_FEE, Ledger,
    MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS, MINE_FINALIZER_FEE, MINE_REWARD, OutPoint,
    OwnedBlindedTransaction, PreparedBlock, RevealBundle, StratumMineShare, StratumMineTemplate,
    Transaction, TransactionSubmitOutcome, VDF_TARGET_BLOCK_MS, Wallet, run_vdf,
};

pub type SharedNode = Arc<Mutex<NodeCore>>;
pub type SharedPeerBook = Arc<Mutex<PeerBook>>;

pub const DEFAULT_BURN_PER_BLOCK: Amount = 0;
pub const DEFAULT_VDF_ROUNDS: u32 = 67_000_000;
pub const PROTOCOL_VERSION: u32 = 1;
pub const NETWORK_ID: &str = "iuna-devnet-v3";
pub const BLOCK_REQUEST_LIMIT: usize = 128;
pub const TRANSACTION_BATCH_LIMIT: usize = 128;
const IMPORT_REBROADCAST_LIMIT: usize = 128;
pub const PEER_MISBEHAVIOR_BAN_SCORE: u32 = 3;
pub const PEER_MISBEHAVIOR_BAN_MS: u64 = 10 * 60 * 1_000;
pub const PEER_CLOCK_OFFSET_ACCEPTANCE_MS: i64 = 10 * 60 * 1_000;
const PEER_CLOCK_OFFSET_STALE_MS: u64 = 20 * 60 * 1_000;
const AUTO_POW_NONCE_ATTEMPTS_PER_WORKER_TICK: u64 = 100_000;
const AUTO_PLAINTEXT_BURN_BEFORE_RECOVERY_MS: u64 = 60_000;
const AUTO_BLOCK_ANCHOR_BURN_AMOUNT: Amount = 1;
const AUTO_BLOCK_ANCHOR_BURN_FEE: Amount = 0;
static DEBUG_LOGGING: AtomicBool = AtomicBool::new(false);

pub fn set_debug_logging(enabled: bool) {
    DEBUG_LOGGING.store(enabled, Ordering::Relaxed);
}

pub fn debug_logging_enabled() -> bool {
    DEBUG_LOGGING.load(Ordering::Relaxed)
}

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalMineJob {
    pub template: StratumMineTemplate,
}

#[derive(Clone, Debug)]
enum NodeWallet {
    Unlocked(Wallet),
    Locked { address: String },
}

impl NodeWallet {
    fn address(&self) -> &str {
        match self {
            Self::Unlocked(wallet) => wallet.address(),
            Self::Locked { address } => address,
        }
    }

    fn unlocked(&self) -> Result<&Wallet> {
        match self {
            Self::Unlocked(wallet) => Ok(wallet),
            Self::Locked { .. } => bail!("wallet is locked"),
        }
    }

    fn is_locked(&self) -> bool {
        matches!(self, Self::Locked { .. })
    }
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
    ChainSnapshotRequest,
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
    BlindedTransaction(BlindedTransaction),
    BlindedTransactions {
        transactions: Vec<BlindedTransaction>,
    },
    MineAction(Transaction),
    MineActions {
        transactions: Vec<Transaction>,
    },
    BlindedReveal(BlindedReveal),
    BlindedReveals {
        reveals: Vec<BlindedReveal>,
    },
    RevealBundle(RevealBundle),
    RevealBundles {
        bundles: Vec<RevealBundle>,
    },
    Block(Block),
    Blocks {
        blocks: Vec<Block>,
    },
    ChainSnapshot(ChainSnapshot),
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
pub struct ProtocolHello {
    pub protocol_version: u32,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NodeStatus {
    pub app_version: String,
    pub wallet_address: String,
    pub wallet_balance: Amount,
    pub wallet_locked: bool,
    pub launch_profile: LaunchProfileStatus,
    pub mining: MiningStatus,
    pub stratum: StratumStatus,
    pub chain: ChainStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LaunchProfileStatus {
    pub profile_id: String,
    pub profile_hash: String,
    pub ticket_maturity_delay_heights: u64,
    pub ticket_expiry_window_heights: u64,
    pub mine_difficulty_bits: u32,
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct AutoPowMineCursor {
    anchor: String,
    salt: u64,
    next_nonce: u64,
    searched: u64,
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
    last_auto_pow_mine_anchor: Option<String>,
    last_auto_pow_mine_status: Option<String>,
    auto_pow_mine_cursor: Option<AutoPowMineCursor>,
    owned_blinded_transactions: BTreeMap<String, BlindedTransaction>,
    owned_blinded_reveals: BTreeMap<String, BlindedReveal>,
    owned_blinded_payloads: BTreeMap<String, Transaction>,
    owned_blinded_outbox_version: u64,
    reveal_bundles: BTreeMap<(u64, u8), RevealBundle>,
    equivocated_reveal_bundle_slots: BTreeSet<(u64, u8)>,
    local_block_anchor_burn: Option<(u64, Transaction)>,
    outbox: Vec<GossipEnvelope>,
}

impl NodeCore {
    pub fn new(config: NodeConfig) -> Self {
        let ledger = Ledger::new(config.genesis_allocations, config.vdf_rounds);
        let mut node = Self::from_ledger_with_burn_fee(
            config.wallet,
            ledger,
            config.burn_per_block,
            config.burn_fee,
        );
        node.set_pow_mining_workers(config.pow_mining_workers);
        node.set_recovery_vdf_top_rank_percent(config.recovery_vdf_top_rank_percent);
        node
    }

    pub fn from_ledger(wallet: Wallet, ledger: Ledger, burn_per_block: Amount) -> Self {
        Self::from_ledger_with_burn_fee(wallet, ledger, burn_per_block, DEFAULT_FEE_PER_BYTE)
    }

    pub fn from_locked_wallet_address(
        address: impl Into<String>,
        ledger: Ledger,
        automatic_mining_enabled: bool,
        burn_per_block: Amount,
        burn_fee: Amount,
    ) -> Self {
        Self::from_node_wallet_with_burn_fee_and_enabled(
            NodeWallet::Locked {
                address: address.into(),
            },
            ledger,
            automatic_mining_enabled,
            burn_per_block,
            burn_fee,
            DEFAULT_POW_MINING_WORKERS,
            100,
        )
    }

    pub fn from_ledger_with_burn_fee(
        wallet: Wallet,
        ledger: Ledger,
        burn_per_block: Amount,
        burn_fee: Amount,
    ) -> Self {
        Self::from_ledger_with_burn_fee_and_enabled(
            wallet,
            ledger,
            burn_per_block > 0,
            burn_per_block,
            burn_fee,
        )
    }

    pub fn from_ledger_with_burn_fee_and_enabled(
        wallet: Wallet,
        ledger: Ledger,
        automatic_mining_enabled: bool,
        burn_per_block: Amount,
        burn_fee: Amount,
    ) -> Self {
        Self::from_node_wallet_with_burn_fee_and_enabled(
            NodeWallet::Unlocked(wallet),
            ledger,
            automatic_mining_enabled,
            burn_per_block,
            burn_fee,
            DEFAULT_POW_MINING_WORKERS,
            100,
        )
    }

    fn from_node_wallet_with_burn_fee_and_enabled(
        wallet: NodeWallet,
        ledger: Ledger,
        automatic_mining_enabled: bool,
        burn_per_block: Amount,
        burn_fee: Amount,
        pow_mining_workers: u8,
        recovery_vdf_top_rank_percent: u8,
    ) -> Self {
        Self {
            wallet,
            ledger,
            automatic_mining_enabled,
            pow_mining_enabled: false,
            pow_mining_workers: clamp_pow_mining_workers(pow_mining_workers),
            burn_per_block,
            burn_fee,
            recovery_vdf_top_rank_percent: recovery_vdf_top_rank_percent.min(100),
            last_auto_burn_height: None,
            last_auto_anchor_burn_height: None,
            last_auto_pow_mine_anchor: None,
            last_auto_pow_mine_status: None,
            auto_pow_mine_cursor: None,
            owned_blinded_transactions: BTreeMap::new(),
            owned_blinded_reveals: BTreeMap::new(),
            owned_blinded_payloads: BTreeMap::new(),
            owned_blinded_outbox_version: 0,
            reveal_bundles: BTreeMap::new(),
            equivocated_reveal_bundle_slots: BTreeSet::new(),
            local_block_anchor_burn: None,
            outbox: Vec::new(),
        }
    }

    pub fn wallet_address(&self) -> &str {
        self.wallet.address()
    }

    pub fn wallet_is_locked(&self) -> bool {
        self.wallet.is_locked()
    }

    pub fn replace_wallet(&mut self, wallet: Wallet) {
        self.wallet = NodeWallet::Unlocked(wallet);
        self.last_auto_burn_height = None;
        self.last_auto_anchor_burn_height = None;
        self.last_auto_pow_mine_anchor = None;
        self.last_auto_pow_mine_status = None;
        self.auto_pow_mine_cursor = None;
        self.owned_blinded_transactions.clear();
        self.owned_blinded_reveals.clear();
        self.owned_blinded_payloads.clear();
        self.bump_owned_blinded_outbox_version();
        self.reveal_bundles.clear();
        self.equivocated_reveal_bundle_slots.clear();
        self.local_block_anchor_burn = None;
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub(crate) fn clone_ledger(&self) -> Ledger {
        self.ledger.clone()
    }

    pub fn wallet_view_ledger(&self) -> Result<Ledger> {
        let mut ledger = self.ledger.clone();
        self.queue_local_block_anchor(&mut ledger)?;
        self.queue_owned_blinded_payloads(&mut ledger)?;
        Ok(ledger)
    }

    pub fn chain(&self) -> &[Block] {
        self.ledger.chain()
    }

    pub fn chain_height(&self) -> u64 {
        self.ledger.height()
    }

    pub fn has_real_chain(&self) -> bool {
        !self.ledger.is_setup_placeholder()
    }

    pub fn recent_blocks(&self, limit: usize) -> Vec<Block> {
        self.ledger.recent_blocks(limit)
    }

    pub fn blocks_before(&self, before_height: u64, limit: usize) -> Vec<Block> {
        self.ledger.blocks_before(before_height, limit)
    }

    pub fn burn_leader_ranks_for_block(&self, height: u64) -> Result<Vec<BurnLeaderRank>> {
        self.ledger.burn_leader_ranks_for_block(height)
    }

    pub fn pending_transactions(&self) -> Vec<Transaction> {
        self.ledger.pending().to_vec()
    }

    pub fn pending_blinded_transactions(&self) -> Vec<BlindedTransaction> {
        self.ledger.pending_blinded_transactions().to_vec()
    }

    pub fn pending_blinded_reveals(&self) -> Vec<BlindedReveal> {
        self.ledger.pending_blinded_reveals().to_vec()
    }

    pub fn pending_revealed_blinded_transactions(
        &self,
    ) -> Vec<crate::domain::RevealedBlindedTransaction> {
        self.ledger.pending_revealed_blinded_transactions()
    }

    pub fn owned_blinded_payloads(&self) -> Vec<Transaction> {
        self.owned_blinded_payloads.values().cloned().collect()
    }

    pub fn owned_blinded_outbox_version(&self) -> u64 {
        self.owned_blinded_outbox_version
    }

    pub fn owned_blinded_transactions(&self) -> Vec<OwnedBlindedTransaction> {
        self.owned_blinded_transactions
            .iter()
            .filter_map(|(commitment, transaction)| {
                let payload = self.owned_blinded_payloads.get(commitment)?;
                let reveal = self.owned_blinded_reveals.get(commitment)?;
                Some(OwnedBlindedTransaction {
                    transaction: transaction.clone(),
                    payload: payload.clone(),
                    reveal: reveal.clone(),
                })
            })
            .collect()
    }

    pub fn mark_owned_blinded_outbox_persisted(&mut self, version: u64) {
        if self.owned_blinded_outbox_version == version {
            self.owned_blinded_outbox_version = 0;
        }
    }

    pub fn restore_owned_blinded_transactions(
        &mut self,
        transactions: Vec<OwnedBlindedTransaction>,
    ) -> Result<()> {
        self.owned_blinded_transactions.clear();
        self.owned_blinded_reveals.clear();
        self.owned_blinded_payloads.clear();
        for owned in transactions {
            let commitment = owned.transaction.commitment.clone();
            if owned.reveal.commitment != commitment {
                continue;
            }
            if self.ledger.has_blinded_reveal(&commitment)
                || !self.ledger.has_unrevealed_blinded_transaction(&commitment)
                    && owned.transaction.expires_at_height <= self.ledger.height().saturating_add(1)
            {
                continue;
            }
            if !self.ledger.has_blinded_transaction(&commitment) {
                match self
                    .ledger
                    .submit_blinded_transaction(owned.transaction.clone())
                {
                    Ok(true) => self.outbox.push(GossipEnvelope::BlindedTransaction(
                        owned.transaction.clone(),
                    )),
                    Ok(false) => {}
                    Err(_) => continue,
                }
            }
            if !self.ledger.has_unrevealed_blinded_transaction(&commitment) {
                continue;
            }
            self.owned_blinded_transactions
                .insert(commitment.clone(), owned.transaction);
            self.owned_blinded_payloads
                .insert(commitment.clone(), owned.payload);
            self.owned_blinded_reveals
                .insert(commitment.clone(), owned.reveal);
        }
        self.publish_owned_reveals_for_active_commits()?;
        self.bump_owned_blinded_outbox_version();
        Ok(())
    }

    pub fn mempool_gossip(&mut self) -> Vec<GossipEnvelope> {
        let _ = self.publish_reveal_bundle_for_next_block();
        let mut gossip = Vec::new();
        let mine_actions = self
            .ledger
            .pending()
            .iter()
            .filter(|transaction| matches!(transaction, Transaction::Mine { .. }))
            .cloned()
            .collect::<Vec<_>>();
        gossip.extend(mine_actions.chunks(TRANSACTION_BATCH_LIMIT).map(|chunk| {
            GossipEnvelope::MineActions {
                transactions: chunk.to_vec(),
            }
        }));
        gossip.extend(
            self.ledger
                .pending_blinded_transactions()
                .chunks(TRANSACTION_BATCH_LIMIT)
                .map(|chunk| GossipEnvelope::BlindedTransactions {
                    transactions: chunk.to_vec(),
                }),
        );
        gossip.extend(
            self.ledger
                .pending_blinded_reveals()
                .chunks(TRANSACTION_BATCH_LIMIT)
                .map(|chunk| GossipEnvelope::BlindedReveals {
                    reveals: chunk.to_vec(),
                }),
        );
        gossip.extend(
            self.usable_reveal_bundles()
                .chunks(TRANSACTION_BATCH_LIMIT)
                .map(|chunk| GossipEnvelope::RevealBundles {
                    bundles: chunk.to_vec(),
                }),
        );
        gossip
    }

    pub fn chain_snapshot(&self) -> ChainSnapshot {
        self.ledger.snapshot()
    }

    pub fn hello(&self, listen_addr: Option<String>, node_id: Option<String>) -> GossipEnvelope {
        let status = self.ledger.status();
        GossipEnvelope::Hello(ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: self.ledger.genesis_hash().to_string(),
            listen_addr,
            node_id,
            height: status.height,
            tip_hash: status.tip_hash,
            time_ms: now_ms(),
        })
    }

    pub fn peer_status(&self) -> GossipEnvelope {
        let status = self.ledger.status();
        GossipEnvelope::PeerStatus {
            height: status.height,
            tip_hash: status.tip_hash,
            time_ms: now_ms(),
        }
    }

    pub fn blocks_from(&self, from_height: u64, limit: usize) -> Vec<Block> {
        self.ledger.blocks_from(from_height, limit)
    }

    pub fn blocks_by_hash(&self, hashes: &[String]) -> Vec<Block> {
        hashes
            .iter()
            .filter_map(|hash| self.ledger.block_by_hash(hash))
            .collect()
    }

    pub fn missing_inventory_requests(&self, blocks: &[BlockInventory]) -> Vec<GossipEnvelope> {
        let local_height = self.ledger.height();
        let first_height_gap = blocks
            .iter()
            .filter(|block| !self.ledger.has_block(&block.hash))
            .filter(|block| block.height > local_height + 1)
            .map(|block| block.height)
            .min();
        let missing_blocks = blocks
            .iter()
            .filter(|block| !self.ledger.has_block(&block.hash))
            .filter(|block| first_height_gap.is_none_or(|gap| block.height < gap))
            .map(|block| block.hash.clone())
            .collect::<Vec<_>>();

        let mut requests = Vec::new();
        if !missing_blocks.is_empty() {
            requests.push(GossipEnvelope::BlockRequest {
                hashes: missing_blocks,
            });
        }
        if first_height_gap.is_some() {
            requests.push(GossipEnvelope::BlockRangeRequest {
                from_height: local_height + 1,
                limit: BLOCK_REQUEST_LIMIT,
            });
        }
        requests
    }

    pub fn status(&self) -> NodeStatus {
        let chain = self.ledger.status();
        let launch_profile = self.ledger.launch_profile();
        let current_leader = self.ledger.expected_leader_for_next_block();
        let wallet_is_current_leader = current_leader
            .as_deref()
            .is_none_or(|leader| leader == self.wallet.address());

        NodeStatus {
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            wallet_address: self.wallet.address().to_string(),
            wallet_balance: self.wallet_projected_balance(),
            wallet_locked: self.wallet.is_locked(),
            launch_profile: LaunchProfileStatus {
                profile_id: launch_profile.profile_id.clone(),
                profile_hash: chain.launch_profile_hash.clone(),
                ticket_maturity_delay_heights: launch_profile.ticket_maturity_delay_heights,
                ticket_expiry_window_heights: launch_profile.ticket_expiry_window_heights,
                mine_difficulty_bits: launch_profile.mine_difficulty_bits,
            },
            mining: MiningStatus {
                automatic: self.automatic_mining_enabled,
                pow_mining_enabled: self.pow_mining_enabled,
                pow_mining_workers: self.pow_mining_workers,
                max_pow_mining_workers: MAX_POW_MINING_WORKERS,
                burn_per_block: self.burn_per_block,
                automatic_burn_fee: self.burn_fee,
                automatic_pow_mine_fee: MINE_FINALIZER_FEE,
                last_auto_pow_mine_anchor: self.last_auto_pow_mine_anchor.clone(),
                last_auto_pow_mine_status: if self.pow_mining_enabled && !self.has_real_chain() {
                    Some("waiting for a real chain before PoW mining can start".to_string())
                } else {
                    self.last_auto_pow_mine_status.clone()
                },
                vdf_rounds: self.ledger.vdf_rounds(),
                vdf_target_block_ms: VDF_TARGET_BLOCK_MS,
                current_leader,
                wallet_is_current_leader,
                last_auto_burn_height: self.last_auto_burn_height,
                recovery_vdf_top_rank_percent: self.recovery_vdf_top_rank_percent,
            },
            stratum: StratumStatus {
                enabled: false,
                listen_addr: None,
            },
            chain,
        }
    }

    fn wallet_projected_balance(&self) -> Amount {
        let address = self.wallet.address();
        let mut balance = self.ledger.balance_of(address);
        let confirmed_outputs = self
            .ledger
            .utxos_for_address(address)
            .into_iter()
            .map(|(outpoint, output)| (outpoint, output.amount))
            .collect::<BTreeMap<_, _>>();

        for (commitment, payload) in &self.owned_blinded_payloads {
            if !self.ledger.has_unrevealed_blinded_transaction(commitment) {
                continue;
            }
            let output_total = transaction_output_total_for_address(payload, address);
            if self.ledger.has_active_blinded_transaction(commitment) {
                balance = balance.saturating_add(output_total);
            } else {
                let input_total =
                    transaction_input_total_from_outputs(payload, address, &confirmed_outputs);
                balance = balance
                    .saturating_sub(input_total)
                    .saturating_add(output_total);
            }
        }

        if let Some((height, burn)) = &self.local_block_anchor_burn {
            if *height == self.ledger.height() && !self.ledger.has_transaction(burn.signature()) {
                let output_total = transaction_output_total_for_address(burn, address);
                let input_total =
                    transaction_input_total_from_outputs(burn, address, &confirmed_outputs);
                balance = balance
                    .saturating_sub(input_total)
                    .saturating_add(output_total);
            }
        }

        balance
    }

    pub fn set_burn_per_block(&mut self, amount: Amount) -> Result<Option<Transaction>> {
        self.set_automatic_burn(amount, self.burn_fee)
    }

    pub fn set_automatic_burn(
        &mut self,
        amount: Amount,
        fee: Amount,
    ) -> Result<Option<Transaction>> {
        self.set_automatic_burn_settings(amount > 0, amount, fee)
    }

    pub fn set_automatic_burn_settings(
        &mut self,
        enabled: bool,
        amount: Amount,
        fee: Amount,
    ) -> Result<Option<Transaction>> {
        let was_disabled = !self.automatic_mining_enabled || self.burn_per_block == 0;
        self.automatic_mining_enabled = enabled;
        self.burn_per_block = amount;
        self.burn_fee = fee;
        if was_disabled && enabled && amount > 0 {
            self.last_auto_burn_height = None;
            self.last_auto_anchor_burn_height = None;
        }
        self.prepare_automatic_burn(now_ms())
    }

    pub fn set_pow_mining_enabled(&mut self, enabled: bool) {
        self.pow_mining_enabled = enabled;
        self.auto_pow_mine_cursor = None;
        if !enabled {
            self.last_auto_pow_mine_anchor = None;
            self.last_auto_pow_mine_status = None;
        } else {
            self.last_auto_pow_mine_status =
                Some("waiting for next automatic PoW mining tick".to_string());
        }
    }

    pub fn pow_mining_enabled(&self) -> bool {
        self.pow_mining_enabled
    }

    pub fn set_pow_mining_workers(&mut self, workers: u8) {
        let workers = clamp_pow_mining_workers(workers);
        if self.pow_mining_workers != workers {
            self.pow_mining_workers = workers;
            self.auto_pow_mine_cursor = None;
            if self.pow_mining_enabled {
                self.last_auto_pow_mine_status =
                    Some("waiting for next automatic PoW mining tick".to_string());
            }
        }
    }

    pub fn pow_mining_workers(&self) -> u8 {
        self.pow_mining_workers
    }

    pub fn set_recovery_vdf_top_rank_percent(&mut self, percent: u8) {
        self.recovery_vdf_top_rank_percent = percent.min(100);
    }

    pub fn burn(&mut self, amount: Amount) -> Result<Transaction> {
        self.burn_with_fee(amount, 0)
    }

    pub fn burn_with_fee(&mut self, amount: Amount, fee: Amount) -> Result<Transaction> {
        let tx = self
            .wallet_build_ledger()?
            .build_burn(self.wallet.unlocked()?, amount, fee)?;
        self.submit_transaction_as_owned_blinded(tx)
    }

    pub fn burn_with_fee_rate(
        &mut self,
        amount: Amount,
        fee_per_byte: Amount,
    ) -> Result<(Transaction, FeeEstimate)> {
        let (built, estimate) = self.build_blinded_burn_with_fee_rate(amount, fee_per_byte)?;
        let tx = built.payload.clone();
        self.submit_owned_blinded_transaction(built)?;
        Ok((tx, estimate))
    }

    pub fn blinded_burn_with_fee(
        &mut self,
        amount: Amount,
        fee: Amount,
        expires_at_height: u64,
    ) -> Result<BlindedTransaction> {
        let built = self.wallet_build_ledger()?.build_blinded_burn(
            self.wallet.unlocked()?,
            amount,
            fee,
            expires_at_height,
        )?;
        self.submit_owned_blinded_transaction(built)
    }

    pub fn estimate_burn_fee(&self, amount: Amount, fee_per_byte: Amount) -> Result<FeeEstimate> {
        self.build_burn_with_fee_rate(amount, fee_per_byte)
            .map(|(_, estimate)| estimate)
    }

    pub fn transfer(&mut self, to: impl Into<String>, amount: Amount) -> Result<Transaction> {
        self.transfer_with_fee(to, amount, DEFAULT_TRANSACTION_FEE)
    }

    pub fn transfer_with_fee(
        &mut self,
        to: impl Into<String>,
        amount: Amount,
        fee: Amount,
    ) -> Result<Transaction> {
        let tx =
            self.wallet_build_ledger()?
                .build_transfer(self.wallet.unlocked()?, to, amount, fee)?;
        self.submit_transaction_as_owned_blinded(tx)
    }

    pub fn transfer_with_fee_spending(
        &mut self,
        to: impl Into<String>,
        amount: Amount,
        fee: Amount,
        outpoints: &[OutPoint],
    ) -> Result<Transaction> {
        let tx = self.wallet_build_ledger()?.build_transfer_with_inputs(
            self.wallet.unlocked()?,
            to,
            amount,
            fee,
            outpoints,
        )?;
        self.submit_transaction_as_owned_blinded(tx)
    }

    pub fn blinded_transfer_with_fee(
        &mut self,
        to: impl Into<String>,
        amount: Amount,
        fee: Amount,
        expires_at_height: u64,
    ) -> Result<BlindedTransaction> {
        let built = self.wallet_build_ledger()?.build_blinded_transfer(
            self.wallet.unlocked()?,
            to,
            amount,
            fee,
            expires_at_height,
        )?;
        self.submit_owned_blinded_transaction(built)
    }

    pub fn transfer_with_fee_rate(
        &mut self,
        to: impl Into<String>,
        amount: Amount,
        fee_per_byte: Amount,
        outpoints: &[OutPoint],
    ) -> Result<(Transaction, FeeEstimate)> {
        let (built, estimate) =
            self.build_blinded_transfer_with_fee_rate(to, amount, fee_per_byte, outpoints)?;
        let tx = built.payload.clone();
        self.submit_owned_blinded_transaction(built)?;
        Ok((tx, estimate))
    }

    pub fn estimate_transfer_fee(
        &self,
        to: impl Into<String>,
        amount: Amount,
        fee_per_byte: Amount,
        outpoints: &[OutPoint],
    ) -> Result<FeeEstimate> {
        self.build_transfer_with_fee_rate(to, amount, fee_per_byte, outpoints)
            .map(|(_, estimate)| estimate)
    }

    pub fn mine_pow_reward(&mut self) -> Result<Transaction> {
        let (tx, _) = self.build_mine_estimate()?;
        self.submit_public_mine_action(tx)
    }

    pub fn estimate_mine_fee(&self, _fee_per_byte: Amount) -> Result<FeeEstimate> {
        self.build_mine_estimate().map(|(_, estimate)| estimate)
    }

    pub fn external_mine_job(
        &self,
        recipient: impl Into<String>,
        salt: u64,
    ) -> Result<ExternalMineJob> {
        let recipient = recipient.into();
        let tip = self
            .chain()
            .last()
            .context("cannot build mine job without a chain tip")?;
        let difficulty_bits = self.ledger.current_mine_difficulty_bits();
        Ok(ExternalMineJob {
            template: self.ledger.stratum_mine_template(
                recipient,
                &tip.hash,
                salt,
                difficulty_bits,
            )?,
        })
    }

    pub fn submit_external_mine(
        &mut self,
        recipient: impl Into<String>,
        template: StratumMineTemplate,
        share: StratumMineShare,
    ) -> Result<Transaction> {
        let tx = self.ledger.build_stratum_mine(template, share)?;
        let recipient = recipient.into();
        if tx.to() != Some(recipient.as_str()) {
            bail!("submitted mine recipient does not match worker");
        }
        self.submit_public_mine_action(tx)
    }

    pub fn receive_transaction(&mut self, tx: Transaction) -> Result<TransactionSubmitOutcome> {
        let outcome = self.ledger.submit_transaction_with_outcome(tx.clone())?;
        Ok(outcome)
    }

    pub fn receive_mine_action(&mut self, tx: Transaction) -> Result<()> {
        if !matches!(tx, Transaction::Mine { .. }) {
            bail!("only mine actions may be gossiped as plaintext");
        }
        if self
            .ledger
            .submit_transaction_with_outcome(tx.clone())?
            .added()
        {
            self.outbox.push(GossipEnvelope::MineAction(tx));
        }
        Ok(())
    }

    pub fn receive_blinded_transaction(&mut self, tx: BlindedTransaction) -> Result<()> {
        if self.blinded_transaction_conflicts_with_local_anchor(&tx) {
            return Ok(());
        }
        if self.ledger.submit_blinded_transaction(tx.clone())? {
            self.outbox.push(GossipEnvelope::BlindedTransaction(tx));
        }
        Ok(())
    }

    fn blinded_transaction_conflicts_with_local_anchor(&self, tx: &BlindedTransaction) -> bool {
        let Some((height, burn)) = &self.local_block_anchor_burn else {
            return false;
        };
        if *height != self.ledger.height() || self.ledger.has_transaction(burn.signature()) {
            return false;
        }
        let anchor_inputs = transaction_input_outpoints(burn);
        tx.inputs
            .iter()
            .any(|input| anchor_inputs.contains(&input.outpoint))
    }

    pub fn receive_blinded_reveal(&mut self, reveal: BlindedReveal) -> Result<()> {
        self.receive_blinded_reveal_without_bundle_publish(reveal)?;
        Ok(())
    }

    fn receive_blinded_reveal_without_bundle_publish(
        &mut self,
        reveal: BlindedReveal,
    ) -> Result<bool> {
        if self.ledger.submit_blinded_reveal(reveal.clone())? {
            self.outbox.push(GossipEnvelope::BlindedReveal(reveal));
            return Ok(true);
        }
        Ok(false)
    }

    pub fn receive_reveal_bundle(&mut self, bundle: RevealBundle) -> Result<()> {
        let next_height = self.ledger.height().saturating_add(1);
        if bundle.height <= self.ledger.height() {
            return Ok(());
        }
        if bundle.height > next_height {
            return Ok(());
        }
        let key = (bundle.height, bundle.slot);
        if self.equivocated_reveal_bundle_slots.contains(&key) {
            return Ok(());
        }
        if let Some(existing) = self.reveal_bundles.get(&key) {
            if existing.canonical() != bundle.canonical() {
                self.reveal_bundles.remove(&key);
                self.equivocated_reveal_bundle_slots.insert(key);
            }
            return Ok(());
        }
        self.ledger
            .validate_next_block_reveal_bundles(vec![bundle.clone()])?;
        self.reveal_bundles.insert(key, bundle.clone());
        self.outbox.push(GossipEnvelope::RevealBundle(bundle));
        Ok(())
    }

    fn usable_reveal_bundles(&self) -> Vec<RevealBundle> {
        let next_height = self.ledger.height().saturating_add(1);
        let mut bundles = self
            .reveal_bundles
            .iter()
            .filter(|((height, slot), _)| {
                *height == next_height
                    && !self
                        .equivocated_reveal_bundle_slots
                        .contains(&(*height, *slot))
            })
            .map(|(_, bundle)| bundle.clone())
            .collect::<Vec<_>>();
        bundles.sort_by_key(|bundle| bundle.slot);
        bundles
    }

    fn prune_reveal_bundles(&mut self) {
        let height = self.ledger.height();
        self.reveal_bundles
            .retain(|(bundle_height, _), _| *bundle_height > height);
        self.equivocated_reveal_bundle_slots
            .retain(|(bundle_height, _)| *bundle_height > height);
    }

    fn publish_reveal_bundle_for_next_block(&mut self) -> Result<()> {
        let wallet = match &self.wallet {
            NodeWallet::Unlocked(wallet) => wallet,
            NodeWallet::Locked { .. } => return Ok(()),
        };
        let Some(bundle) = self.ledger.build_reveal_bundle(wallet)? else {
            return Ok(());
        };
        let key = (bundle.height, bundle.slot);
        if self.equivocated_reveal_bundle_slots.contains(&key)
            || self.reveal_bundles.contains_key(&key)
        {
            return Ok(());
        }
        self.ledger
            .validate_next_block_reveal_bundles(vec![bundle.clone()])?;
        self.reveal_bundles.insert(key, bundle.clone());
        self.outbox.push(GossipEnvelope::RevealBundle(bundle));
        Ok(())
    }

    fn submit_owned_blinded_transaction(
        &mut self,
        built: BuiltBlindedTransaction,
    ) -> Result<BlindedTransaction> {
        let transaction = built.transaction;
        self.owned_blinded_transactions
            .insert(transaction.commitment.clone(), transaction.clone());
        self.owned_blinded_payloads
            .insert(transaction.commitment.clone(), built.payload);
        self.owned_blinded_reveals
            .insert(transaction.commitment.clone(), built.reveal);
        self.bump_owned_blinded_outbox_version();
        if self
            .ledger
            .submit_blinded_transaction(transaction.clone())?
        {
            self.outbox
                .push(GossipEnvelope::BlindedTransaction(transaction.clone()));
        }
        Ok(transaction)
    }

    fn submit_public_mine_action(&mut self, tx: Transaction) -> Result<Transaction> {
        if !matches!(tx, Transaction::Mine { .. }) {
            bail!("only mine actions may be submitted as public mempool transactions");
        }
        if self
            .ledger
            .submit_transaction_with_outcome(tx.clone())?
            .added()
        {
            self.outbox.push(GossipEnvelope::MineAction(tx.clone()));
        }
        Ok(tx)
    }

    fn submit_transaction_as_owned_blinded(&mut self, tx: Transaction) -> Result<Transaction> {
        let built = self.ledger.build_blinded_transaction(
            self.wallet.unlocked()?,
            tx.clone(),
            self.default_blinded_transaction_expiry_height(),
        )?;
        self.submit_owned_blinded_transaction(built)?;
        Ok(tx)
    }

    fn default_blinded_transaction_expiry_height(&self) -> u64 {
        self.ledger
            .height()
            .saturating_add(MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS)
    }

    fn publish_owned_reveals_for_block(&mut self, block: &Block) -> Result<()> {
        for transaction in &block.blinded_transactions {
            let Some(reveal) = self.owned_blinded_reveals.get(&transaction.commitment) else {
                continue;
            };
            if self.ledger.submit_blinded_reveal(reveal.clone())? {
                self.outbox
                    .push(GossipEnvelope::BlindedReveal(reveal.clone()));
            }
        }
        Ok(())
    }

    fn publish_owned_reveals_for_active_commits(&mut self) -> Result<()> {
        let reveals = self
            .owned_blinded_reveals
            .iter()
            .filter(|(commitment, _)| self.ledger.has_active_blinded_transaction(commitment))
            .map(|(_, reveal)| reveal.clone())
            .collect::<Vec<_>>();
        for reveal in reveals {
            if self.ledger.submit_blinded_reveal(reveal.clone())? {
                self.outbox.push(GossipEnvelope::BlindedReveal(reveal));
            }
        }
        Ok(())
    }

    fn prune_owned_blinded_payloads_for_block(&mut self, block: &Block) {
        let before = self.owned_blinded_transactions.len()
            + self.owned_blinded_reveals.len()
            + self.owned_blinded_payloads.len();
        for reveal in block.all_blinded_reveals() {
            self.owned_blinded_transactions.remove(&reveal.commitment);
            self.owned_blinded_reveals.remove(&reveal.commitment);
            self.owned_blinded_payloads.remove(&reveal.commitment);
        }
        let unrevealed = self
            .owned_blinded_transactions
            .keys()
            .filter(|commitment| self.ledger.has_unrevealed_blinded_transaction(commitment))
            .cloned()
            .collect::<Vec<_>>();
        self.owned_blinded_transactions
            .retain(|commitment, _| unrevealed.contains(commitment));
        self.owned_blinded_reveals
            .retain(|commitment, _| unrevealed.contains(commitment));
        self.owned_blinded_payloads
            .retain(|commitment, _| unrevealed.contains(commitment));
        let after = self.owned_blinded_transactions.len()
            + self.owned_blinded_reveals.len()
            + self.owned_blinded_payloads.len();
        if before != after {
            self.bump_owned_blinded_outbox_version();
        }
    }

    fn bump_owned_blinded_outbox_version(&mut self) {
        self.owned_blinded_outbox_version = self.owned_blinded_outbox_version.saturating_add(1);
    }

    fn build_burn_with_fee_rate(
        &self,
        amount: Amount,
        fee_per_byte: Amount,
    ) -> Result<(Transaction, FeeEstimate)> {
        let (built, estimate) = self.build_blinded_burn_with_fee_rate(amount, fee_per_byte)?;
        Ok((built.payload, estimate))
    }

    fn build_blinded_burn_with_fee_rate(
        &self,
        amount: Amount,
        fee_per_byte: Amount,
    ) -> Result<(BuiltBlindedTransaction, FeeEstimate)> {
        let ledger = self.wallet_build_ledger()?;
        self.build_blinded_burn_with_fee_rate_on_ledger(&ledger, amount, fee_per_byte)
    }

    fn build_blinded_burn_with_fee_rate_on_ledger(
        &self,
        ledger: &Ledger,
        amount: Amount,
        fee_per_byte: Amount,
    ) -> Result<(BuiltBlindedTransaction, FeeEstimate)> {
        let expires_at_height = self.default_blinded_transaction_expiry_height();
        converge_fee_by_byte(fee_per_byte, |fee| {
            let tx = ledger.build_burn(self.wallet.unlocked()?, amount, fee)?;
            ledger.build_blinded_transaction(self.wallet.unlocked()?, tx, expires_at_height)
        })
    }

    fn build_blinded_burn_with_fee_on_ledger(
        &self,
        ledger: &Ledger,
        amount: Amount,
        fee: Amount,
    ) -> Result<BuiltBlindedTransaction> {
        let expires_at_height = self.default_blinded_transaction_expiry_height();
        let tx = ledger.build_burn(self.wallet.unlocked()?, amount, fee)?;
        ledger.build_blinded_transaction(self.wallet.unlocked()?, tx, expires_at_height)
    }

    fn build_transfer_with_fee_rate(
        &self,
        to: impl Into<String>,
        amount: Amount,
        fee_per_byte: Amount,
        outpoints: &[OutPoint],
    ) -> Result<(Transaction, FeeEstimate)> {
        let (built, estimate) =
            self.build_blinded_transfer_with_fee_rate(to, amount, fee_per_byte, outpoints)?;
        Ok((built.payload, estimate))
    }

    fn build_blinded_transfer_with_fee_rate(
        &self,
        to: impl Into<String>,
        amount: Amount,
        fee_per_byte: Amount,
        outpoints: &[OutPoint],
    ) -> Result<(BuiltBlindedTransaction, FeeEstimate)> {
        let to = to.into();
        let ledger = self.wallet_build_ledger()?;
        let expires_at_height = self.default_blinded_transaction_expiry_height();
        converge_fee_by_byte(fee_per_byte, |fee| {
            let tx = if outpoints.is_empty() {
                ledger.build_transfer(self.wallet.unlocked()?, to.clone(), amount, fee)
            } else {
                ledger.build_transfer_with_inputs(
                    self.wallet.unlocked()?,
                    to.clone(),
                    amount,
                    fee,
                    outpoints,
                )
            }?;
            ledger.build_blinded_transaction(self.wallet.unlocked()?, tx, expires_at_height)
        })
    }

    fn build_mine_estimate(&self) -> Result<(Transaction, FeeEstimate)> {
        let tx = self.ledger.build_mine(self.wallet.address())?;
        Ok((
            tx.clone(),
            FeeEstimate {
                bytes: tx.economic_size_bytes(),
                fee: tx.fee(),
            },
        ))
    }

    fn wallet_build_ledger(&self) -> Result<Ledger> {
        let mut ledger = self.ledger.clone();
        self.reserve_local_block_anchor_inputs(&mut ledger)?;
        self.queue_owned_blinded_payloads(&mut ledger)?;
        Ok(ledger)
    }

    fn wallet_anchor_build_ledger(&self) -> Result<Ledger> {
        let mut ledger = self.ledger.clone();
        self.reserve_local_block_anchor_inputs(&mut ledger)?;
        ledger.clear_pending_transactions();
        ledger.clear_pending_blinded_transactions();
        Ok(ledger)
    }

    fn queue_owned_blinded_payloads(&self, ledger: &mut Ledger) -> Result<()> {
        for (commitment, payload) in &self.owned_blinded_payloads {
            if self.ledger.has_unrevealed_blinded_transaction(commitment)
                && !ledger.has_transaction(payload.signature())
            {
                let _ = ledger.submit_transaction(payload.clone());
            }
        }
        Ok(())
    }

    fn queue_local_block_anchor(&self, ledger: &mut Ledger) -> Result<()> {
        let Some((height, burn)) = &self.local_block_anchor_burn else {
            return Ok(());
        };
        if *height == ledger.height() && !ledger.has_transaction(burn.signature()) {
            let _ = ledger.submit_transaction(burn.clone())?;
        }
        Ok(())
    }

    fn reserve_local_block_anchor_inputs(&self, ledger: &mut Ledger) -> Result<()> {
        let Some((height, burn)) = &self.local_block_anchor_burn else {
            return Ok(());
        };
        if *height == ledger.height() && !ledger.has_transaction(burn.signature()) {
            let _ = ledger.reserve_transaction_inputs(burn);
        }
        Ok(())
    }

    pub fn mine_one(&mut self) -> Result<Block> {
        self.mine_one_at(now_ms())
    }

    pub fn automatic_mine_once(&mut self, timestamp_ms: u64) -> AutoMineOutcome {
        let plan = self.prepare_automatic_mining(timestamp_ms);
        let mut outcome = AutoMineOutcome {
            pow_mined: plan.pow_mined,
            burned: plan.burned,
            block: None,
            skipped_reason: plan.skipped_reason,
        };

        let Some(work) = plan.work else {
            return outcome;
        };
        let vdf_output = run_vdf(work.vdf_seed(), work.vdf_rounds());
        match self.complete_prepared_block_at(work, vdf_output, timestamp_ms) {
            Ok(block) => {
                outcome.block = Some(block);
                outcome.skipped_reason = None;
            }
            Err(error) => {
                outcome.skipped_reason = Some(format!("{error:#}"));
            }
        }

        outcome
    }

    pub fn prepare_automatic_mining(&mut self, timestamp_ms: u64) -> AutoMinePlan {
        let mut plan = AutoMinePlan {
            pow_mined: None,
            burned: None,
            work: None,
            skipped_reason: None,
        };

        if self.wallet.is_locked() {
            if self.pow_mining_enabled {
                self.last_auto_pow_mine_status = Some("wallet is locked".to_string());
            }
            return AutoMinePlan {
                pow_mined: None,
                burned: None,
                work: None,
                skipped_reason: Some("wallet is locked".to_string()),
            };
        }

        let pow_error = match self.prepare_automatic_pow_mine() {
            Ok(tx) => {
                plan.pow_mined = tx;
                None
            }
            Err(error) => {
                let message = format!("automatic PoW mining failed: {error:#}");
                self.last_auto_pow_mine_status = Some(message.clone());
                Some(message)
            }
        };

        if !self.automatic_mining_enabled {
            plan.skipped_reason =
                Some(pow_error.unwrap_or_else(|| "automatic mining is off".to_string()));
            return plan;
        }

        if let Some(error) = pow_error {
            plan.skipped_reason = Some(error);
            return plan;
        }

        match self.prepare_automatic_burn(timestamp_ms) {
            Ok(tx) => plan.burned = tx,
            Err(error) => {
                plan.skipped_reason = Some(format!("automatic burn failed: {error:#}"));
                return plan;
            }
        }

        if let Err(error) = self.publish_reveal_bundle_for_next_block() {
            plan.skipped_reason = Some(format!("{error:#}"));
            return plan;
        }

        let wallet_rank = self
            .ledger
            .finalizer_rank_for_next_block(self.wallet.address());
        if let Some(rank) = wallet_rank {
            if !self.wallet_rank_runs_vdf(rank) {
                plan.skipped_reason = Some(format!(
                    "wallet finalizer rank {rank} is outside the top {}% VDF threshold",
                    self.recovery_vdf_top_rank_percent
                ));
                return plan;
            }
        } else {
            if self.should_prepare_recovery_vdf(timestamp_ms) {
                match self.prepare_recovery_block_with_local_anchor(timestamp_ms) {
                    Ok(work) => {
                        plan.work = Some(work);
                    }
                    Err(error) => {
                        plan.skipped_reason = Some(format!("{error:#}"));
                    }
                }
            } else {
                let selected_leader = self.ledger.expected_leader_for_next_block();
                plan.skipped_reason = selected_leader.map(|leader| {
                    format!("wallet is waiting for selected finalizer {leader} to finish the VDF")
                });
            }
            return plan;
        }

        match self.prepare_next_block_with_local_anchor(timestamp_ms) {
            Ok(work) => {
                plan.work = Some(work);
            }
            Err(error) => {
                plan.skipped_reason = Some(format!("{error:#}"));
            }
        }

        plan
    }

    pub fn prepare_automatic_pow_mining(&mut self) -> Result<Option<Transaction>> {
        if self.wallet.is_locked() {
            if self.pow_mining_enabled {
                self.last_auto_pow_mine_status = Some("wallet is locked".to_string());
            }
            return Ok(None);
        }
        if self.pow_mining_enabled && !self.has_real_chain() {
            self.last_auto_pow_mine_status =
                Some("waiting for a real chain before PoW mining can start".to_string());
            self.auto_pow_mine_cursor = None;
            return Ok(None);
        }

        self.prepare_automatic_pow_mine()
    }

    pub fn prepare_automatic_finalization(&mut self, timestamp_ms: u64) -> AutoMinePlan {
        let mut plan = AutoMinePlan {
            pow_mined: None,
            burned: None,
            work: None,
            skipped_reason: None,
        };

        if self.wallet.is_locked() {
            plan.skipped_reason = Some("wallet is locked".to_string());
            return plan;
        }

        if !self.automatic_mining_enabled {
            plan.skipped_reason = Some("automatic mining is off".to_string());
            return plan;
        }

        match self.prepare_automatic_burn(timestamp_ms) {
            Ok(tx) => plan.burned = tx,
            Err(error) => {
                plan.skipped_reason = Some(format!("automatic burn failed: {error:#}"));
                return plan;
            }
        }

        if let Err(error) = self.publish_reveal_bundle_for_next_block() {
            plan.skipped_reason = Some(format!("{error:#}"));
            return plan;
        }

        let wallet_rank = self
            .ledger
            .finalizer_rank_for_next_block(self.wallet.address());
        if let Some(rank) = wallet_rank {
            if !self.wallet_rank_runs_vdf(rank) {
                plan.skipped_reason = Some(format!(
                    "wallet finalizer rank {rank} is outside the top {}% VDF threshold",
                    self.recovery_vdf_top_rank_percent
                ));
                return plan;
            }
        } else {
            if self.should_prepare_recovery_vdf(timestamp_ms) {
                match self.prepare_recovery_block_with_local_anchor(timestamp_ms) {
                    Ok(work) => {
                        plan.work = Some(work);
                    }
                    Err(error) => {
                        plan.skipped_reason = Some(format!("{error:#}"));
                    }
                }
            } else {
                let selected_leader = self.ledger.expected_leader_for_next_block();
                plan.skipped_reason = selected_leader.map(|leader| {
                    format!("wallet is waiting for selected finalizer {leader} to finish the VDF")
                });
            }
            return plan;
        }

        match self.prepare_next_block_with_local_anchor(timestamp_ms) {
            Ok(work) => {
                plan.work = Some(work);
            }
            Err(error) => {
                plan.skipped_reason = Some(format!("{error:#}"));
            }
        }

        plan
    }

    pub fn record_automatic_pow_mining_error(&mut self, message: String) {
        self.last_auto_pow_mine_status = Some(message);
    }

    fn prepare_automatic_pow_mine(&mut self) -> Result<Option<Transaction>> {
        if !self.pow_mining_enabled {
            self.last_auto_pow_mine_status = None;
            self.auto_pow_mine_cursor = None;
            return Ok(None);
        }
        let anchor = self
            .ledger
            .chain()
            .last()
            .map(|block| block.hash.clone())
            .context("ledger has no anchor block")?;
        let wallet_address = self.wallet.address().to_string();
        let needs_cursor = self
            .auto_pow_mine_cursor
            .as_ref()
            .is_none_or(|cursor| cursor.anchor != anchor);
        if needs_cursor {
            self.auto_pow_mine_cursor = Some(AutoPowMineCursor {
                salt: auto_pow_salt(&wallet_address, &anchor),
                anchor: anchor.clone(),
                next_nonce: 0,
                searched: 0,
            });
        }
        let cursor = self
            .auto_pow_mine_cursor
            .as_ref()
            .context("automatic PoW cursor was not initialized")?
            .clone();
        let budget = AUTO_POW_NONCE_ATTEMPTS_PER_WORKER_TICK
            .saturating_mul(u64::from(self.pow_mining_workers));
        let mut remaining = budget;
        let mut next_nonce = cursor.next_nonce;
        let mut tick_attempts = 0_u64;
        let mut queued = 0_u64;
        let mut first_tx = None;
        while remaining > 0 {
            let outcome = self.wallet_build_ledger()?.search_mine(
                wallet_address.clone(),
                cursor.salt,
                next_nonce,
                remaining,
            )?;
            remaining = remaining.saturating_sub(outcome.attempts);
            tick_attempts = tick_attempts.saturating_add(outcome.attempts);
            next_nonce = outcome.next_nonce;
            let Some(tx) = outcome.transaction else {
                break;
            };
            self.submit_public_mine_action(tx.clone())?;
            queued = queued.saturating_add(1);
            first_tx.get_or_insert(tx);
        }

        let mut searched = tick_attempts;
        if let Some(cursor) = &mut self.auto_pow_mine_cursor {
            if cursor.anchor == anchor {
                cursor.next_nonce = next_nonce;
                cursor.searched = cursor.searched.saturating_add(tick_attempts);
                searched = cursor.searched;
            }
        }
        let Some(tx) = first_tx else {
            self.last_auto_pow_mine_status = Some(format!(
                "searched {searched} PoW nonces for the current tip; no proof yet"
            ));
            return Ok(None);
        };
        self.last_auto_pow_mine_anchor = Some(anchor);
        self.last_auto_pow_mine_status = Some(format!(
            "queued {queued} mine action{} after {searched} PoW nonce attempts for the current tip",
            if queued == 1 { "" } else { "s" }
        ));
        Ok(Some(tx))
    }

    fn prepare_automatic_burn(&mut self, timestamp_ms: u64) -> Result<Option<Transaction>> {
        let current_height = self.ledger.status().height;
        if !self.automatic_mining_enabled {
            return Ok(None);
        }
        let anchor_burn = self.prepare_automatic_anchor_burn(timestamp_ms)?;
        if self.burn_per_block == 0 {
            self.last_auto_burn_height = Some(current_height);
            return Ok(anchor_burn);
        }
        if self.last_auto_burn_height == Some(current_height) {
            return Ok(anchor_burn);
        }

        let fee_per_byte = self.burn_fee;
        let balance = self.ledger.balance_of(self.wallet.address());
        let ledger = self.wallet_build_ledger()?;
        let best = self.best_automatic_burn_on_ledger(&ledger, fee_per_byte, balance);
        let Some(tx) = best else {
            self.last_auto_burn_height = Some(current_height);
            return Ok(anchor_burn);
        };
        let burn = tx.payload.clone();
        self.submit_owned_blinded_transaction(tx)?;
        self.last_auto_burn_height = Some(current_height);
        Ok(Some(burn))
    }

    fn prepare_automatic_anchor_burn(&mut self, timestamp_ms: u64) -> Result<Option<Transaction>> {
        let current_height = self.ledger.status().height;
        if !self.automatic_burn_needs_plaintext_anchor(timestamp_ms) {
            return Ok(None);
        }
        if self
            .local_block_anchor_burn
            .as_ref()
            .is_some_and(|(height, _)| *height == current_height)
        {
            return Ok(None);
        }
        if self.last_auto_anchor_burn_height == Some(current_height) {
            return Ok(None);
        }

        let ledger = self.wallet_anchor_build_ledger()?;
        let wallet = self.wallet.unlocked()?;
        let required = AUTO_BLOCK_ANCHOR_BURN_AMOUNT
            .checked_add(AUTO_BLOCK_ANCHOR_BURN_FEE)
            .context("automatic finalizer anchor burn amount plus fee overflows")?;
        let outpoint = ledger
            .available_utxos_for_address(wallet.address())?
            .into_iter()
            .filter(|(_, output)| output.amount >= required)
            .min_by_key(|(_, output)| output.amount)
            .map(|(outpoint, _)| outpoint);
        let burn = match outpoint {
            Some(outpoint) => ledger.build_burn_with_inputs(
                wallet,
                AUTO_BLOCK_ANCHOR_BURN_AMOUNT,
                AUTO_BLOCK_ANCHOR_BURN_FEE,
                &[outpoint],
            ),
            None => ledger.build_burn(
                wallet,
                AUTO_BLOCK_ANCHOR_BURN_AMOUNT,
                AUTO_BLOCK_ANCHOR_BURN_FEE,
            ),
        };
        let burn = match burn {
            Ok(burn) => burn,
            Err(error) => {
                self.last_auto_anchor_burn_height = Some(current_height);
                return Err(error).context("automatic finalizer anchor burn failed");
            }
        };
        self.local_block_anchor_burn = Some((current_height, burn.clone()));
        self.last_auto_anchor_burn_height = Some(current_height);
        Ok(Some(burn))
    }

    fn best_automatic_burn_on_ledger(
        &self,
        ledger: &Ledger,
        fee_per_byte: Amount,
        balance: Amount,
    ) -> Option<BuiltBlindedTransaction> {
        let target = self.burn_per_block.min(balance);
        if target == 0 {
            return None;
        }
        let exact_at_fee_rate =
            self.build_blinded_burn_with_fee_rate_on_ledger(ledger, target, fee_per_byte);
        if let Ok((built, estimate)) = exact_at_fee_rate {
            if target
                .checked_add(estimate.fee)
                .is_some_and(|required| required <= balance)
            {
                return Some(built);
            }
        }
        if self.burn_per_block <= balance {
            let affordable_fee = balance.saturating_sub(target);
            if let Ok(built) =
                self.build_blinded_burn_with_fee_on_ledger(ledger, target, affordable_fee)
            {
                return Some(built);
            }
        }

        let mut low = 1;
        let mut high = target;
        let mut best = None;
        while low <= high {
            let amount = low + (high - low) / 2;
            match self.build_blinded_burn_with_fee_rate_on_ledger(ledger, amount, fee_per_byte) {
                Ok((built, estimate)) => {
                    let fits = amount
                        .checked_add(estimate.fee)
                        .is_some_and(|required| required <= balance);
                    if fits {
                        best = Some(built);
                        if amount == Amount::MAX {
                            break;
                        }
                        low = amount + 1;
                    } else {
                        high = amount.saturating_sub(1);
                    }
                }
                Err(_) => {
                    high = amount.saturating_sub(1);
                }
            }
        }
        best
    }

    fn automatic_burn_needs_plaintext_anchor(&self, timestamp_ms: u64) -> bool {
        self.ledger
            .finalizer_rank_for_next_block(self.wallet.address())
            .is_some_and(|rank| self.wallet_rank_runs_vdf(rank))
            || self.should_prepare_recovery_vdf(timestamp_ms)
            || timestamp_ms.saturating_add(AUTO_PLAINTEXT_BURN_BEFORE_RECOVERY_MS)
                >= self.ledger.recovery_block_min_timestamp()
    }

    fn wallet_rank_runs_vdf(&self, rank: u32) -> bool {
        let rank_count = self.ledger.finalizer_rank_count_for_next_block();
        let allowed =
            allowed_recovery_vdf_rank_count(rank_count, self.recovery_vdf_top_rank_percent);
        usize::try_from(rank).is_ok_and(|rank| rank < allowed)
    }

    fn should_prepare_recovery_vdf(&self, timestamp_ms: u64) -> bool {
        if !self.ledger.recovery_block_available_at(timestamp_ms) {
            return false;
        }
        if self.recovery_vdf_top_rank_percent == 100 {
            return true;
        }
        if self.recovery_vdf_top_rank_percent == 0 {
            return false;
        }
        if self.ledger.finalizer_rank_count_for_next_block() > 0 {
            return false;
        }
        let tip_hash = self.ledger.status().tip_hash;
        recovery_vdf_sample_percent(self.wallet.address(), tip_hash.as_str())
            < self.recovery_vdf_top_rank_percent
    }

    fn prepare_next_block_with_local_anchor(&self, timestamp_ms: u64) -> Result<PreparedBlock> {
        let (ledger, required_burn_signature) = self.ledger_with_local_block_anchor();
        ledger.prepare_next_block_with_required_burn_and_reveal_bundles(
            self.wallet.address(),
            timestamp_ms,
            self.usable_reveal_bundles(),
            required_burn_signature.as_deref(),
        )
    }

    fn prepare_recovery_block_with_local_anchor(&self, timestamp_ms: u64) -> Result<PreparedBlock> {
        let (ledger, required_burn_signature) = self.ledger_with_local_block_anchor();
        ledger.prepare_recovery_block_with_required_burn_and_reveal_bundles(
            self.wallet.address(),
            timestamp_ms,
            self.usable_reveal_bundles(),
            required_burn_signature.as_deref(),
        )
    }

    fn ledger_with_local_block_anchor(&self) -> (Ledger, Option<String>) {
        let mut ledger = self.ledger.clone();
        let Some((height, burn)) = &self.local_block_anchor_burn else {
            return (ledger, None);
        };
        if *height == ledger.height() && !ledger.has_transaction(burn.signature()) {
            ledger.drop_pending_blinded_conflicting_with_transaction(burn);
            if ledger.submit_transaction(burn.clone()).is_ok() {
                return (ledger, Some(burn.signature().to_string()));
            }
        }
        (ledger, None)
    }

    fn clear_stale_local_block_anchor(&mut self) {
        if self
            .local_block_anchor_burn
            .as_ref()
            .is_some_and(|(height, _)| *height != self.ledger.height())
        {
            self.local_block_anchor_burn = None;
        }
    }

    pub fn mine_one_at(&mut self, timestamp_ms: u64) -> Result<Block> {
        self.publish_reveal_bundle_for_next_block()?;
        let work = self.prepare_next_block_with_local_anchor(timestamp_ms)?;
        let vdf_output = run_vdf(work.vdf_seed(), work.vdf_rounds());
        self.complete_prepared_block_at(work, vdf_output, timestamp_ms)
    }

    pub fn complete_prepared_block(
        &mut self,
        work: PreparedBlock,
        vdf_output: String,
    ) -> Result<Block> {
        self.complete_prepared_block_at(work, vdf_output, now_ms())
    }

    pub fn complete_prepared_block_at(
        &mut self,
        work: PreparedBlock,
        vdf_output: String,
        timestamp_ms: u64,
    ) -> Result<Block> {
        let block = work.finish_at(self.wallet.unlocked()?, vdf_output, timestamp_ms);
        self.ledger.apply_locally_mined_block(block.clone())?;
        self.clear_stale_local_block_anchor();
        self.prune_reveal_bundles();
        self.prune_owned_blinded_payloads_for_block(&block);
        self.outbox.push(GossipEnvelope::Block(block.clone()));
        self.publish_owned_reveals_for_block(&block)?;
        Ok(block)
    }

    pub fn receive(&mut self, envelope: GossipEnvelope) -> Result<()> {
        match envelope {
            GossipEnvelope::Hello(_)
            | GossipEnvelope::PeerStatus { .. }
            | GossipEnvelope::ChainSnapshotRequest
            | GossipEnvelope::BlockRangeRequest { .. }
            | GossipEnvelope::BlockRequest { .. }
            | GossipEnvelope::Inventory { .. } => Ok(()),
            GossipEnvelope::BlindedTransaction(tx) => self.receive_blinded_transaction(tx),
            GossipEnvelope::BlindedTransactions { transactions } => {
                for tx in transactions {
                    self.receive_blinded_transaction(tx)?;
                }
                Ok(())
            }
            GossipEnvelope::MineAction(tx) => self.receive_mine_action(tx),
            GossipEnvelope::MineActions { transactions } => {
                for tx in transactions {
                    self.receive_mine_action(tx)?;
                }
                Ok(())
            }
            GossipEnvelope::BlindedReveal(reveal) => self.receive_blinded_reveal(reveal),
            GossipEnvelope::BlindedReveals { reveals } => {
                let mut added = false;
                for reveal in reveals {
                    added |= self.receive_blinded_reveal_without_bundle_publish(reveal)?;
                }
                if added {
                    self.publish_reveal_bundle_for_next_block()?;
                }
                Ok(())
            }
            GossipEnvelope::RevealBundle(bundle) => self.receive_reveal_bundle(bundle),
            GossipEnvelope::RevealBundles { bundles } => {
                for bundle in bundles {
                    self.receive_reveal_bundle(bundle)?;
                }
                Ok(())
            }
            GossipEnvelope::Block(block) => {
                let previous_height = self.ledger.height();
                self.ledger.apply_block(block.clone())?;
                if self.ledger.height() > previous_height {
                    self.clear_stale_local_block_anchor();
                    self.prune_reveal_bundles();
                    self.prune_owned_blinded_payloads_for_block(&block);
                    self.publish_owned_reveals_for_block(&block)?;
                    self.outbox.push(GossipEnvelope::Block(block));
                }
                Ok(())
            }
            GossipEnvelope::Blocks { blocks } => {
                let mut imported = Vec::new();
                for block in blocks {
                    let previous_height = self.ledger.height();
                    self.ledger.apply_block(block.clone())?;
                    if self.ledger.height() > previous_height {
                        self.clear_stale_local_block_anchor();
                        self.prune_reveal_bundles();
                        self.prune_owned_blinded_payloads_for_block(&block);
                        self.publish_owned_reveals_for_block(&block)?;
                        imported.push(block);
                    }
                }
                for block in imported {
                    self.outbox.push(GossipEnvelope::Block(block));
                }
                Ok(())
            }
            GossipEnvelope::ChainSnapshot(snapshot) => self.import_chain_snapshot(snapshot),
            GossipEnvelope::PeerAnnouncement { .. }
            | GossipEnvelope::PeerVerificationChallenge { .. }
            | GossipEnvelope::PeerVerificationResponse { .. }
            | GossipEnvelope::PeerList { .. } => Ok(()),
        }
    }

    pub(crate) fn receive_preverified_block_at(&mut self, block: Block, now_ms: u64) -> Result<()> {
        let previous_height = self.ledger.height();
        self.ledger
            .apply_preverified_block_at(block.clone(), now_ms)?;
        if self.ledger.height() > previous_height {
            self.clear_stale_local_block_anchor();
            self.prune_reveal_bundles();
            self.prune_owned_blinded_payloads_for_block(&block);
            self.publish_owned_reveals_for_block(&block)?;
            self.outbox.push(GossipEnvelope::Block(block));
        }
        Ok(())
    }

    pub(crate) fn block_requires_vdf_verification_at(
        &self,
        block: &Block,
        now_ms: u64,
    ) -> Result<bool> {
        self.ledger
            .block_requires_vdf_verification_at(block, now_ms)
    }

    pub fn import_chain_snapshot(&mut self, snapshot: ChainSnapshot) -> Result<()> {
        let previous_height = self.ledger.height();
        let imported = self.ledger.extend_from_snapshot(snapshot)?;
        if imported {
            self.last_auto_burn_height = None;
            self.last_auto_anchor_burn_height = None;
            self.last_auto_pow_mine_anchor = None;
            self.last_auto_pow_mine_status = None;
            self.auto_pow_mine_cursor = None;
            self.clear_stale_local_block_anchor();
            self.prune_reveal_bundles();
            self.enqueue_imported_blocks(previous_height)?;
        }
        Ok(())
    }

    pub(crate) fn import_verified_ledger(&mut self, ledger: Ledger) -> Result<bool> {
        let replaces_setup_placeholder = self.ledger.is_setup_placeholder()
            && ledger.genesis_hash() != self.ledger.genesis_hash();
        if ledger.genesis_hash() != self.ledger.genesis_hash() && !replaces_setup_placeholder {
            anyhow::bail!("chain snapshot genesis does not match local chain");
        }
        let previous_height = self.ledger.height();
        if !replaces_setup_placeholder && ledger.height() <= previous_height {
            return Ok(false);
        }

        self.ledger = ledger;
        self.last_auto_burn_height = None;
        self.last_auto_anchor_burn_height = None;
        self.last_auto_pow_mine_anchor = None;
        self.last_auto_pow_mine_status = None;
        self.auto_pow_mine_cursor = None;
        self.clear_stale_local_block_anchor();
        self.prune_reveal_bundles();
        self.enqueue_imported_blocks(previous_height)?;
        Ok(true)
    }

    pub fn drain_outbox(&mut self) -> Vec<GossipEnvelope> {
        std::mem::take(&mut self.outbox)
    }

    fn enqueue_imported_blocks(&mut self, previous_height: u64) -> Result<()> {
        if self.ledger.height() <= previous_height {
            return Ok(());
        }
        let blocks = self
            .ledger
            .blocks_from(previous_height + 1, IMPORT_REBROADCAST_LIMIT);
        for block in &blocks {
            self.prune_reveal_bundles();
            self.prune_owned_blinded_payloads_for_block(block);
            self.publish_owned_reveals_for_block(block)?;
        }
        if !blocks.is_empty() {
            self.outbox.push(GossipEnvelope::Blocks { blocks });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PeerBook {
    peers: BTreeMap<String, PeerInfo>,
}

impl PeerBook {
    pub fn from_addresses(addresses: Vec<String>) -> Self {
        let mut book = Self::default();
        for address in addresses {
            book.add_peer(address);
        }
        book
    }

    pub fn add_peer(&mut self, address: impl Into<String>) {
        let address = address.into();
        let peer = self
            .peers
            .entry(address.clone())
            .or_insert_with(|| PeerInfo::new(address, PeerDirection::Outbound));
        if peer.direction == PeerDirection::Inbound {
            peer.direction = PeerDirection::Outbound;
        }
    }

    pub fn observe_inbound_peer(&mut self, address: impl Into<String>) {
        let address = address.into();
        self.peers
            .entry(address.clone())
            .or_insert_with(|| PeerInfo::new(address, PeerDirection::Inbound));
    }

    pub fn replace_peer_address(&mut self, from: &str, to: impl Into<String>) {
        let to = to.into();
        if from == to {
            if !self.peers.contains_key(from) {
                self.add_peer(to);
            }
            return;
        }

        let Some(from_peer) = self.peers.remove(from) else {
            self.add_peer(to);
            return;
        };

        let to_peer = self
            .peers
            .entry(to.clone())
            .or_insert_with(|| PeerInfo::new(to, from_peer.direction.clone()));
        if from_peer.direction == PeerDirection::Outbound {
            to_peer.direction = PeerDirection::Outbound;
        }
        to_peer.messages_sent = to_peer
            .messages_sent
            .saturating_add(from_peer.messages_sent);
        to_peer.messages_received = to_peer
            .messages_received
            .saturating_add(from_peer.messages_received);
        to_peer.last_known_height = to_peer.last_known_height.or(from_peer.last_known_height);
        to_peer.last_known_tip_hash = to_peer
            .last_known_tip_hash
            .clone()
            .or(from_peer.last_known_tip_hash);
        if from_peer.last_clock_observed_ms > to_peer.last_clock_observed_ms {
            to_peer.last_clock_offset_ms = from_peer.last_clock_offset_ms;
            to_peer.last_clock_offset_accepted = from_peer.last_clock_offset_accepted;
            to_peer.last_clock_observed_ms = from_peer.last_clock_observed_ms;
        }
        to_peer.last_contact_ms = to_peer.last_contact_ms.max(from_peer.last_contact_ms);
        to_peer.last_success_ms = to_peer.last_success_ms.max(from_peer.last_success_ms);
        to_peer.last_error_ms = to_peer.last_error_ms.max(from_peer.last_error_ms);
        if to_peer.last_error.is_none() {
            to_peer.last_error = from_peer.last_error;
        }
        to_peer.misbehavior_score = to_peer
            .misbehavior_score
            .saturating_add(from_peer.misbehavior_score);
        to_peer.banned_until_ms = to_peer.banned_until_ms.max(from_peer.banned_until_ms);
        if to_peer.ban_reason.is_none() {
            to_peer.ban_reason = from_peer.ban_reason;
        }
    }

    pub fn remove_peer(&mut self, address: &str) -> bool {
        if self
            .peers
            .get(address)
            .is_some_and(|peer| peer.direction != PeerDirection::Inbound)
        {
            self.peers.remove(address);
            true
        } else {
            false
        }
    }

    pub fn is_configured_outbound(&self, address: &str) -> bool {
        self.peers
            .get(address)
            .is_some_and(|peer| peer.direction != PeerDirection::Inbound)
    }

    pub fn addresses(&self) -> Vec<String> {
        self.peers
            .values()
            .filter(|peer| peer.direction != PeerDirection::Inbound)
            .map(|peer| peer.address.clone())
            .collect()
    }

    pub fn connectable_addresses_at(&self, now_ms: u64) -> Vec<String> {
        self.peers
            .values()
            .filter(|peer| peer.direction != PeerDirection::Inbound)
            .filter(|peer| !peer.is_banned_at(now_ms))
            .map(|peer| peer.address.clone())
            .collect()
    }

    pub fn addresses_except(&self, excluded: &str) -> Vec<String> {
        self.connectable_addresses_at(now_ms())
            .into_iter()
            .filter(|address| address != excluded)
            .collect()
    }

    pub fn list(&self) -> Vec<PeerInfo> {
        self.peers.values().cloned().collect()
    }

    pub fn record_sent(&mut self, address: &str, count: u64) {
        let now = now_ms();
        let peer = self.ensure(address, PeerDirection::Outbound);
        peer.messages_sent += count;
        peer.last_contact_ms = Some(now);
        peer.last_success_ms = Some(now);
        if !peer.is_banned_at(now) {
            peer.last_error = None;
            peer.clear_misbehavior();
        }
    }

    pub fn record_status(&mut self, address: &str, height: u64, tip_hash: String) {
        let now = now_ms();
        let peer = self.ensure(address, PeerDirection::Outbound);
        peer.last_known_height = Some(height);
        peer.last_known_tip_hash = Some(tip_hash);
        peer.last_contact_ms = Some(now);
        peer.last_success_ms = Some(now);
        if !peer.is_banned_at(now) {
            peer.last_error = None;
            peer.clear_misbehavior();
        }
    }

    pub fn record_clock_observation(
        &mut self,
        address: &str,
        direction: PeerDirection,
        remote_time_ms: u64,
        local_receive_time_ms: u64,
    ) {
        if remote_time_ms == 0 {
            return;
        }
        let offset = remote_time_ms as i128 - local_receive_time_ms as i128;
        let offset = offset.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
        let accepted = offset.abs() <= PEER_CLOCK_OFFSET_ACCEPTANCE_MS;
        let peer = self.ensure(address, direction);
        peer.last_clock_offset_ms = Some(offset);
        peer.last_clock_offset_accepted = Some(accepted);
        peer.last_clock_observed_ms = Some(local_receive_time_ms);
    }

    pub fn network_time_offset_ms_at(&self, now_ms: u64) -> Option<i64> {
        median_i64(
            self.peers
                .values()
                .filter(|peer| !peer.is_banned_at(now_ms))
                .filter(|peer| peer.last_error.is_none())
                .filter(|peer| peer.last_clock_offset_accepted == Some(true))
                .filter(|peer| {
                    peer.last_clock_observed_ms.is_some_and(|observed_ms| {
                        now_ms.saturating_sub(observed_ms) <= PEER_CLOCK_OFFSET_STALE_MS
                    })
                })
                .filter_map(|peer| peer.last_clock_offset_ms)
                .collect(),
        )
    }

    pub fn adjusted_time_ms_at(&self, now_ms: u64) -> u64 {
        match self.network_time_offset_ms_at(now_ms) {
            Some(offset) if offset >= 0 => now_ms.saturating_add(offset as u64),
            Some(offset) => now_ms.saturating_sub(offset.unsigned_abs()),
            None => now_ms,
        }
    }

    pub fn bad_clock_peer_count_at(&self, now_ms: u64) -> usize {
        self.peers
            .values()
            .filter(|peer| !peer.is_banned_at(now_ms))
            .filter(|peer| {
                peer.last_clock_observed_ms.is_some_and(|observed_ms| {
                    now_ms.saturating_sub(observed_ms) <= PEER_CLOCK_OFFSET_STALE_MS
                })
            })
            .filter(|peer| peer.last_clock_offset_accepted == Some(false))
            .count()
    }

    pub fn record_error(&mut self, address: &str, error: impl Into<String>) {
        let now = now_ms();
        let peer = self.ensure(address, PeerDirection::Outbound);
        peer.last_contact_ms = Some(now);
        peer.last_error_ms = Some(now);
        peer.last_error = Some(error.into());
    }

    pub fn record_inbound_error(&mut self, address: &str, error: impl Into<String>) {
        let now = now_ms();
        let peer = self.ensure(address, PeerDirection::Inbound);
        peer.last_contact_ms = Some(now);
        peer.last_error_ms = Some(now);
        peer.last_error = Some(error.into());
    }

    pub fn record_received(&mut self, address: &str, count: u64) {
        let now = now_ms();
        let peer = self.ensure(address, PeerDirection::Inbound);
        peer.messages_received += count;
        peer.last_contact_ms = Some(now);
        peer.last_success_ms = Some(now);
        if !peer.is_banned_at(now) {
            peer.last_error = None;
            peer.clear_misbehavior();
        }
    }

    pub fn record_misbehavior(&mut self, address: &str, reason: impl Into<String>) {
        self.record_misbehavior_at(address, reason, now_ms());
    }

    pub fn record_misbehavior_at(&mut self, address: &str, reason: impl Into<String>, now_ms: u64) {
        self.record_misbehavior_with_direction(address, reason, now_ms, PeerDirection::Outbound);
    }

    pub fn record_inbound_misbehavior(&mut self, address: &str, reason: impl Into<String>) {
        self.record_misbehavior_with_direction(address, reason, now_ms(), PeerDirection::Inbound);
    }

    fn record_misbehavior_with_direction(
        &mut self,
        address: &str,
        reason: impl Into<String>,
        now_ms: u64,
        direction: PeerDirection,
    ) {
        let reason = reason.into();
        let peer = self.ensure(address, direction);
        peer.last_contact_ms = Some(now_ms);
        peer.last_error_ms = Some(now_ms);
        peer.last_error = Some(reason.clone());
        peer.misbehavior_score = peer.misbehavior_score.saturating_add(1);
        peer.ban_reason = Some(reason);
        if peer.misbehavior_score >= PEER_MISBEHAVIOR_BAN_SCORE {
            peer.banned_until_ms = Some(now_ms.saturating_add(PEER_MISBEHAVIOR_BAN_MS));
        }
    }

    pub fn is_banned(&self, address: &str) -> bool {
        self.is_banned_at(address, now_ms())
    }

    pub fn is_banned_at(&self, address: &str, now_ms: u64) -> bool {
        self.peers
            .get(address)
            .is_some_and(|peer| peer.is_banned_at(now_ms))
    }

    fn ensure(&mut self, address: &str, direction: PeerDirection) -> &mut PeerInfo {
        self.peers
            .entry(address.to_string())
            .or_insert_with(|| PeerInfo::new(address.to_string(), direction))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PeerInfo {
    pub address: String,
    pub direction: PeerDirection,
    pub messages_sent: u64,
    pub messages_received: u64,
    pub last_known_height: Option<u64>,
    pub last_known_tip_hash: Option<String>,
    #[serde(default)]
    pub last_clock_offset_ms: Option<i64>,
    #[serde(default)]
    pub last_clock_offset_accepted: Option<bool>,
    #[serde(default)]
    pub last_clock_observed_ms: Option<u64>,
    pub last_error: Option<String>,
    pub last_contact_ms: Option<u64>,
    pub last_success_ms: Option<u64>,
    pub last_error_ms: Option<u64>,
    pub misbehavior_score: u32,
    pub banned_until_ms: Option<u64>,
    pub ban_reason: Option<String>,
}

impl PeerInfo {
    fn new(address: String, direction: PeerDirection) -> Self {
        Self {
            address,
            direction,
            messages_sent: 0,
            messages_received: 0,
            last_known_height: None,
            last_known_tip_hash: None,
            last_clock_offset_ms: None,
            last_clock_offset_accepted: None,
            last_clock_observed_ms: None,
            last_error: None,
            last_contact_ms: None,
            last_success_ms: None,
            last_error_ms: None,
            misbehavior_score: 0,
            banned_until_ms: None,
            ban_reason: None,
        }
    }

    pub fn is_banned_at(&self, now_ms: u64) -> bool {
        self.banned_until_ms
            .is_some_and(|banned_until| banned_until > now_ms)
    }

    fn clear_misbehavior(&mut self) {
        self.misbehavior_score = 0;
        self.banned_until_ms = None;
        self.ban_reason = None;
    }
}

fn median_i64(mut values: Vec<i64>) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerDirection {
    Outbound,
    Inbound,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time is before unix epoch")
        .as_millis() as u64
}

fn auto_pow_salt(wallet_address: &str, anchor: &str) -> u64 {
    let digest = Sha256::digest(format!("iuna-auto-pow:{wallet_address}:{anchor}").as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes)
}

fn converge_fee_by_byte(
    fee_per_byte: Amount,
    mut build: impl FnMut(Amount) -> Result<BuiltBlindedTransaction>,
) -> Result<(BuiltBlindedTransaction, FeeEstimate)> {
    let mut fee = 0;
    let mut best = None;
    for _ in 0..64 {
        let built = build(fee)?;
        let bytes = built.transaction.fee_rate_size_bytes();
        let required_fee = fee_per_byte
            .checked_mul(bytes as Amount)
            .context("fee per byte times blinded transaction bytes overflows")?;
        if fee == required_fee {
            return Ok((built, FeeEstimate { bytes, fee }));
        }
        if fee > required_fee
            && best
                .as_ref()
                .is_none_or(|(_, estimate): &(BuiltBlindedTransaction, FeeEstimate)| {
                    fee < estimate.fee
                })
        {
            best = Some((built, FeeEstimate { bytes, fee }));
        }
        fee = required_fee;
    }

    let built = build(fee)?;
    let bytes = built.transaction.fee_rate_size_bytes();
    let required_fee = fee_per_byte
        .checked_mul(bytes as Amount)
        .context("fee per byte times blinded transaction bytes overflows")?;
    if fee >= required_fee {
        if best
            .as_ref()
            .is_none_or(|(_, estimate): &(BuiltBlindedTransaction, FeeEstimate)| fee < estimate.fee)
        {
            best = Some((built, FeeEstimate { bytes, fee }));
        }
        if let Some(best) = best {
            return Ok(best);
        }
    }
    let built = build(required_fee)?;
    let bytes = built.transaction.fee_rate_size_bytes();
    let final_required_fee = fee_per_byte
        .checked_mul(bytes as Amount)
        .context("fee per byte times blinded transaction bytes overflows")?;
    if required_fee < final_required_fee {
        bail!("fee per byte did not converge");
    }
    Ok((
        built,
        FeeEstimate {
            bytes,
            fee: required_fee,
        },
    ))
}

fn transaction_output_total_for_address(transaction: &Transaction, address: &str) -> Amount {
    match transaction {
        Transaction::Transfer { outputs, .. } => outputs,
        Transaction::Burn { change, .. } => change,
        Transaction::Mine { recipient, .. } if recipient == address => return MINE_REWARD,
        Transaction::Mine { .. } => return 0,
    }
    .iter()
    .filter(|output| output.address == address)
    .fold(0_u64, |total, output| total.saturating_add(output.amount))
}

fn transaction_input_total_from_outputs(
    transaction: &Transaction,
    address: &str,
    outputs: &BTreeMap<OutPoint, Amount>,
) -> Amount {
    let inputs = match transaction {
        Transaction::Transfer { inputs, .. } | Transaction::Burn { inputs, .. } => inputs,
        Transaction::Mine { .. } => return 0,
    };
    inputs
        .iter()
        .filter(|input| input.owner == address)
        .filter_map(|input| outputs.get(&input.outpoint))
        .fold(0_u64, |total, amount| total.saturating_add(*amount))
}

fn transaction_input_outpoints(transaction: &Transaction) -> BTreeSet<OutPoint> {
    match transaction {
        Transaction::Transfer { inputs, .. } | Transaction::Burn { inputs, .. } => inputs,
        Transaction::Mine { .. } => return BTreeSet::new(),
    }
    .iter()
    .map(|input| input.outpoint.clone())
    .collect()
}

fn allowed_recovery_vdf_rank_count(rank_count: usize, percent: u8) -> usize {
    if rank_count == 0 || percent == 0 {
        return 0;
    }
    rank_count
        .saturating_mul(usize::from(percent.min(100)))
        .saturating_add(99)
        / 100
}

fn recovery_vdf_sample_percent(address: &str, tip_hash: &str) -> u8 {
    let digest = Sha256::digest(format!("iuna-recovery-vdf-sample:{tip_hash}:{address}"));
    digest[0] % 100
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use crate::domain::{
        FinalizerMode, GenesisBurn, Ledger, MICRO_IUNA, MINE_FINALIZER_FEE, OutPoint,
        RECOVERY_BLOCK_DELAY_MS, Transaction, VDF_TARGET_BLOCK_MS, Wallet, run_vdf,
    };

    use super::{
        DEFAULT_POW_MINING_WORKERS, GossipEnvelope, InMemoryNetwork, MAX_POW_MINING_WORKERS,
        NodeConfig, NodeCore,
    };

    fn wallet_for_address<'a>(wallets: &'a [Wallet], address: &str) -> &'a Wallet {
        wallets
            .iter()
            .find(|wallet| wallet.address() == address)
            .unwrap_or_else(|| panic!("missing wallet for address {address}"))
    }

    fn queue_auto_pow_mine_action(node: &mut NodeCore) -> Transaction {
        node.set_pow_mining_enabled(true);
        (0..10_000)
            .find_map(|timestamp| node.prepare_automatic_mining(timestamp).pow_mined)
            .expect("test node should find a PoW mine action")
    }

    fn assert_block_has_mine_action(block: &crate::domain::Block) {
        assert!(
            block
                .transactions
                .iter()
                .any(|transaction| matches!(transaction, Transaction::Mine { .. })),
            "block {} should include a mine action",
            block.height
        );
    }

    #[test]
    fn same_height_verified_import_does_not_reset_auto_burn_guard() {
        let alice = Wallet::from_seed("same-height-import-alice");
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), MICRO_IUNA);
        let mut node = NodeCore::new(NodeConfig {
            wallet: alice,
            genesis_allocations: allocations,
            vdf_rounds: 10,
            burn_per_block: 1,
            burn_fee: 1,
            pow_mining_workers: 1,
            recovery_vdf_top_rank_percent: 100,
        });

        let first = node.prepare_automatic_mining(1);
        assert!(first.burned.is_some());
        assert_eq!(node.last_auto_burn_height, Some(0));

        let same_height_ledger = node.clone_ledger();
        assert!(!node.import_verified_ledger(same_height_ledger).unwrap());
        assert_eq!(node.last_auto_burn_height, Some(0));

        let second = node.prepare_automatic_mining(2);
        assert!(second.burned.is_none());
    }

    #[test]
    fn automatic_pow_mining_searches_bounded_nonce_batches_per_tip() {
        let wallet = Wallet::from_seed("automatic-pow-mining-wallet");
        let ledger = Ledger::new_with_genesis_burns(
            BTreeMap::from([(wallet.address().to_string(), 1)]),
            vec![GenesisBurn::new(wallet.address(), 1)],
            10,
        )
        .unwrap();
        let mut node = NodeCore::from_ledger(wallet.clone(), ledger, 0);

        let disabled = node.prepare_automatic_mining(1);
        assert!(disabled.pow_mined.is_none());
        assert_eq!(
            disabled.skipped_reason.as_deref(),
            Some("automatic mining is off")
        );

        node.set_pow_mining_enabled(true);
        let first = node.prepare_automatic_mining(2);
        assert!(node.ledger().pending_blinded_transactions().len() <= 1);
        let first = std::iter::once(first)
            .chain((3..10_000).map(|timestamp| node.prepare_automatic_mining(timestamp)))
            .find(|plan| plan.pow_mined.is_some())
            .expect("bounded PoW search should eventually find a proof");
        let first_mine = first.pow_mined.as_ref().expect("PoW should be queued");
        let Transaction::Mine {
            anchor,
            recipient,
            difficulty_bits,
            ..
        } = first_mine
        else {
            panic!("expected mine transaction");
        };
        assert_eq!(anchor, &node.chain().last().unwrap().hash);
        assert_eq!(recipient, wallet.address());
        assert_eq!(
            *difficulty_bits,
            node.ledger().current_mine_difficulty_bits()
        );
        let first_pending = node.ledger().pending().len();
        assert!(first_pending >= 1);
        assert!(node.ledger().pending_blinded_transactions().is_empty());
        assert!(node
            .drain_outbox()
            .iter()
            .any(|envelope| matches!(envelope, GossipEnvelope::MineAction(tx) if tx.signature() == first_mine.signature())));
        assert!(
            node.status()
                .mining
                .last_auto_pow_mine_status
                .as_deref()
                .unwrap_or_default()
                .contains("queued")
        );

        let second = (10_000..20_000)
            .map(|timestamp| node.prepare_automatic_mining(timestamp))
            .find(|plan| plan.pow_mined.is_some())
            .expect("automatic PoW should keep searching the same tip after one proof");
        assert_ne!(
            second.pow_mined.as_ref().unwrap().signature(),
            first_mine.signature()
        );
        assert!(node.ledger().pending().len() > first_pending);
        assert!(node.ledger().pending_blinded_transactions().is_empty());
    }

    #[test]
    fn automatic_pow_mining_can_tick_without_finalization() {
        let wallet = Wallet::from_seed("automatic-pow-independent-wallet");
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), 1);
        let mut node = NodeCore::new(NodeConfig {
            wallet,
            genesis_allocations: allocations,
            vdf_rounds: 10,
            burn_per_block: 0,
            burn_fee: 0,
            pow_mining_workers: 1,
            recovery_vdf_top_rank_percent: 100,
        });

        node.set_pow_mining_enabled(true);
        node.prepare_automatic_pow_mining().unwrap();
        let first_searched = node
            .auto_pow_mine_cursor
            .as_ref()
            .expect("PoW cursor should be initialized")
            .searched;

        node.prepare_automatic_pow_mining().unwrap();
        let second_searched = node
            .auto_pow_mine_cursor
            .as_ref()
            .expect("PoW cursor should keep tracking the current tip")
            .searched;

        assert!(second_searched > first_searched);
    }

    #[test]
    fn disabling_automatic_pow_mining_clears_local_work() {
        let wallet = Wallet::from_seed("automatic-pow-disable-wallet");
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), 1);
        let mut node = NodeCore::new(NodeConfig {
            wallet,
            genesis_allocations: allocations,
            vdf_rounds: 10,
            burn_per_block: 0,
            burn_fee: 0,
            pow_mining_workers: 1,
            recovery_vdf_top_rank_percent: 100,
        });

        node.set_pow_mining_enabled(true);
        assert!(node.pow_mining_enabled());
        node.prepare_automatic_pow_mining().unwrap();
        assert!(node.auto_pow_mine_cursor.is_some());
        assert!(node.status().mining.last_auto_pow_mine_status.is_some());

        node.set_pow_mining_enabled(false);

        assert!(!node.pow_mining_enabled());
        assert!(node.auto_pow_mine_cursor.is_none());
        assert!(node.status().mining.last_auto_pow_mine_status.is_none());
    }

    #[test]
    fn automatic_pow_mining_workers_are_clamped_and_reported() {
        let wallet = Wallet::from_seed("automatic-pow-workers-wallet");
        let mut node = NodeCore::new(NodeConfig {
            wallet,
            genesis_allocations: BTreeMap::new(),
            vdf_rounds: 10,
            burn_per_block: 0,
            burn_fee: 0,
            pow_mining_workers: 99,
            recovery_vdf_top_rank_percent: 100,
        });

        assert_eq!(node.pow_mining_workers(), MAX_POW_MINING_WORKERS);
        assert_eq!(
            node.status().mining.max_pow_mining_workers,
            MAX_POW_MINING_WORKERS
        );

        node.set_pow_mining_workers(0);

        assert_eq!(node.pow_mining_workers(), DEFAULT_POW_MINING_WORKERS);
        assert_eq!(
            node.status().mining.pow_mining_workers,
            DEFAULT_POW_MINING_WORKERS
        );
    }

    #[test]
    fn automatic_pow_mining_skips_unspendable_owned_blinded_payloads() {
        let alice = Wallet::from_seed("automatic-pow-stale-owned-blind-alice");
        let bob = Wallet::from_seed("automatic-pow-stale-owned-blind-bob");
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(bob.address().to_string(), MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(bob.address(), MICRO_IUNA)],
            10,
        )
        .unwrap();
        let mut node = NodeCore::from_ledger(alice.clone(), ledger, 0);

        let blinded = node
            .blinded_burn_with_fee(MICRO_IUNA / 10, 7, node.chain_height() + 4)
            .unwrap();
        let mut finalizer_ledger = node.ledger().clone();
        let leader_burn = finalizer_ledger.build_burn(&bob, 1, 0).unwrap();
        finalizer_ledger.submit_transaction(leader_burn).unwrap();
        let commit_block = finalizer_ledger.mine_next_block(&bob, 1).unwrap();
        assert!(
            commit_block
                .blinded_transactions
                .iter()
                .any(|tx| tx.commitment == blinded.commitment)
        );
        finalizer_ledger.apply_block(commit_block).unwrap();
        assert!(node.import_verified_ledger(finalizer_ledger).unwrap());
        assert!(
            node.ledger()
                .has_unrevealed_blinded_transaction(&blinded.commitment)
        );

        node.set_pow_mining_enabled(true);

        assert!(node.prepare_automatic_pow_mining().is_ok());
    }

    #[test]
    fn automatic_pow_mining_skips_stale_local_anchor_reservation() {
        let wallet = Wallet::from_seed("automatic-pow-stale-anchor-wallet");
        let mut stale_allocations = BTreeMap::new();
        stale_allocations.insert(wallet.address().to_string(), MICRO_IUNA);
        let stale_ledger = Ledger::new(stale_allocations, 10);
        let stale_anchor = stale_ledger.build_burn(&wallet, 1, 0).unwrap();
        let live_ledger = Ledger::new(BTreeMap::new(), 10);
        let mut node = NodeCore::from_ledger(wallet.clone(), live_ledger, 0);
        node.local_block_anchor_burn = Some((node.chain_height(), stale_anchor));
        node.set_pow_mining_enabled(true);

        assert!(node.prepare_automatic_pow_mining().is_ok());
    }

    #[test]
    fn automatic_finalization_does_not_tick_pow_mining() {
        let wallet = Wallet::from_seed("automatic-pow-separated-finalizer-wallet");
        let mut node = NodeCore::new(NodeConfig {
            wallet,
            genesis_allocations: BTreeMap::new(),
            vdf_rounds: 10,
            burn_per_block: 0,
            burn_fee: 0,
            pow_mining_workers: 1,
            recovery_vdf_top_rank_percent: 100,
        });

        node.set_pow_mining_enabled(true);
        let _ = node.prepare_automatic_finalization(1);

        assert!(node.auto_pow_mine_cursor.is_none());
    }

    #[test]
    fn automatic_finalization_prepares_recovery_after_ticket_timeout() {
        let alice = Wallet::from_seed("automatic-recovery-alice");
        let bob = Wallet::from_seed("automatic-recovery-bob");
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(alice.address(), 1)],
            10,
        )
        .unwrap();
        let mut node = NodeCore::from_ledger(bob, ledger, 1);

        let early = node.prepare_automatic_finalization(RECOVERY_BLOCK_DELAY_MS - 1);
        assert!(early.work.is_none());
        assert!(
            early
                .skipped_reason
                .as_deref()
                .unwrap_or_default()
                .contains("waiting for selected finalizer")
        );

        let recovery = node.prepare_automatic_finalization(RECOVERY_BLOCK_DELAY_MS);
        let work = recovery.work.expect("recovery work should be prepared");
        let block = work.finish(
            node.wallet.unlocked().unwrap(),
            "preverified-vdf".to_string(),
        );

        assert_eq!(block.finalizer_mode, FinalizerMode::Recovery);
        assert!(block.leader_proof.is_none());
    }

    #[test]
    fn automatic_finalization_respects_zero_recovery_vdf_threshold() {
        let alice = Wallet::from_seed("automatic-recovery-zero-alice");
        let bob = Wallet::from_seed("automatic-recovery-zero-bob");
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(alice.address(), 1)],
            10,
        )
        .unwrap();
        let mut node = NodeCore::from_ledger(bob, ledger, 1);
        node.set_recovery_vdf_top_rank_percent(0);

        let recovery = node.prepare_automatic_finalization(RECOVERY_BLOCK_DELAY_MS);

        assert!(recovery.work.is_none());
    }

    #[test]
    fn automatic_pow_mining_uses_protocol_finalizer_fee() {
        let wallet = Wallet::from_seed("automatic-pow-mining-fee-wallet");
        let mut node = NodeCore::new(NodeConfig {
            wallet,
            genesis_allocations: BTreeMap::new(),
            vdf_rounds: 10,
            burn_per_block: 0,
            burn_fee: 0,
            pow_mining_workers: 1,
            recovery_vdf_top_rank_percent: 100,
        });

        node.set_pow_mining_enabled(true);
        let plan = (1..10_000)
            .map(|timestamp| node.prepare_automatic_mining(timestamp))
            .find(|plan| plan.pow_mined.is_some())
            .expect("bounded PoW search should eventually find a proof");
        let mine = plan.pow_mined.expect("PoW should be queued");

        assert_eq!(mine.fee(), MINE_FINALIZER_FEE);
        assert_eq!(mine.amount(), crate::domain::MINE_REWARD);
        assert_eq!(
            node.status().mining.automatic_pow_mine_fee,
            MINE_FINALIZER_FEE
        );
    }

    #[test]
    fn status_reports_package_version() {
        let wallet = Wallet::from_seed("status-version-wallet");
        let node = NodeCore::new(NodeConfig {
            wallet,
            genesis_allocations: BTreeMap::new(),
            vdf_rounds: 1,
            burn_per_block: 0,
            burn_fee: 0,
            pow_mining_workers: 1,
            recovery_vdf_top_rank_percent: 100,
        });

        assert_eq!(node.status().app_version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn automatic_pow_status_reports_setup_placeholder_wait() {
        let wallet = Wallet::from_seed("automatic-pow-setup-placeholder-wallet");
        let ledger = Ledger::new(BTreeMap::new(), 1);
        let mut node = NodeCore::from_ledger(wallet, ledger, 0);

        node.set_pow_mining_enabled(true);

        assert_eq!(
            node.status().mining.last_auto_pow_mine_status.as_deref(),
            Some("waiting for a real chain before PoW mining can start")
        );
    }

    #[test]
    fn fee_rate_transfer_and_burn_pay_at_least_bytes_times_rate() {
        let transfer_sender = Wallet::from_seed("fee-rate-transfer-sender");
        let transfer_recipient = Wallet::from_seed("fee-rate-transfer-recipient");
        let mut transfer_genesis = BTreeMap::new();
        transfer_genesis.insert(transfer_sender.address().to_string(), 10 * MICRO_IUNA);
        let transfer_ledger = crate::domain::Ledger::new_with_genesis_burns(
            transfer_genesis,
            vec![GenesisBurn::new(transfer_sender.address(), MICRO_IUNA)],
            1,
        )
        .unwrap();
        let mut transfer_node = NodeCore::from_ledger(transfer_sender, transfer_ledger, 0);

        let (transfer, transfer_estimate) = transfer_node
            .transfer_with_fee_rate(transfer_recipient.address(), MICRO_IUNA, 2, &[])
            .unwrap();
        let transfer_blinded_bytes =
            transfer_node.ledger().pending_blinded_transactions()[0].fee_rate_size_bytes();
        assert_eq!(transfer_estimate.bytes, transfer_blinded_bytes);
        let minimum_transfer_fee = transfer_blinded_bytes as u64 * 2;
        assert!(transfer.fee() >= minimum_transfer_fee);
        assert!(transfer_node.ledger().pending().is_empty());
        assert_eq!(
            transfer_node.ledger().pending_blinded_transactions().len(),
            1
        );

        let burn_wallet = Wallet::from_seed("fee-rate-burn-wallet");
        let mut burn_genesis = BTreeMap::new();
        burn_genesis.insert(burn_wallet.address().to_string(), 10 * MICRO_IUNA);
        let burn_ledger = crate::domain::Ledger::new_with_genesis_burns(
            burn_genesis,
            vec![GenesisBurn::new(burn_wallet.address(), MICRO_IUNA)],
            1,
        )
        .unwrap();
        let mut burn_node = NodeCore::from_ledger(burn_wallet, burn_ledger, 0);
        let (burn, burn_estimate) = burn_node.burn_with_fee_rate(MICRO_IUNA, 3).unwrap();
        let burn_blinded_bytes =
            burn_node.ledger().pending_blinded_transactions()[0].fee_rate_size_bytes();
        assert_eq!(burn_estimate.bytes, burn_blinded_bytes);
        let minimum_burn_fee = burn_blinded_bytes as u64 * 3;
        assert!(burn.fee() >= minimum_burn_fee);
        assert!(burn_node.ledger().pending().is_empty());
        assert_eq!(burn_node.ledger().pending_blinded_transactions().len(), 1);
    }

    #[test]
    fn mempool_gossip_includes_blinded_transactions() {
        let alice = Wallet::from_seed("blinded-gossip-alice");
        let mut genesis = BTreeMap::new();
        genesis.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new(genesis, 1);
        let blinded = ledger.build_blinded_burn(&alice, MICRO_IUNA, 7, 3).unwrap();
        let mut sender = NodeCore::from_ledger(alice.clone(), ledger.clone(), 0);
        let mut receiver = NodeCore::from_ledger(alice, ledger, 0);

        sender
            .receive_blinded_transaction(blinded.transaction.clone())
            .unwrap();
        for envelope in sender.mempool_gossip() {
            receiver.receive(envelope).unwrap();
        }

        assert_eq!(
            receiver.ledger().pending_blinded_transactions(),
            std::slice::from_ref(&blinded.transaction)
        );
    }

    #[test]
    fn mempool_gossip_includes_public_mine_actions() {
        let alice = Wallet::from_seed("mine-gossip-alice");
        let ledger = Ledger::new(BTreeMap::new(), 1);
        let mine = ledger.build_mine(alice.address()).unwrap();
        let mut sender = NodeCore::from_ledger(alice.clone(), ledger.clone(), 0);
        let mut receiver = NodeCore::from_ledger(alice, ledger, 0);

        sender.submit_public_mine_action(mine.clone()).unwrap();
        for envelope in sender.mempool_gossip() {
            receiver.receive(envelope).unwrap();
        }

        assert_eq!(receiver.ledger().pending(), std::slice::from_ref(&mine));
        assert!(receiver.ledger().pending_blinded_transactions().is_empty());
    }

    #[test]
    fn receiving_blinded_reveal_batch_publishes_complete_committee_bundle() {
        let alice = Wallet::from_seed("immediate-bundle-alice");
        let bob = Wallet::from_seed("immediate-bundle-bob");
        let carol = Wallet::from_seed("immediate-bundle-carol");
        let dave = Wallet::from_seed("immediate-bundle-dave");
        let finalizers = [alice.clone(), bob.clone()];
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(carol.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(dave.address().to_string(), 10 * MICRO_IUNA);
        let mut ledger = Ledger::new_with_genesis_burns(
            allocations,
            finalizers
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
                .collect(),
            1,
        )
        .unwrap();
        let first = ledger
            .build_blinded_burn(&carol, 3, 100, ledger.height() + 4)
            .unwrap();
        let second = ledger
            .build_blinded_burn(&dave, 4, 100, ledger.height() + 4)
            .unwrap();
        ledger
            .submit_blinded_transaction(first.transaction.clone())
            .unwrap();
        ledger
            .submit_blinded_transaction(second.transaction.clone())
            .unwrap();
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let leader_wallet = wallet_for_address(&finalizers, &leader);
        let burn = ledger.build_burn(leader_wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let commit_block = ledger.mine_next_block(leader_wallet, 1).unwrap();
        ledger.apply_locally_mined_block(commit_block).unwrap();

        let committee = ledger.reveal_committee_for_next_block();
        let committee_wallet = committee
            .iter()
            .filter_map(|member| {
                finalizers
                    .iter()
                    .find(|wallet| wallet.address() == member.owner)
            })
            .next()
            .expect("test finalizer should be in reveal committee");
        let mut committee_node = NodeCore::from_ledger(committee_wallet.clone(), ledger, 0);

        committee_node
            .receive(GossipEnvelope::BlindedReveals {
                reveals: vec![first.reveal.clone(), second.reveal.clone()],
            })
            .unwrap();
        let outbox = committee_node.drain_outbox();

        assert!(outbox.iter().any(|envelope| matches!(
            envelope,
            GossipEnvelope::BlindedReveal(reveal) if reveal.commitment == first.reveal.commitment
        )));
        assert!(outbox.iter().any(|envelope| matches!(
            envelope,
            GossipEnvelope::BlindedReveal(reveal) if reveal.commitment == second.reveal.commitment
        )));
        assert!(outbox.iter().any(|envelope| matches!(
            envelope,
            GossipEnvelope::RevealBundle(bundle)
                if bundle.member == committee_wallet.address()
                    && bundle.reveals.len() == 2
                    && bundle.reveals.iter().any(|reveal| reveal.commitment == first.reveal.commitment)
                    && bundle.reveals.iter().any(|reveal| reveal.commitment == second.reveal.commitment)
        )));
    }

    #[test]
    fn automatic_finalization_includes_reveals_with_two_nodes_and_one_burner() {
        let finalizer = Wallet::from_seed("single-burner-reveal-finalizer");
        let wallet = Wallet::from_seed("single-burner-reveal-wallet");
        let mut allocations = BTreeMap::new();
        allocations.insert(finalizer.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(wallet.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(finalizer.address(), MICRO_IUNA)],
            1,
        )
        .unwrap();
        let mut network = InMemoryNetwork::default();
        network.insert(
            "finalizer",
            NodeCore::from_ledger_with_burn_fee_and_enabled(
                finalizer.clone(),
                ledger.clone(),
                true,
                MICRO_IUNA / 10,
                1,
            ),
        );
        network.insert("wallet", NodeCore::from_ledger(wallet.clone(), ledger, 0));

        let blinded = network
            .node_mut("wallet")
            .unwrap()
            .blinded_burn_with_fee(MICRO_IUNA / 10, 1, 4)
            .unwrap();
        network.deliver_until_idle().unwrap();

        let commit_plan = network
            .node_mut("finalizer")
            .unwrap()
            .prepare_automatic_finalization(1);
        let commit_work = commit_plan
            .work
            .expect("finalizer should prepare commit block");
        let commit_vdf = run_vdf(commit_work.vdf_seed(), commit_work.vdf_rounds());
        let commit_block = network
            .node_mut("finalizer")
            .unwrap()
            .complete_prepared_block_at(commit_work, commit_vdf, 1)
            .unwrap();
        let wallet_commitment = blinded.commitment.clone();
        assert!(
            commit_block
                .blinded_transactions
                .iter()
                .any(|transaction| transaction.commitment == wallet_commitment),
            "first block should commit the wallet's blinded burn"
        );
        network.deliver_until_idle().unwrap();
        assert!(
            network
                .node("finalizer")
                .unwrap()
                .ledger()
                .pending_blinded_reveals()
                .iter()
                .any(|reveal| reveal.commitment == wallet_commitment),
            "finalizer should have received the reveal before building the next block"
        );
        assert!(
            network
                .node("wallet")
                .unwrap()
                .ledger()
                .pending_blinded_reveals()
                .iter()
                .any(|reveal| reveal.commitment == wallet_commitment),
            "wallet node should also keep the reveal in its mempool"
        );

        let reveal_plan = network
            .node_mut("finalizer")
            .unwrap()
            .prepare_automatic_finalization(2);
        let reveal_work = reveal_plan
            .work
            .expect("finalizer should prepare reveal block");
        let reveal_vdf = run_vdf(reveal_work.vdf_seed(), reveal_work.vdf_rounds());
        let reveal_block = network
            .node_mut("finalizer")
            .unwrap()
            .complete_prepared_block_at(reveal_work, reveal_vdf, 2)
            .unwrap();

        assert!(
            reveal_block
                .all_blinded_reveals()
                .iter()
                .any(|reveal| reveal.commitment == wallet_commitment),
            "automatic finalization should include the pending reveal without requiring an extra mempool poll"
        );
    }

    #[test]
    fn genesis_transfer_arriving_during_vdf_with_peer_mines_does_not_stall_following_blocks() {
        let miner = Wallet::from_seed("during-vdf-miner");
        let (_finalizer, finalizer_node, block2_work) = (0..1_000)
            .find_map(|seed_index| {
                let finalizer = Wallet::from_seed(&format!("during-vdf-finalizer-{seed_index}"));
                let mut allocations = BTreeMap::new();
                allocations.insert(finalizer.address().to_string(), 10 * MICRO_IUNA);
                let mut ledger = Ledger::new_with_genesis_burns(
                    allocations,
                    vec![GenesisBurn::new(finalizer.address(), MICRO_IUNA)],
                    1,
                )
                .unwrap();
                let split = ledger
                    .build_transfer(&finalizer, finalizer.address(), MICRO_IUNA / 10, 0)
                    .ok()?;
                let split_change = OutPoint {
                    txid: split.signature().to_string(),
                    index: 1,
                };
                ledger.submit_transaction(split).ok()?;
                let burn = ledger
                    .build_burn_with_inputs(&finalizer, 1, 0, &[split_change])
                    .ok()?;
                ledger.submit_transaction(burn).ok()?;
                let block1 = ledger.mine_next_block(&finalizer, 1).ok()?;
                assert!(block1.blinded_transactions.is_empty());
                assert!(
                    !block1
                        .transactions
                        .iter()
                        .any(|transaction| matches!(transaction, Transaction::Mine { .. })),
                    "node B joins after block 1, so block 1 should not include B's mine action"
                );
                ledger.apply_block(block1).ok()?;
                let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
                    finalizer.clone(),
                    ledger,
                    true,
                    0,
                    100,
                );
                let block2_plan = node.prepare_automatic_finalization(2);
                let block2_work = block2_plan.work?;
                let (_, anchor_burn) = node.local_block_anchor_burn.clone()?;
                let anchor_inputs = super::transaction_input_outpoints(&anchor_burn);
                let anchor_total = node
                    .ledger()
                    .utxos_for_address(finalizer.address())
                    .iter()
                    .filter(|(outpoint, _)| anchor_inputs.contains(outpoint))
                    .map(|(_, output)| output.amount)
                    .sum::<u64>();
                if anchor_total >= 200_000 {
                    return None;
                }
                Some((finalizer, node, block2_work))
            })
            .expect("test should find a seed where block 2 anchor uses the small reward UTXO");
        let mut network = InMemoryNetwork::default();
        network.insert("finalizer", finalizer_node);

        let miner_ledger =
            Ledger::from_snapshot(network.node("finalizer").unwrap().chain_snapshot()).unwrap();
        network.insert(
            "miner",
            NodeCore::from_ledger(miner.clone(), miner_ledger, 0),
        );

        queue_auto_pow_mine_action(network.node_mut("miner").unwrap());
        network.deliver_until_idle().unwrap();
        network
            .node_mut("finalizer")
            .unwrap()
            .transfer_with_fee_rate(miner.address(), MICRO_IUNA / 10, 100, &[])
            .unwrap();
        let blinded = network
            .node("finalizer")
            .unwrap()
            .ledger()
            .pending_blinded_transactions()
            .last()
            .cloned()
            .expect("A should queue the A -> B blinded transfer while block 2 VDF is running");
        network.deliver_until_idle().unwrap();
        assert!(
            network
                .node("finalizer")
                .unwrap()
                .ledger()
                .pending_blinded_transactions()
                .iter()
                .any(|tx| tx.commitment == blinded.commitment),
            "the finalizer should receive the blinded tx while block 2 VDF is running"
        );

        let block2_vdf = run_vdf(block2_work.vdf_seed(), block2_work.vdf_rounds());
        let block2 = network
            .node_mut("finalizer")
            .unwrap()
            .complete_prepared_block_at(block2_work, block2_vdf, 2)
            .unwrap();
        assert!(
            block2.blinded_transactions.is_empty(),
            "block 2 work was prepared before the blinded tx arrived"
        );
        assert!(
            !block2
                .transactions
                .iter()
                .any(|transaction| matches!(transaction, Transaction::Mine { .. })),
            "block 2 work was prepared before B's mine action arrived"
        );
        network.deliver_until_idle().unwrap();

        let block3_outcome = network
            .node_mut("finalizer")
            .unwrap()
            .automatic_mine_once(3);
        assert!(
            block3_outcome.block.is_some(),
            "finalizer should keep producing after the during-VDF blinded tx: {:?}",
            block3_outcome.skipped_reason
        );
        let block3 = block3_outcome.block.unwrap();
        assert!(
            block3
                .transactions
                .first()
                .is_some_and(Transaction::is_burn),
            "the mandatory anchor burn must be selected before during-VDF mempool items"
        );
        let committed_blinded = block3
            .blinded_transactions
            .iter()
            .any(|tx| tx.commitment == blinded.commitment);
        assert_block_has_mine_action(&block3);
        network.deliver_until_idle().unwrap();

        queue_auto_pow_mine_action(network.node_mut("miner").unwrap());
        network.deliver_until_idle().unwrap();
        let block4 = network
            .node_mut("finalizer")
            .unwrap()
            .automatic_mine_once(4)
            .block
            .expect("finalizer should keep producing the next block");
        if committed_blinded {
            assert!(
                block4
                    .all_blinded_reveals()
                    .iter()
                    .any(|reveal| reveal.commitment == blinded.commitment),
                "the committed during-VDF blinded tx should reveal in a later block"
            );
        } else {
            assert!(
                network
                    .node("finalizer")
                    .unwrap()
                    .ledger()
                    .pending_blinded_transactions()
                    .is_empty(),
                "a conflicting during-VDF blinded tx should be pruned after the anchor burn spends its input"
            );
            assert!(
                network
                    .node("finalizer")
                    .unwrap()
                    .owned_blinded_transactions()
                    .is_empty(),
                "owned blinded state should not keep rebroadcasting a pruned tx"
            );
            assert!(
                block4.all_blinded_reveals().is_empty(),
                "a pruned blinded tx was never committed, so there should be no reveal"
            );
        }
        assert_block_has_mine_action(&block4);
    }

    #[test]
    fn own_blinded_transaction_arriving_during_vdf_does_not_starve_next_anchor_burn() {
        let recipient = Wallet::from_seed("during-vdf-own-recipient");
        let (mut node, block2_work, transfer_outpoint, transfer_amount) = (0..1_000)
            .find_map(|seed_index| {
                let finalizer =
                    Wallet::from_seed(&format!("during-vdf-own-finalizer-{seed_index}"));
                let mut allocations = BTreeMap::new();
                allocations.insert(finalizer.address().to_string(), 10 * MICRO_IUNA);
                let ledger = Ledger::new_with_genesis_burns(
                    allocations,
                    vec![GenesisBurn::new(finalizer.address(), MICRO_IUNA)],
                    1,
                )
                .unwrap();
                let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
                    finalizer.clone(),
                    ledger,
                    true,
                    0,
                    1_000,
                );
                node.set_pow_mining_enabled(true);
                let block1 = node.automatic_mine_once(1).block?;
                assert!(block1.blinded_transactions.is_empty());

                let block2_plan = node.prepare_automatic_finalization(2);
                let block2_work = block2_plan.work?;
                let (_, anchor_burn) = node.local_block_anchor_burn.clone()?;
                let anchor_inputs = super::transaction_input_outpoints(&anchor_burn);
                let utxos = node.ledger().utxos_for_address(finalizer.address());
                let anchor_total = utxos
                    .iter()
                    .filter(|(outpoint, _)| anchor_inputs.contains(outpoint))
                    .map(|(_, output)| output.amount)
                    .sum::<u64>();
                let (transfer_outpoint, transfer_output) =
                    utxos.into_iter().find(|(outpoint, output)| {
                        !anchor_inputs.contains(outpoint) && output.amount > MICRO_IUNA
                    })?;
                if anchor_total >= 2_000_000 {
                    return None;
                }
                Some((
                    node,
                    block2_work,
                    transfer_outpoint,
                    transfer_output.amount.min(MICRO_IUNA) / 10,
                ))
            })
            .expect("test should find a seed with live-like small-anchor/large-change UTXOs");

        node.transfer_with_fee_spending(
            recipient.address(),
            transfer_amount,
            1,
            &[transfer_outpoint],
        )
        .expect("wallet tx created while block 2 VDF is running");
        let blinded = node
            .ledger()
            .pending_blinded_transactions()
            .last()
            .cloned()
            .expect("wallet tx should be queued as a blinded transaction");

        let block2_vdf = run_vdf(block2_work.vdf_seed(), block2_work.vdf_rounds());
        let block2 = node
            .complete_prepared_block_at(block2_work, block2_vdf, 2)
            .unwrap();
        assert!(
            block2.blinded_transactions.is_empty(),
            "block 2 work was prepared before the blinded tx arrived"
        );
        assert!(
            node.ledger()
                .pending_blinded_transactions()
                .iter()
                .any(|tx| tx.commitment == blinded.commitment),
            "the during-VDF blinded tx should remain pending for block 3"
        );

        let block3_outcome = node.automatic_mine_once(3);
        assert!(
            block3_outcome.block.is_some(),
            "pending own blinded tx must not starve the next anchor burn: {:?}",
            block3_outcome.skipped_reason
        );
        let block3 = block3_outcome.block.unwrap();
        assert!(
            block3.blinded_transactions.is_empty()
                || block3
                    .blinded_transactions
                    .iter()
                    .any(|tx| tx.commitment == blinded.commitment),
            "block 3 may include the during-VDF tx, but must not stall when the anchor burn has priority"
        );
    }

    #[test]
    fn owned_blinded_transaction_reveals_after_commit_block_import() {
        let alice = Wallet::from_seed("owned-blinded-reveal-alice");
        let bob = Wallet::from_seed("owned-blinded-reveal-bob");
        let carol = Wallet::from_seed("owned-blinded-reveal-carol");
        let finalizers = [alice.clone(), bob.clone()];
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(carol.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            finalizers
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
                .collect(),
            1,
        )
        .unwrap();
        let mut wallet_node = NodeCore::from_ledger(carol.clone(), ledger.clone(), 0);
        let mut finalizer_ledger = ledger;

        let blinded = wallet_node
            .blinded_burn_with_fee(3, 7, wallet_node.chain_height() + 4)
            .unwrap();
        wallet_node.drain_outbox();
        finalizer_ledger
            .submit_blinded_transaction(blinded.clone())
            .unwrap();
        let leader = finalizer_ledger.expected_leader_for_next_block().unwrap();
        let finalizer = finalizers
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap();
        let burn = finalizer_ledger.build_burn(finalizer, 1, 0).unwrap();
        finalizer_ledger.submit_transaction(burn).unwrap();
        let commit_block = finalizer_ledger.mine_next_block(finalizer, 1).unwrap();

        wallet_node
            .receive(GossipEnvelope::Block(commit_block))
            .unwrap();
        let outbox = wallet_node.drain_outbox();

        assert!(
            wallet_node
                .ledger()
                .pending_blinded_reveals()
                .iter()
                .any(|reveal| reveal.commitment == blinded.commitment)
        );
        assert!(outbox.iter().any(|envelope| matches!(
            envelope,
            GossipEnvelope::BlindedReveal(reveal) if reveal.commitment == blinded.commitment
        )));
    }

    #[test]
    fn status_wallet_balance_includes_owned_blinded_change_before_and_after_commit() {
        let alice = Wallet::from_seed("owned-blinded-balance-alice");
        let bob = Wallet::from_seed("owned-blinded-balance-bob");
        let carol = Wallet::from_seed("owned-blinded-balance-carol");
        let finalizers = [alice.clone(), bob.clone()];
        let starting_balance = 2 * MICRO_IUNA;
        let burn_amount = MICRO_IUNA / 10;
        let fee = MICRO_IUNA / 10;
        let expected_balance = starting_balance - burn_amount - fee;
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(carol.address().to_string(), starting_balance);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            finalizers
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
                .collect(),
            1,
        )
        .unwrap();
        let mut wallet_node = NodeCore::from_ledger(carol.clone(), ledger.clone(), 0);
        let mut finalizer_ledger = ledger;

        let blinded = wallet_node
            .blinded_burn_with_fee(burn_amount, fee, wallet_node.chain_height() + 4)
            .unwrap();

        assert_eq!(wallet_node.status().wallet_balance, expected_balance);

        finalizer_ledger
            .submit_blinded_transaction(blinded.clone())
            .unwrap();
        let leader = finalizer_ledger.expected_leader_for_next_block().unwrap();
        let finalizer = finalizers
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap();
        let burn = finalizer_ledger.build_burn(finalizer, 1, 0).unwrap();
        finalizer_ledger.submit_transaction(burn).unwrap();
        let commit_block = finalizer_ledger.mine_next_block(finalizer, 1).unwrap();

        wallet_node
            .receive(GossipEnvelope::Block(commit_block))
            .unwrap();

        assert_eq!(wallet_node.ledger().balance_of(carol.address()), 0);
        assert_eq!(wallet_node.status().wallet_balance, expected_balance);
    }

    #[test]
    fn owned_blinded_transaction_restore_requeues_pending_commit() {
        let alice = Wallet::from_seed("owned-blinded-restore-pending-alice");
        let bob = Wallet::from_seed("owned-blinded-restore-pending-bob");
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new(allocations, 1);
        let mut node = NodeCore::from_ledger(alice.clone(), ledger.clone(), 0);

        let blinded = node
            .blinded_transfer_with_fee(bob.address(), MICRO_IUNA, 7, node.chain_height() + 4)
            .unwrap();
        let owned = node.owned_blinded_transactions();
        let mut restarted = NodeCore::from_ledger(alice, ledger, 0);

        restarted.restore_owned_blinded_transactions(owned).unwrap();

        assert_eq!(
            restarted.ledger().pending_blinded_transactions(),
            std::slice::from_ref(&blinded)
        );
        let outbox = restarted.drain_outbox();
        assert!(outbox.iter().any(|envelope| matches!(
            envelope,
            GossipEnvelope::BlindedTransaction(transaction)
                if transaction.commitment == blinded.commitment
        )));
        assert!(!outbox.iter().any(|envelope| matches!(
            envelope,
            GossipEnvelope::BlindedReveal(reveal) if reveal.commitment == blinded.commitment
        )));
    }

    #[test]
    fn owned_blinded_transaction_restore_skips_stale_commit_with_spent_input() {
        let alice = Wallet::from_seed("owned-blinded-restore-stale-alice");
        let bob = Wallet::from_seed("owned-blinded-restore-stale-bob");
        let finalizers = [bob.clone()];
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), MICRO_IUNA);
        allocations.insert(bob.address().to_string(), MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            finalizers
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
                .collect(),
            1,
        )
        .unwrap();
        let mut wallet_node = NodeCore::from_ledger(alice.clone(), ledger.clone(), 0);

        wallet_node
            .blinded_burn_with_fee(MICRO_IUNA / 10, 7, wallet_node.chain_height() + 4)
            .unwrap();
        let owned = wallet_node.owned_blinded_transactions();
        let stale_conflict = ledger.build_burn(&alice, MICRO_IUNA / 10, 7).unwrap();
        let mut advanced_ledger = ledger;
        advanced_ledger.submit_transaction(stale_conflict).unwrap();
        let leader = advanced_ledger.expected_leader_for_next_block().unwrap();
        assert_eq!(leader, bob.address());
        let anchor_burn = advanced_ledger.build_burn(&bob, 1, 0).unwrap();
        advanced_ledger.submit_transaction(anchor_burn).unwrap();
        let block = advanced_ledger.mine_next_block(&bob, 1).unwrap();
        advanced_ledger.apply_block(block).unwrap();
        let mut restarted = NodeCore::from_ledger(alice, advanced_ledger, 0);

        restarted.restore_owned_blinded_transactions(owned).unwrap();

        assert!(restarted.owned_blinded_transactions().is_empty());
        assert!(restarted.ledger().pending_blinded_transactions().is_empty());
        assert!(restarted.drain_outbox().is_empty());
    }

    #[test]
    fn owned_blinded_transaction_restore_publishes_reveal_after_commit() {
        let alice = Wallet::from_seed("owned-blinded-restore-reveal-alice");
        let bob = Wallet::from_seed("owned-blinded-restore-reveal-bob");
        let carol = Wallet::from_seed("owned-blinded-restore-reveal-carol");
        let finalizers = [alice.clone(), bob.clone()];
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(carol.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            finalizers
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
                .collect(),
            1,
        )
        .unwrap();
        let mut wallet_node = NodeCore::from_ledger(carol.clone(), ledger.clone(), 0);
        let mut finalizer_ledger = ledger;

        let blinded = wallet_node
            .blinded_burn_with_fee(3, 7, wallet_node.chain_height() + 4)
            .unwrap();
        let owned = wallet_node.owned_blinded_transactions();
        finalizer_ledger
            .submit_blinded_transaction(blinded.clone())
            .unwrap();
        let leader = finalizer_ledger.expected_leader_for_next_block().unwrap();
        let finalizer = finalizers
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap();
        let burn = finalizer_ledger.build_burn(finalizer, 1, 0).unwrap();
        finalizer_ledger.submit_transaction(burn).unwrap();
        let commit_block = finalizer_ledger.mine_next_block(finalizer, 1).unwrap();
        finalizer_ledger.apply_block(commit_block.clone()).unwrap();
        let mut restarted_ledger = NodeCore::from_ledger(
            carol,
            Ledger::from_snapshot(finalizer_ledger.snapshot()).unwrap(),
            0,
        );

        restarted_ledger
            .restore_owned_blinded_transactions(owned)
            .unwrap();

        assert!(
            restarted_ledger
                .ledger()
                .pending_blinded_reveals()
                .iter()
                .any(|reveal| reveal.commitment == blinded.commitment)
        );
        assert!(
            restarted_ledger
                .drain_outbox()
                .iter()
                .any(|envelope| matches!(
                    envelope,
                    GossipEnvelope::BlindedReveal(reveal) if reveal.commitment == blinded.commitment
                ))
        );
        assert!(
            commit_block
                .blinded_transactions
                .iter()
                .any(|transaction| transaction.commitment == blinded.commitment)
        );
    }

    #[test]
    fn automatic_non_leader_burn_is_queued_as_blinded() {
        let alice = Wallet::from_seed("auto-blinded-burn-alice");
        let bob = Wallet::from_seed("auto-blinded-burn-bob");
        let carol = Wallet::from_seed("auto-blinded-burn-carol");
        let finalizers = [alice.clone(), bob.clone()];
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(carol.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            finalizers
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
                .collect(),
            1,
        )
        .unwrap();
        assert_eq!(ledger.finalizer_rank_for_next_block(carol.address()), None);
        let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
            carol,
            ledger,
            true,
            MICRO_IUNA / 10,
            1,
        );

        let plan = node.prepare_automatic_finalization(1);
        let outbox = node.drain_outbox();

        assert!(plan.burned.is_some());
        assert!(node.ledger().pending().is_empty());
        assert_eq!(node.ledger().pending_blinded_transactions().len(), 1);
        assert!(
            outbox
                .iter()
                .any(|envelope| matches!(envelope, GossipEnvelope::BlindedTransaction(_)))
        );
    }

    #[test]
    fn automatic_fallback_finalizer_prepares_anchor_and_blinded_burn() {
        let alice = Wallet::from_seed("auto-fallback-burn-alice");
        let bob = Wallet::from_seed("auto-fallback-burn-bob");
        let finalizers = [alice.clone(), bob.clone()];
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            finalizers
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
                .collect(),
            1,
        )
        .unwrap();
        let fallback = finalizers
            .iter()
            .find(|wallet| ledger.finalizer_rank_for_next_block(wallet.address()) == Some(1))
            .unwrap()
            .clone();
        let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
            fallback,
            ledger,
            true,
            MICRO_IUNA / 10,
            1,
        );

        let plan = node.prepare_automatic_finalization(1);
        let outbox = node.drain_outbox();

        assert!(plan.burned.is_some());
        assert!(
            plan.skipped_reason.is_none(),
            "fallback should not be skipped: {:?}",
            plan.skipped_reason
        );
        assert!(node.ledger().pending().is_empty());
        assert_eq!(node.ledger().pending_blinded_transactions().len(), 1);
        let (_, anchor_burn) = node
            .local_block_anchor_burn
            .as_ref()
            .expect("fallback anchor burn should be held locally");
        let anchor_signature = anchor_burn.signature().to_string();
        assert_eq!(anchor_burn.amount(), super::AUTO_BLOCK_ANCHOR_BURN_AMOUNT);
        assert!(
            outbox
                .iter()
                .any(|envelope| matches!(envelope, GossipEnvelope::BlindedTransaction(_)))
        );
        let work = plan.work.expect("fallback work should be prepared");
        let vdf_output = run_vdf(work.vdf_seed(), work.vdf_rounds());
        let block = node
            .complete_prepared_block_at(work, vdf_output, VDF_TARGET_BLOCK_MS * 2)
            .unwrap();

        assert_eq!(block.finalizer_rank, 1);
        assert_eq!(block.finalizer_mode, FinalizerMode::Ticket);
        assert!(block.transactions.iter().any(|transaction| {
            transaction.is_burn() && transaction.amount() == super::AUTO_BLOCK_ANCHOR_BURN_AMOUNT
        }));
        assert_eq!(
            block
                .transactions
                .first()
                .map(|transaction| transaction.signature()),
            Some(anchor_signature.as_str())
        );
        assert!(!block.blinded_transactions.is_empty());
        assert_eq!(node.ledger().height(), 1);
    }

    #[test]
    fn automatic_leader_prepares_anchor_and_blinded_burn() {
        let alice = Wallet::from_seed("auto-plaintext-burn-alice");
        let bob = Wallet::from_seed("auto-plaintext-burn-bob");
        let finalizers = [alice.clone(), bob.clone()];
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
        let mut ledger = Ledger::new_with_genesis_burns(
            allocations,
            finalizers
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
                .collect(),
            1,
        )
        .unwrap();
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let leader_wallet = finalizers
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap()
            .clone();
        for wallet in &finalizers {
            let split = ledger
                .build_transfer(wallet, wallet.address(), MICRO_IUNA, 0)
                .unwrap();
            ledger.submit_transaction(split).unwrap();
        }
        let anchor = ledger.build_burn(&leader_wallet, 1, 0).unwrap();
        ledger.submit_transaction(anchor).unwrap();
        let split_block = ledger.mine_next_block(&leader_wallet, 1).unwrap();
        ledger.apply_block(split_block).unwrap();
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let leader_wallet = finalizers
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap()
            .clone();
        let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
            leader_wallet,
            ledger,
            true,
            MICRO_IUNA / 10,
            1,
        );

        let plan = node.prepare_automatic_finalization(1);
        let outbox = node.drain_outbox();

        assert!(plan.burned.is_some());
        assert!(node.ledger().pending().is_empty());
        assert_eq!(node.ledger().pending_blinded_transactions().len(), 1);
        let (_, anchor_burn) = node
            .local_block_anchor_burn
            .as_ref()
            .expect("leader anchor burn should be held locally");
        assert_eq!(anchor_burn.amount(), super::AUTO_BLOCK_ANCHOR_BURN_AMOUNT);
        assert!(
            outbox
                .iter()
                .any(|envelope| matches!(envelope, GossipEnvelope::BlindedTransaction(_)))
        );
        assert!(node.prepare_automatic_finalization(1).work.is_some());
    }

    #[test]
    fn wallet_building_reserves_local_anchor_burn_inputs() {
        let alice = Wallet::from_seed("local-anchor-reserve-alice");
        let finalizers = [alice.clone()];
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        let mut ledger = Ledger::new_with_genesis_burns(
            allocations,
            finalizers
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
                .collect(),
            1,
        )
        .unwrap();
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let leader_wallet = finalizers
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap()
            .clone();
        let split = ledger
            .build_transfer(&leader_wallet, leader_wallet.address(), MICRO_IUNA, 0)
            .unwrap();
        ledger.submit_transaction(split).unwrap();
        let anchor = ledger.build_burn(&leader_wallet, 1, 0).unwrap();
        ledger.submit_transaction(anchor).unwrap();
        let split_block = ledger.mine_next_block(&leader_wallet, 1).unwrap();
        ledger.apply_block(split_block).unwrap();
        let mut node =
            NodeCore::from_ledger_with_burn_fee_and_enabled(leader_wallet, ledger, true, 0, 1);

        let plan = node.prepare_automatic_finalization(1);
        assert!(plan.burned.is_some());
        assert!(node.status().wallet_balance < node.ledger().balance_of(node.wallet_address()));
        let (_, anchor_burn) = node
            .local_block_anchor_burn
            .clone()
            .expect("leader burn should be held as a local block anchor");
        let Transaction::Burn { inputs, .. } = anchor_burn else {
            panic!("local block anchor must be a burn");
        };
        let anchor_inputs = inputs
            .iter()
            .map(|input| input.outpoint.clone())
            .collect::<Vec<_>>();

        let blinded = node
            .blinded_burn_with_fee(MICRO_IUNA / 20, 1, node.chain_height() + 4)
            .unwrap();

        assert!(
            blinded
                .inputs
                .iter()
                .all(|input| !anchor_inputs.contains(&input.outpoint)),
            "blinded wallet transactions must not spend inputs reserved by the local anchor burn"
        );

        let work = node.prepare_next_block_with_local_anchor(2).unwrap();
        let vdf_output = run_vdf(work.vdf_seed(), work.vdf_rounds());
        let mut peer_ledger = node.clone_ledger();
        let block = node
            .complete_prepared_block_at(work, vdf_output, VDF_TARGET_BLOCK_MS * 2)
            .unwrap();
        assert!(block.transactions.iter().any(Transaction::is_burn));
        assert!(
            block
                .blinded_transactions
                .iter()
                .any(|transaction| transaction.commitment == blinded.commitment)
        );
        peer_ledger.apply_block_at(block, u64::MAX).unwrap();
        assert_eq!(
            node.ledger().status().tip_hash,
            peer_ledger.status().tip_hash
        );
    }

    #[test]
    fn inbound_blinded_transaction_conflicting_with_local_anchor_is_not_queued() {
        let alice = Wallet::from_seed("local-anchor-inbound-alice");
        let bob = Wallet::from_seed("local-anchor-inbound-bob");
        let finalizers = [alice.clone(), bob.clone()];
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            finalizers
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
                .collect(),
            1,
        )
        .unwrap();
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let leader_wallet = finalizers
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap()
            .clone();
        let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
            leader_wallet.clone(),
            ledger,
            true,
            MICRO_IUNA / 10,
            1,
        );

        let plan = node.prepare_automatic_finalization(1);
        assert!(plan.burned.is_some());
        let (_, anchor_burn) = node
            .local_block_anchor_burn
            .clone()
            .expect("leader burn should be held as a local block anchor");
        let anchor_inputs = super::transaction_input_outpoints(&anchor_burn)
            .into_iter()
            .collect::<Vec<_>>();
        let conflicting_payload = node
            .ledger()
            .build_transfer_with_inputs(&leader_wallet, bob.address(), 1, 0, &anchor_inputs)
            .unwrap();
        let conflicting = node
            .ledger()
            .build_blinded_transaction(&leader_wallet, conflicting_payload, node.chain_height() + 4)
            .unwrap();

        node.receive_blinded_transaction(conflicting.transaction)
            .unwrap();

        assert!(node.ledger().pending_blinded_transactions().is_empty());
        assert!(node.drain_outbox().is_empty());
        assert!(node.prepare_automatic_finalization(1).work.is_some());
    }

    #[test]
    fn locally_produced_blocks_import_on_independent_peer_ledger() {
        let alice = Wallet::from_seed("producer-parity-alice");
        let bob = Wallet::from_seed("producer-parity-bob");
        let carol = Wallet::from_seed("producer-parity-carol");
        let wallets = [alice.clone(), bob.clone(), carol.clone()];
        let mut allocations = BTreeMap::new();
        for wallet in &wallets {
            allocations.insert(wallet.address().to_string(), 20 * MICRO_IUNA);
        }
        let genesis_burns = wallets
            .iter()
            .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
            .collect();
        let mut producer_ledger =
            Ledger::new_with_genesis_burns(allocations, genesis_burns, 1).unwrap();
        let mut peer_ledger = producer_ledger.clone();

        for step in 0..8 {
            assert_eq!(
                producer_ledger.status().tip_hash,
                peer_ledger.status().tip_hash
            );
            let leader = producer_ledger
                .expected_leader_for_next_block()
                .expect("test chain should have an eligible leader");
            let leader_wallet = wallet_for_address(&wallets, &leader).clone();
            let timestamp_ms = (step + 1) as u64 * VDF_TARGET_BLOCK_MS;
            let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
                leader_wallet.clone(),
                producer_ledger.clone(),
                true,
                MICRO_IUNA / 10,
                1,
            );
            let plan = node.prepare_automatic_finalization(timestamp_ms);
            assert!(plan.burned.is_some());

            match step % 3 {
                0 => {
                    let _ = node.blinded_burn_with_fee(1, 0, node.chain_height() + 4);
                }
                1 => {
                    let recipient = wallets[(step + 1) % wallets.len()].address();
                    let _ =
                        node.blinded_transfer_with_fee(recipient, 1, 0, node.chain_height() + 4);
                }
                _ => {}
            }

            let work = node
                .prepare_next_block_with_local_anchor(timestamp_ms)
                .unwrap();
            let vdf_output = run_vdf(work.vdf_seed(), work.vdf_rounds());
            let block = node
                .complete_prepared_block_at(work, vdf_output, timestamp_ms)
                .unwrap();
            peer_ledger.apply_block_at(block, u64::MAX).unwrap();
            producer_ledger = node.clone_ledger();
            assert_eq!(
                producer_ledger.status().tip_hash,
                peer_ledger.status().tip_hash
            );
        }
    }

    #[derive(Clone, Debug)]
    struct ChaosRng {
        state: u64,
    }

    impl ChaosRng {
        fn new(seed: u64) -> Self {
            Self {
                state: seed ^ 0x517c_c1b7_2722_0a95,
            }
        }

        fn next_u64(&mut self) -> u64 {
            self.state = self
                .state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.state
        }

        fn index(&mut self, len: usize) -> usize {
            assert!(len > 0);
            (self.next_u64() as usize) % len
        }
    }

    #[test]
    fn sparse_network_stays_bounded_under_generated_actions() {
        const NODES: usize = 10;
        const ROUNDS: usize = 36;
        const SETTLE_BLOCKS: u64 = super::MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS + 10;
        const QUIET_BLOCKS: usize = SETTLE_BLOCKS as usize + 4;

        let mut rng = ChaosRng::new(0x1aba_0100);
        let wallets = (0..NODES)
            .map(|index| Wallet::from_seed(&format!("sparse-network-chaos-{index}")))
            .collect::<Vec<_>>();
        let allocations = wallets
            .iter()
            .map(|wallet| (wallet.address().to_string(), 75 * MICRO_IUNA))
            .collect::<BTreeMap<_, _>>();
        let genesis_burns = wallets
            .iter()
            .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
            .collect::<Vec<_>>();
        let ledger = Ledger::new_with_genesis_burns(allocations, genesis_burns, 1)
            .expect("chaos genesis is valid");
        let node_ids = (0..NODES)
            .map(|index| format!("n{index}"))
            .collect::<Vec<_>>();
        let peers = sparse_chaos_peers(NODES, &mut rng);
        let mut offline_until = vec![0_usize; NODES];
        let mut pending_since = BTreeMap::new();
        let mut network = InMemoryNetwork::default();

        for (index, wallet) in wallets.iter().enumerate() {
            let joined = Ledger::from_snapshot(ledger.snapshot()).expect("node joins valid chain");
            let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
                wallet.clone(),
                joined,
                true,
                MICRO_IUNA / 10,
                0,
            );
            node.set_recovery_vdf_top_rank_percent(100);
            network.insert(node_ids[index].clone(), node);
        }

        for round in 0..ROUNDS {
            let online = online_chaos_nodes(&offline_until, round);
            for index in online.iter().copied().collect::<Vec<_>>() {
                match rng.index(32) {
                    0 => {
                        let recipient = wallets[rng.index(wallets.len())].address().to_string();
                        let expiry_height = network
                            .node(&node_ids[index])
                            .expect("actor node exists")
                            .chain_height()
                            + 16;
                        let _ = network
                            .node_mut(&node_ids[index])
                            .expect("actor node exists")
                            .blinded_transfer_with_fee(recipient, 1, 0, expiry_height);
                    }
                    1 => {
                        attempt_bounded_mine_action(
                            &mut network,
                            &node_ids[index],
                            wallets[index].address(),
                            &mut rng,
                        );
                    }
                    2 if online.len() > 1 => {
                        offline_until[index] = round + 1 + rng.index(4);
                    }
                    _ => {}
                }
            }

            deliver_sparse_chaos_until_idle(
                &mut network,
                &node_ids,
                &peers,
                &online,
                chaos_timestamp(round),
                &mut rng,
            );
            mine_one_sparse_chaos_block(
                &mut network,
                &node_ids,
                &wallets,
                &online,
                chaos_timestamp(round),
            );
            deliver_sparse_chaos_until_idle(
                &mut network,
                &node_ids,
                &peers,
                &online,
                chaos_timestamp(round),
                &mut rng,
            );
            observe_bounded_pending(&network, &node_ids, &mut pending_since, SETTLE_BLOCKS);
        }

        for round in ROUNDS..ROUNDS + QUIET_BLOCKS {
            let online = online_chaos_nodes(&offline_until, round);
            deliver_sparse_chaos_until_idle(
                &mut network,
                &node_ids,
                &peers,
                &online,
                chaos_timestamp(round),
                &mut rng,
            );
            mine_one_sparse_chaos_block(
                &mut network,
                &node_ids,
                &wallets,
                &online,
                chaos_timestamp(round),
            );
            deliver_sparse_chaos_until_idle(
                &mut network,
                &node_ids,
                &peers,
                &online,
                chaos_timestamp(round),
                &mut rng,
            );
            observe_bounded_pending(&network, &node_ids, &mut pending_since, SETTLE_BLOCKS);
        }

        let all_online = (0..NODES).collect::<BTreeSet<_>>();
        for round in ROUNDS + QUIET_BLOCKS..ROUNDS + QUIET_BLOCKS + SETTLE_BLOCKS as usize * 4 {
            deliver_sparse_chaos_until_idle(
                &mut network,
                &node_ids,
                &peers,
                &all_online,
                chaos_timestamp(round),
                &mut rng,
            );
            if !sparse_chaos_has_pending(&network, &node_ids) {
                break;
            }
            mine_one_sparse_chaos_block(
                &mut network,
                &node_ids,
                &wallets,
                &all_online,
                chaos_timestamp(round),
            );
            deliver_sparse_chaos_until_idle(
                &mut network,
                &node_ids,
                &peers,
                &all_online,
                chaos_timestamp(round),
                &mut rng,
            );
            observe_bounded_pending(&network, &node_ids, &mut pending_since, SETTLE_BLOCKS);
        }
        assert_sparse_chaos_converged(&network, &node_ids);
        assert_sparse_chaos_mempools_empty(&network, &node_ids);
    }

    fn sparse_chaos_peers(nodes: usize, rng: &mut ChaosRng) -> Vec<Vec<usize>> {
        let mut peers = vec![BTreeSet::new(); nodes];
        for index in 0..nodes {
            let next = (index + 1) % nodes;
            peers[index].insert(next);
            peers[next].insert(index);
        }
        for index in 0..nodes {
            let target_degree = 1 + rng.index(4);
            while peers[index].len() < target_degree {
                let peer = rng.index(nodes);
                if peer != index {
                    peers[index].insert(peer);
                    peers[peer].insert(index);
                }
            }
        }
        peers
            .into_iter()
            .map(|set| set.into_iter().take(4).collect())
            .collect()
    }

    fn online_chaos_nodes(offline_until: &[usize], round: usize) -> BTreeSet<usize> {
        offline_until
            .iter()
            .enumerate()
            .filter_map(|(index, until)| (*until <= round).then_some(index))
            .collect()
    }

    fn chaos_timestamp(round: usize) -> u64 {
        (round as u64 + 1) * (RECOVERY_BLOCK_DELAY_MS + VDF_TARGET_BLOCK_MS)
    }

    fn attempt_bounded_mine_action(
        network: &mut InMemoryNetwork,
        node_id: &str,
        recipient: &str,
        rng: &mut ChaosRng,
    ) {
        let outcome = network
            .node(node_id)
            .expect("mine actor exists")
            .ledger()
            .search_mine(recipient, rng.next_u64(), 0, 4)
            .expect("bounded mine search is valid");
        let Some(transaction) = outcome.transaction else {
            return;
        };
        let _ = network
            .node_mut(node_id)
            .expect("mine actor exists")
            .receive_mine_action(transaction);
    }

    fn mine_one_sparse_chaos_block(
        network: &mut InMemoryNetwork,
        node_ids: &[String],
        wallets: &[Wallet],
        online: &BTreeSet<usize>,
        timestamp_ms: u64,
    ) {
        let Some(reference_index) = highest_online_node(network, node_ids, online) else {
            return;
        };
        let reference = network
            .node(&node_ids[reference_index])
            .expect("reference node exists");
        let leader = reference.ledger().expected_leader_for_next_block();
        let mut candidates = Vec::new();
        if let Some(index) = leader
            .as_deref()
            .and_then(|leader| {
                wallets
                    .iter()
                    .position(|wallet| wallet.address() == leader)
                    .filter(|index| online.contains(index))
            })
            .filter(|index| {
                network.node(&node_ids[*index]).unwrap().chain_height() == reference.chain_height()
            })
        {
            candidates.push(index);
        }
        candidates.push(reference_index);
        candidates.extend(online.iter().copied().filter(|index| {
            network.node(&node_ids[*index]).unwrap().chain_height() == reference.chain_height()
        }));
        let mut seen = BTreeSet::new();
        for producer_index in candidates {
            if !seen.insert(producer_index) {
                continue;
            }
            let mut producer = network
                .node(&node_ids[producer_index])
                .expect("producer node exists")
                .clone();
            let plan = producer.prepare_automatic_finalization(timestamp_ms);
            let Some(work) = plan.work else {
                continue;
            };
            let block = work.finish_at(
                &wallets[producer_index],
                "preverified-chaos-vdf".to_string(),
                timestamp_ms,
            );
            network
                .node_mut(&node_ids[producer_index])
                .expect("producer node exists")
                .receive_preverified_block_at(block, timestamp_ms)
                .expect("mock-VDF block applies locally");
            return;
        }
    }

    fn highest_online_node(
        network: &InMemoryNetwork,
        node_ids: &[String],
        online: &BTreeSet<usize>,
    ) -> Option<usize> {
        online.iter().copied().max_by_key(|index| {
            network
                .node(&node_ids[*index])
                .expect("online node exists")
                .chain_height()
        })
    }

    fn deliver_sparse_chaos_until_idle(
        network: &mut InMemoryNetwork,
        node_ids: &[String],
        peers: &[Vec<usize>],
        online: &BTreeSet<usize>,
        timestamp_ms: u64,
        rng: &mut ChaosRng,
    ) {
        for _ in 0..512 {
            let mut progressed =
                sync_sparse_chaos_once(network, node_ids, peers, online, timestamp_ms);
            progressed |=
                deliver_sparse_chaos_once(network, node_ids, peers, online, timestamp_ms, rng);
            if !progressed {
                return;
            }
        }
        panic!("sparse chaos network did not become idle");
    }

    fn sync_sparse_chaos_once(
        network: &mut InMemoryNetwork,
        node_ids: &[String],
        peers: &[Vec<usize>],
        online: &BTreeSet<usize>,
        timestamp_ms: u64,
    ) -> bool {
        let mut syncs = Vec::new();
        for from in online {
            let from_height = network.node(&node_ids[*from]).unwrap().chain_height();
            for to in &peers[*from] {
                if !online.contains(to) {
                    continue;
                }
                let to_height = network.node(&node_ids[*to]).unwrap().chain_height();
                if from_height > to_height {
                    let blocks = network
                        .node(&node_ids[*from])
                        .unwrap()
                        .blocks_from(to_height + 1, 16);
                    syncs.push((*to, blocks));
                }
            }
        }

        let mut progressed = false;
        for (to, blocks) in syncs {
            for block in blocks {
                let before = network.node(&node_ids[to]).unwrap().chain_height();
                receive_sparse_chaos_block(network, &node_ids[to], block, timestamp_ms);
                progressed |= network.node(&node_ids[to]).unwrap().chain_height() > before;
            }
        }
        progressed
    }

    fn deliver_sparse_chaos_once(
        network: &mut InMemoryNetwork,
        node_ids: &[String],
        peers: &[Vec<usize>],
        online: &BTreeSet<usize>,
        timestamp_ms: u64,
        rng: &mut ChaosRng,
    ) -> bool {
        let mut outbound = Vec::new();
        for from in online {
            let node = network
                .node_mut(&node_ids[*from])
                .expect("online node exists");
            outbound.extend(
                node.drain_outbox()
                    .into_iter()
                    .map(|envelope| (*from, envelope)),
            );
        }
        if outbound.is_empty() {
            return false;
        }

        while !outbound.is_empty() {
            let index = rng.index(outbound.len());
            let (from, envelope) = outbound.swap_remove(index);
            for to in &peers[from] {
                if online.contains(to) {
                    receive_sparse_chaos_envelope(
                        network,
                        &node_ids[*to],
                        envelope.clone(),
                        timestamp_ms,
                    );
                }
            }
        }
        true
    }

    fn receive_sparse_chaos_envelope(
        network: &mut InMemoryNetwork,
        node_id: &str,
        envelope: GossipEnvelope,
        timestamp_ms: u64,
    ) {
        match envelope {
            GossipEnvelope::Block(block) => {
                receive_sparse_chaos_block(network, node_id, block, timestamp_ms);
            }
            other => {
                if let Err(error) = network
                    .node_mut(node_id)
                    .expect("node exists")
                    .receive(other)
                {
                    let message = error.to_string();
                    assert!(
                        message.contains("mine transaction anchor is not on this chain")
                            || message.contains("conflicts with an existing pending transaction")
                            || message.contains("blinded transaction expired"),
                        "unexpected sparse chaos delivery error: {message}"
                    );
                }
            }
        }
    }

    fn receive_sparse_chaos_block(
        network: &mut InMemoryNetwork,
        node_id: &str,
        block: crate::domain::Block,
        timestamp_ms: u64,
    ) {
        if let Err(error) = network
            .node_mut(node_id)
            .expect("node exists")
            .receive_preverified_block_at(block, timestamp_ms)
        {
            let message = error.to_string();
            assert!(
                message.contains("expected block height")
                    || message.contains("same-height fork")
                    || message.contains("block is already known"),
                "unexpected sparse chaos block error: {message}"
            );
        }
    }

    fn observe_bounded_pending(
        network: &InMemoryNetwork,
        node_ids: &[String],
        pending_since: &mut BTreeMap<String, u64>,
        settle_blocks: u64,
    ) {
        let mut current = BTreeSet::new();
        for node_id in node_ids {
            let node = network.node(node_id).expect("node exists");
            for id in pending_item_ids(node) {
                current.insert(id);
            }
        }
        let max_height = node_ids
            .iter()
            .map(|id| network.node(id).unwrap().chain_height())
            .max()
            .unwrap_or_default();
        pending_since.retain(|id, _| current.contains(id));
        for id in current {
            pending_since.entry(id).or_insert(max_height);
        }
        for (id, first_height) in pending_since {
            assert!(
                max_height.saturating_sub(*first_height) <= settle_blocks,
                "pending item {id} stayed in mempool for more than {settle_blocks} blocks"
            );
        }
    }

    fn pending_item_ids(node: &NodeCore) -> Vec<String> {
        let mut ids = Vec::new();
        ids.extend(
            node.pending_transactions()
                .into_iter()
                .map(|tx| format!("tx:{}", tx.signature())),
        );
        ids.extend(
            node.pending_blinded_transactions()
                .into_iter()
                .map(|tx| format!("commit:{}@{}", tx.commitment, tx.expires_at_height)),
        );
        ids.extend(
            node.pending_blinded_reveals()
                .into_iter()
                .map(|reveal| format!("reveal:{}", reveal.commitment)),
        );
        ids
    }

    fn assert_sparse_chaos_converged(network: &InMemoryNetwork, node_ids: &[String]) {
        let first = network.node(&node_ids[0]).expect("first node exists");
        let height = first.chain_height();
        let tip = first.ledger().status().tip_hash.clone();
        for node_id in node_ids.iter().skip(1) {
            let node = network.node(node_id).expect("node exists");
            assert_eq!(node.chain_height(), height, "{node_id} height diverged");
            assert_eq!(
                node.ledger().status().tip_hash,
                tip,
                "{node_id} tip diverged"
            );
        }
    }

    fn sparse_chaos_has_pending(network: &InMemoryNetwork, node_ids: &[String]) -> bool {
        node_ids.iter().any(|node_id| {
            let node = network.node(node_id).expect("node exists");
            !node.pending_transactions().is_empty()
                || !node.pending_blinded_transactions().is_empty()
                || !node.pending_blinded_reveals().is_empty()
        })
    }

    fn assert_sparse_chaos_mempools_empty(network: &InMemoryNetwork, node_ids: &[String]) {
        for node_id in node_ids {
            let node = network.node(node_id).expect("node exists");
            let pending = pending_item_ids(node);
            assert!(
                pending.is_empty(),
                "{node_id} at height {} still has pending mempool items: {pending:?}",
                node.chain_height()
            );
        }
    }
}

#[derive(Debug, Default)]
pub struct InMemoryNetwork {
    nodes: BTreeMap<String, NodeCore>,
}

impl InMemoryNetwork {
    pub fn insert(&mut self, id: impl Into<String>, node: NodeCore) {
        self.nodes.insert(id.into(), node);
    }

    pub fn node(&self, id: &str) -> Option<&NodeCore> {
        self.nodes.get(id)
    }

    pub fn node_mut(&mut self, id: &str) -> Option<&mut NodeCore> {
        self.nodes.get_mut(id)
    }

    pub fn deliver_until_idle(&mut self) -> Result<()> {
        loop {
            let mut outbound = Vec::new();
            for (id, node) in &mut self.nodes {
                for envelope in node.drain_outbox() {
                    outbound.push((id.clone(), envelope));
                }
            }

            if outbound.is_empty() {
                return Ok(());
            }

            for (from, envelope) in outbound {
                for (id, node) in &mut self.nodes {
                    if *id != from {
                        receive_in_memory_envelope(node, envelope.clone())?;
                    }
                }
            }
        }
    }

    pub fn gossip_mempools_once(&mut self) -> Result<()> {
        let mut outbound = Vec::new();
        for (id, node) in &mut self.nodes {
            for envelope in node.mempool_gossip() {
                outbound.push((id.clone(), envelope));
            }
        }

        for (from, envelope) in outbound {
            for (id, node) in &mut self.nodes {
                if *id != from {
                    receive_in_memory_envelope(node, envelope.clone())?;
                }
            }
        }
        Ok(())
    }

    pub fn sync_node_from_peer(&mut self, from: &str, to: &str, limit: usize) -> Result<bool> {
        let from_height = self
            .nodes
            .get(to)
            .map(|node| node.chain_height() + 1)
            .ok_or_else(|| anyhow::anyhow!("missing sync target node {to}"))?;
        let blocks = self
            .nodes
            .get(from)
            .map(|node| node.blocks_from(from_height, limit))
            .ok_or_else(|| anyhow::anyhow!("missing sync source node {from}"))?;
        if blocks.is_empty() {
            return Ok(false);
        }

        self.nodes
            .get_mut(to)
            .expect("sync target exists")
            .receive(GossipEnvelope::Blocks { blocks })?;
        Ok(true)
    }
}

fn receive_in_memory_envelope(node: &mut NodeCore, envelope: GossipEnvelope) -> Result<()> {
    let transaction_like = matches!(
        envelope,
        GossipEnvelope::BlindedTransaction(_)
            | GossipEnvelope::BlindedTransactions { .. }
            | GossipEnvelope::MineAction(_)
            | GossipEnvelope::MineActions { .. }
            | GossipEnvelope::BlindedReveal(_)
            | GossipEnvelope::BlindedReveals { .. }
    );
    match node.receive(envelope) {
        Ok(()) => Ok(()),
        Err(_) if transaction_like => Ok(()),
        Err(error) => Err(error),
    }
}
