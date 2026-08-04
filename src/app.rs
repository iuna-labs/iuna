use std::{
    collections::BTreeMap,
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

use crate::domain::{
    Amount, BlindedReveal, BlindedTransaction, Block, BuiltBlindedTransaction, BurnLeaderRank,
    ChainSnapshot, ChainStatus, DEFAULT_FEE_PER_BYTE, DEFAULT_TRANSACTION_FEE, Ledger,
    MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS, MINE_FINALIZER_FEE, OutPoint, OwnedBlindedTransaction,
    PreparedBlock, StratumMineShare, StratumMineTemplate, Transaction, TransactionSubmitOutcome,
    VDF_TARGET_BLOCK_MS, Wallet, run_vdf,
};

pub type SharedNode = Arc<Mutex<NodeCore>>;
pub type SharedPeerBook = Arc<Mutex<PeerBook>>;

pub const DEFAULT_BURN_PER_BLOCK: Amount = 0;
pub const DEFAULT_VDF_ROUNDS: u32 = 67_000_000;
pub const PROTOCOL_VERSION: u32 = 1;
pub const NETWORK_ID: &str = "iuna-devnet-v2";
pub const BLOCK_REQUEST_LIMIT: usize = 128;
pub const TRANSACTION_BATCH_LIMIT: usize = 128;
const IMPORT_REBROADCAST_LIMIT: usize = 128;
pub const PEER_MISBEHAVIOR_BAN_SCORE: u32 = 3;
pub const PEER_MISBEHAVIOR_BAN_MS: u64 = 10 * 60 * 1_000;
pub const PEER_CLOCK_OFFSET_ACCEPTANCE_MS: i64 = 10 * 60 * 1_000;
const PEER_CLOCK_OFFSET_STALE_MS: u64 = 20 * 60 * 1_000;
const AUTO_POW_NONCE_ATTEMPTS_PER_TICK: u64 = 8;
const AUTO_PLAINTEXT_BURN_BEFORE_RECOVERY_MS: u64 = 60_000;
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
    BlindedReveal(BlindedReveal),
    BlindedReveals {
        reveals: Vec<BlindedReveal>,
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
    burn_per_block: Amount,
    burn_fee: Amount,
    last_auto_burn_height: Option<u64>,
    last_auto_pow_mine_anchor: Option<String>,
    last_auto_pow_mine_status: Option<String>,
    auto_pow_mine_cursor: Option<AutoPowMineCursor>,
    owned_blinded_transactions: BTreeMap<String, BlindedTransaction>,
    owned_blinded_reveals: BTreeMap<String, BlindedReveal>,
    owned_blinded_payloads: BTreeMap<String, Transaction>,
    owned_blinded_outbox_version: u64,
    local_block_anchor_burn: Option<(u64, Transaction)>,
    outbox: Vec<GossipEnvelope>,
}

impl NodeCore {
    pub fn new(config: NodeConfig) -> Self {
        let ledger = Ledger::new(config.genesis_allocations, config.vdf_rounds);
        Self::from_ledger_with_burn_fee(
            config.wallet,
            ledger,
            config.burn_per_block,
            config.burn_fee,
        )
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
        )
    }

    fn from_node_wallet_with_burn_fee_and_enabled(
        wallet: NodeWallet,
        ledger: Ledger,
        automatic_mining_enabled: bool,
        burn_per_block: Amount,
        burn_fee: Amount,
    ) -> Self {
        Self {
            wallet,
            ledger,
            automatic_mining_enabled,
            pow_mining_enabled: false,
            burn_per_block,
            burn_fee,
            last_auto_burn_height: None,
            last_auto_pow_mine_anchor: None,
            last_auto_pow_mine_status: None,
            auto_pow_mine_cursor: None,
            owned_blinded_transactions: BTreeMap::new(),
            owned_blinded_reveals: BTreeMap::new(),
            owned_blinded_payloads: BTreeMap::new(),
            owned_blinded_outbox_version: 0,
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
        self.last_auto_pow_mine_anchor = None;
        self.last_auto_pow_mine_status = None;
        self.auto_pow_mine_cursor = None;
        self.owned_blinded_transactions.clear();
        self.owned_blinded_reveals.clear();
        self.owned_blinded_payloads.clear();
        self.bump_owned_blinded_outbox_version();
        self.local_block_anchor_burn = None;
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub(crate) fn clone_ledger(&self) -> Ledger {
        self.ledger.clone()
    }

    pub fn wallet_view_ledger(&self) -> Result<Ledger> {
        self.wallet_build_ledger()
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
            if !self.ledger.has_blinded_transaction(&commitment)
                && self
                    .ledger
                    .submit_blinded_transaction(owned.transaction.clone())?
            {
                self.outbox.push(GossipEnvelope::BlindedTransaction(
                    owned.transaction.clone(),
                ));
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

    pub fn mempool_gossip(&self) -> Vec<GossipEnvelope> {
        let mut gossip = Vec::new();
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
            wallet_balance: self.ledger.balance_of(self.wallet.address()),
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
            },
            stratum: StratumStatus {
                enabled: false,
                listen_addr: None,
            },
            chain,
        }
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
        self.submit_transaction_as_owned_blinded(tx)
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
        self.submit_transaction_as_owned_blinded(tx)
    }

    pub fn receive_transaction(&mut self, tx: Transaction) -> Result<TransactionSubmitOutcome> {
        let outcome = self.ledger.submit_transaction_with_outcome(tx.clone())?;
        Ok(outcome)
    }

    pub fn receive_blinded_transaction(&mut self, tx: BlindedTransaction) -> Result<()> {
        if self.ledger.submit_blinded_transaction(tx.clone())? {
            self.outbox.push(GossipEnvelope::BlindedTransaction(tx));
        }
        Ok(())
    }

    pub fn receive_blinded_reveal(&mut self, reveal: BlindedReveal) -> Result<()> {
        if self.ledger.submit_blinded_reveal(reveal.clone())? {
            self.outbox.push(GossipEnvelope::BlindedReveal(reveal));
        }
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

    fn submit_transaction_as_owned_blinded(&mut self, tx: Transaction) -> Result<Transaction> {
        let built = self.ledger.build_blinded_transaction(
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
        for reveal in &block.blinded_reveals {
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
        let expires_at_height = self.default_blinded_transaction_expiry_height();
        converge_fee_by_byte(fee_per_byte, |fee| {
            let tx = ledger.build_burn(self.wallet.unlocked()?, amount, fee)?;
            ledger.build_blinded_transaction(tx, expires_at_height)
        })
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
            ledger.build_blinded_transaction(tx, expires_at_height)
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
        for (commitment, payload) in &self.owned_blinded_payloads {
            if self.ledger.has_unrevealed_blinded_transaction(commitment)
                && !ledger.has_transaction(payload.signature())
            {
                let _ = ledger.submit_transaction(payload.clone())?;
            }
        }
        Ok(ledger)
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

        let wallet_rank = self
            .ledger
            .finalizer_rank_for_next_block(self.wallet.address());
        if wallet_rank.is_none() {
            if self.ledger.recovery_block_available_at(timestamp_ms) {
                match self
                    .ledger
                    .prepare_recovery_block(self.wallet.address(), timestamp_ms)
                {
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

        let wallet_rank = self
            .ledger
            .finalizer_rank_for_next_block(self.wallet.address());
        if wallet_rank.is_none() {
            if self.ledger.recovery_block_available_at(timestamp_ms) {
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
        let outcome = self.wallet_build_ledger()?.search_mine(
            wallet_address,
            cursor.salt,
            cursor.next_nonce,
            AUTO_POW_NONCE_ATTEMPTS_PER_TICK,
        )?;
        let mut searched = outcome.attempts;
        if let Some(cursor) = &mut self.auto_pow_mine_cursor {
            if cursor.anchor == anchor {
                cursor.next_nonce = outcome.next_nonce;
                cursor.searched = cursor.searched.saturating_add(outcome.attempts);
                searched = cursor.searched;
            }
        }
        let Some(tx) = outcome.transaction else {
            self.last_auto_pow_mine_status = Some(format!(
                "searched {searched} PoW nonces for the current tip; no proof yet"
            ));
            return Ok(None);
        };
        self.submit_transaction_as_owned_blinded(tx.clone())?;
        self.last_auto_pow_mine_anchor = Some(anchor);
        self.last_auto_pow_mine_status = Some(format!(
            "queued mine action after {searched} PoW nonce attempts for the current tip"
        ));
        Ok(Some(tx))
    }

    fn prepare_automatic_burn(&mut self, timestamp_ms: u64) -> Result<Option<Transaction>> {
        let current_height = self.ledger.status().height;
        if !self.automatic_mining_enabled {
            return Ok(None);
        }
        if self.burn_per_block == 0 {
            self.last_auto_burn_height = Some(current_height);
            return Ok(None);
        }
        if self.last_auto_burn_height == Some(current_height) {
            return Ok(None);
        }

        let fee_per_byte = self.burn_fee;
        let balance = self.ledger.balance_of(self.wallet.address());
        let mut low = 1;
        let mut high = self.burn_per_block.min(balance);
        let mut best = None;
        while low <= high {
            let amount = low + (high - low) / 2;
            match self.build_blinded_burn_with_fee_rate(amount, fee_per_byte) {
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
        let Some(tx) = best else {
            self.last_auto_burn_height = Some(current_height);
            return Ok(None);
        };
        let burn = tx.payload.clone();
        if self.automatic_burn_needs_plaintext_anchor(timestamp_ms) {
            self.local_block_anchor_burn = Some((current_height, burn.clone()));
        } else {
            self.submit_owned_blinded_transaction(tx)?;
        }
        self.last_auto_burn_height = Some(current_height);
        Ok(Some(burn))
    }

    fn automatic_burn_needs_plaintext_anchor(&self, timestamp_ms: u64) -> bool {
        self.ledger
            .expected_leader_for_next_block()
            .is_none_or(|leader| leader == self.wallet.address())
            || self.ledger.recovery_block_available_at(timestamp_ms)
            || timestamp_ms.saturating_add(AUTO_PLAINTEXT_BURN_BEFORE_RECOVERY_MS)
                >= self.ledger.recovery_block_min_timestamp()
    }

    fn prepare_next_block_with_local_anchor(&self, timestamp_ms: u64) -> Result<PreparedBlock> {
        self.ledger_with_local_block_anchor()?
            .prepare_next_block(self.wallet.address(), timestamp_ms)
    }

    fn prepare_recovery_block_with_local_anchor(&self, timestamp_ms: u64) -> Result<PreparedBlock> {
        self.ledger_with_local_block_anchor()?
            .prepare_recovery_block(self.wallet.address(), timestamp_ms)
    }

    fn ledger_with_local_block_anchor(&self) -> Result<Ledger> {
        let mut ledger = self.ledger.clone();
        let Some((height, burn)) = &self.local_block_anchor_burn else {
            return Ok(ledger);
        };
        if *height == ledger.height() && !ledger.has_transaction(burn.signature()) {
            let _ = ledger.submit_transaction(burn.clone())?;
        }
        Ok(ledger)
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
        let block = self
            .ledger
            .mine_next_block(self.wallet.unlocked()?, timestamp_ms)?;
        self.ledger.apply_locally_mined_block(block.clone())?;
        self.clear_stale_local_block_anchor();
        self.prune_owned_blinded_payloads_for_block(&block);
        self.outbox.push(GossipEnvelope::Block(block.clone()));
        self.publish_owned_reveals_for_block(&block)?;
        Ok(block)
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
            GossipEnvelope::BlindedReveal(reveal) => self.receive_blinded_reveal(reveal),
            GossipEnvelope::BlindedReveals { reveals } => {
                for reveal in reveals {
                    self.receive_blinded_reveal(reveal)?;
                }
                Ok(())
            }
            GossipEnvelope::Block(block) => {
                let previous_height = self.ledger.height();
                self.ledger.apply_block(block.clone())?;
                if self.ledger.height() > previous_height {
                    self.clear_stale_local_block_anchor();
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
            self.last_auto_pow_mine_anchor = None;
            self.last_auto_pow_mine_status = None;
            self.auto_pow_mine_cursor = None;
            self.clear_stale_local_block_anchor();
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
        self.last_auto_pow_mine_anchor = None;
        self.last_auto_pow_mine_status = None;
        self.auto_pow_mine_cursor = None;
        self.clear_stale_local_block_anchor();
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::domain::{
        FinalizerMode, GenesisBurn, Ledger, MICRO_IUNA, MINE_FINALIZER_FEE,
        RECOVERY_BLOCK_DELAY_MS, Transaction, Wallet,
    };

    use super::{GossipEnvelope, NodeConfig, NodeCore};

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
        assert!(node.ledger().pending().is_empty());
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
        assert!(node.ledger().pending().is_empty());
        assert_eq!(node.ledger().pending_blinded_transactions().len(), 1);
        assert!(
            node.status()
                .mining
                .last_auto_pow_mine_status
                .as_deref()
                .unwrap_or_default()
                .contains("queued mine action after")
        );

        let second = (10_000..20_000)
            .map(|timestamp| node.prepare_automatic_mining(timestamp))
            .find(|plan| plan.pow_mined.is_some())
            .expect("automatic PoW should keep searching the same tip after one proof");
        assert_ne!(
            second.pow_mined.as_ref().unwrap().signature(),
            first_mine.signature()
        );
        assert!(node.ledger().pending().is_empty());
        assert_eq!(node.ledger().pending_blinded_transactions().len(), 2);
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
    fn automatic_finalization_does_not_tick_pow_mining() {
        let wallet = Wallet::from_seed("automatic-pow-separated-finalizer-wallet");
        let mut node = NodeCore::new(NodeConfig {
            wallet,
            genesis_allocations: BTreeMap::new(),
            vdf_rounds: 10,
            burn_per_block: 0,
            burn_fee: 0,
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
    fn automatic_pow_mining_uses_protocol_finalizer_fee() {
        let wallet = Wallet::from_seed("automatic-pow-mining-fee-wallet");
        let mut node = NodeCore::new(NodeConfig {
            wallet,
            genesis_allocations: BTreeMap::new(),
            vdf_rounds: 10,
            burn_per_block: 0,
            burn_fee: 0,
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
        let alice = Wallet::from_seed("fee-rate-alice");
        let bob = Wallet::from_seed("fee-rate-bob");
        let mut genesis = BTreeMap::new();
        genesis.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        let ledger = crate::domain::Ledger::new(genesis, 1);
        let mut node = NodeCore::from_ledger(alice, ledger, 0);

        let (transfer, transfer_estimate) = node
            .transfer_with_fee_rate(bob.address(), MICRO_IUNA, 2, &[])
            .unwrap();
        let transfer_blinded_bytes =
            node.ledger().pending_blinded_transactions()[0].fee_rate_size_bytes();
        assert_eq!(transfer_estimate.bytes, transfer_blinded_bytes);
        let minimum_transfer_fee = transfer_blinded_bytes as u64 * 2;
        assert!(transfer.fee() >= minimum_transfer_fee);
        assert!(node.ledger().pending().is_empty());
        assert_eq!(node.ledger().pending_blinded_transactions().len(), 1);

        let (burn, burn_estimate) = node.burn_with_fee_rate(MICRO_IUNA, 3).unwrap();
        let burn_blinded_bytes =
            node.ledger().pending_blinded_transactions()[1].fee_rate_size_bytes();
        assert_eq!(burn_estimate.bytes, burn_blinded_bytes);
        let minimum_burn_fee = burn_blinded_bytes as u64 * 3;
        assert!(burn.fee() >= minimum_burn_fee);
        assert!(node.ledger().pending().is_empty());
        assert_eq!(node.ledger().pending_blinded_transactions().len(), 2);
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
        let non_leader = finalizers
            .iter()
            .find(|wallet| wallet.address() != leader)
            .unwrap()
            .clone();
        let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
            non_leader,
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
    fn automatic_leader_burn_stays_plaintext_for_block_anchor() {
        let alice = Wallet::from_seed("auto-plaintext-burn-alice");
        let bob = Wallet::from_seed("auto-plaintext-burn-bob");
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
        assert!(node.ledger().pending_blinded_transactions().is_empty());
        assert!(node.local_block_anchor_burn.is_some());
        assert!(outbox.is_empty());
        assert!(node.prepare_automatic_finalization(1).work.is_some());
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
            | GossipEnvelope::BlindedReveal(_)
            | GossipEnvelope::BlindedReveals { .. }
    );
    match node.receive(envelope) {
        Ok(()) => Ok(()),
        Err(_) if transaction_like => Ok(()),
        Err(error) => Err(error),
    }
}
