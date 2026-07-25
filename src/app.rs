use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::domain::{
    Amount, Block, ChainSnapshot, ChainStatus, DEFAULT_FEE_PER_BYTE, DEFAULT_TRANSACTION_FEE,
    Ledger, OutPoint, PreparedBlock, Transaction, VDF_TARGET_BLOCK_MS, Wallet, run_vdf,
};

pub type SharedNode = Arc<Mutex<NodeCore>>;
pub type SharedPeerBook = Arc<Mutex<PeerBook>>;

pub const DEFAULT_BURN_PER_BLOCK: Amount = 0;
pub const DEFAULT_VDF_ROUNDS: u32 = 67_000_000;
pub const PROTOCOL_VERSION: u32 = 1;
pub const NETWORK_ID: &str = "luun-devnet-v2";
pub const BLOCK_REQUEST_LIMIT: usize = 128;
const IMPORT_REBROADCAST_LIMIT: usize = 128;

#[derive(Clone, Debug)]
pub struct NodeConfig {
    pub wallet: Wallet,
    pub genesis_allocations: BTreeMap<String, Amount>,
    pub vdf_rounds: u32,
    pub burn_per_block: Amount,
    pub burn_fee: Amount,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeeEstimate {
    pub bytes: usize,
    pub fee: Amount,
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
    },
    ChainSnapshotRequest,
    BlockRangeRequest {
        from_height: u64,
        limit: usize,
    },
    TransactionRequest {
        signatures: Vec<String>,
    },
    BlockRequest {
        hashes: Vec<String>,
    },
    Inventory {
        txs: Vec<String>,
        blocks: Vec<BlockInventory>,
    },
    TransactionAck {
        accepted: Vec<String>,
        rejected: Vec<TransactionRejection>,
    },
    Transaction(Transaction),
    Transactions {
        transactions: Vec<Transaction>,
    },
    Block(Block),
    Blocks {
        blocks: Vec<Block>,
    },
    ChainSnapshot(ChainSnapshot),
    PeerAnnouncement {
        address: String,
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
    pub height: u64,
    pub tip_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlockInventory {
    pub height: u64,
    pub hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransactionRejection {
    pub signature: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NodeStatus {
    pub wallet_address: String,
    pub wallet_balance: Amount,
    pub wallet_locked: bool,
    pub launch_profile: LaunchProfileStatus,
    pub mining: MiningStatus,
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
    pub vdf_rounds: u32,
    pub vdf_target_block_ms: u64,
    pub current_leader: Option<String>,
    pub wallet_is_current_leader: bool,
    pub last_auto_burn_height: Option<u64>,
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

#[derive(Clone, Debug)]
pub struct NodeCore {
    wallet: NodeWallet,
    ledger: Ledger,
    automatic_mining_enabled: bool,
    pow_mining_enabled: bool,
    pow_mine_fee: Amount,
    burn_per_block: Amount,
    burn_fee: Amount,
    last_auto_burn_height: Option<u64>,
    last_auto_pow_mine_anchor: Option<String>,
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
            pow_mine_fee: DEFAULT_FEE_PER_BYTE,
            burn_per_block,
            burn_fee,
            last_auto_burn_height: None,
            last_auto_pow_mine_anchor: None,
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
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub(crate) fn clone_ledger(&self) -> Ledger {
        self.ledger.clone()
    }

    pub fn chain(&self) -> &[Block] {
        self.ledger.chain()
    }

    pub fn chain_height(&self) -> u64 {
        self.ledger.height()
    }

    pub fn recent_blocks(&self, limit: usize) -> Vec<Block> {
        self.ledger.recent_blocks(limit)
    }

    pub fn blocks_before(&self, before_height: u64, limit: usize) -> Vec<Block> {
        self.ledger.blocks_before(before_height, limit)
    }

    pub fn pending_transactions(&self) -> Vec<Transaction> {
        self.ledger.pending().to_vec()
    }

    pub fn mempool_gossip(&self) -> Vec<GossipEnvelope> {
        let transactions = self.ledger.pending().to_vec();
        if transactions.is_empty() {
            Vec::new()
        } else {
            vec![GossipEnvelope::Transactions { transactions }]
        }
    }

    pub fn chain_snapshot(&self) -> ChainSnapshot {
        self.ledger.snapshot()
    }

    pub fn hello(&self, listen_addr: Option<String>) -> GossipEnvelope {
        let status = self.ledger.status();
        GossipEnvelope::Hello(ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: self.ledger.genesis_hash().to_string(),
            listen_addr,
            height: status.height,
            tip_hash: status.tip_hash,
        })
    }

    pub fn peer_status(&self) -> GossipEnvelope {
        let status = self.ledger.status();
        GossipEnvelope::PeerStatus {
            height: status.height,
            tip_hash: status.tip_hash,
        }
    }

    pub fn blocks_from(&self, from_height: u64, limit: usize) -> Vec<Block> {
        self.ledger.blocks_from(from_height, limit)
    }

    pub fn transactions_by_signature(&self, signatures: &[String]) -> Vec<Transaction> {
        signatures
            .iter()
            .filter_map(|signature| self.ledger.transaction_by_signature(signature))
            .collect()
    }

    pub fn blocks_by_hash(&self, hashes: &[String]) -> Vec<Block> {
        hashes
            .iter()
            .filter_map(|hash| self.ledger.block_by_hash(hash))
            .collect()
    }

    pub fn missing_inventory_requests(
        &self,
        txs: &[String],
        blocks: &[BlockInventory],
    ) -> Vec<GossipEnvelope> {
        let missing_txs = txs
            .iter()
            .filter(|signature| !self.ledger.has_transaction(signature))
            .cloned()
            .collect::<Vec<_>>();
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
        if !missing_txs.is_empty() {
            requests.push(GossipEnvelope::TransactionRequest {
                signatures: missing_txs,
            });
        }
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
                automatic_pow_mine_fee: self.pow_mine_fee,
                vdf_rounds: self.ledger.vdf_rounds(),
                vdf_target_block_ms: VDF_TARGET_BLOCK_MS,
                current_leader,
                wallet_is_current_leader,
                last_auto_burn_height: self.last_auto_burn_height,
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
        self.prepare_automatic_burn()
    }

    pub fn set_pow_mining_enabled(&mut self, enabled: bool) {
        self.pow_mining_enabled = enabled;
        if !enabled {
            self.last_auto_pow_mine_anchor = None;
        }
    }

    pub fn set_pow_mining_settings(&mut self, enabled: bool, fee: Amount) -> Result<()> {
        self.pow_mining_enabled = enabled;
        self.pow_mine_fee = fee;
        if !enabled {
            self.last_auto_pow_mine_anchor = None;
        }
        Ok(())
    }

    pub fn burn(&mut self, amount: Amount) -> Result<Transaction> {
        self.burn_with_fee(amount, 0)
    }

    pub fn burn_with_fee(&mut self, amount: Amount, fee: Amount) -> Result<Transaction> {
        let tx = self
            .ledger
            .build_burn(self.wallet.unlocked()?, amount, fee)?;
        if self.ledger.submit_transaction(tx.clone())? {
            self.outbox.push(GossipEnvelope::Transaction(tx.clone()));
        }
        Ok(tx)
    }

    pub fn burn_with_fee_rate(
        &mut self,
        amount: Amount,
        fee_per_byte: Amount,
    ) -> Result<(Transaction, FeeEstimate)> {
        let (tx, estimate) = self.build_burn_with_fee_rate(amount, fee_per_byte)?;
        if self.ledger.submit_transaction(tx.clone())? {
            self.outbox.push(GossipEnvelope::Transaction(tx.clone()));
        }
        Ok((tx, estimate))
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
        let tx = self
            .ledger
            .build_transfer(self.wallet.unlocked()?, to, amount, fee)?;
        if self.ledger.submit_transaction(tx.clone())? {
            self.outbox.push(GossipEnvelope::Transaction(tx.clone()));
        }
        Ok(tx)
    }

    pub fn transfer_with_fee_spending(
        &mut self,
        to: impl Into<String>,
        amount: Amount,
        fee: Amount,
        outpoints: &[OutPoint],
    ) -> Result<Transaction> {
        let tx = self.ledger.build_transfer_with_inputs(
            self.wallet.unlocked()?,
            to,
            amount,
            fee,
            outpoints,
        )?;
        if self.ledger.submit_transaction(tx.clone())? {
            self.outbox.push(GossipEnvelope::Transaction(tx.clone()));
        }
        Ok(tx)
    }

    pub fn transfer_with_fee_rate(
        &mut self,
        to: impl Into<String>,
        amount: Amount,
        fee_per_byte: Amount,
        outpoints: &[OutPoint],
    ) -> Result<(Transaction, FeeEstimate)> {
        let (tx, estimate) =
            self.build_transfer_with_fee_rate(to, amount, fee_per_byte, outpoints)?;
        if self.ledger.submit_transaction(tx.clone())? {
            self.outbox.push(GossipEnvelope::Transaction(tx.clone()));
        }
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
        let (tx, _) = self.build_mine_with_fee_rate(self.pow_mine_fee)?;
        if self.ledger.submit_transaction(tx.clone())? {
            self.outbox.push(GossipEnvelope::Transaction(tx.clone()));
        }
        Ok(tx)
    }

    pub fn estimate_mine_fee(&self, fee_per_byte: Amount) -> Result<FeeEstimate> {
        self.build_mine_with_fee_rate(fee_per_byte)
            .map(|(_, estimate)| estimate)
    }

    pub fn receive_transaction(&mut self, tx: Transaction) -> Result<bool> {
        let accepted = self.ledger.submit_transaction(tx.clone())?;
        if accepted {
            self.outbox.push(GossipEnvelope::Transaction(tx));
        }
        Ok(accepted)
    }

    fn build_burn_with_fee_rate(
        &self,
        amount: Amount,
        fee_per_byte: Amount,
    ) -> Result<(Transaction, FeeEstimate)> {
        converge_fee_by_byte(fee_per_byte, |fee| {
            self.ledger.build_burn(self.wallet.unlocked()?, amount, fee)
        })
    }

    fn build_transfer_with_fee_rate(
        &self,
        to: impl Into<String>,
        amount: Amount,
        fee_per_byte: Amount,
        outpoints: &[OutPoint],
    ) -> Result<(Transaction, FeeEstimate)> {
        let to = to.into();
        converge_fee_by_byte(fee_per_byte, |fee| {
            if outpoints.is_empty() {
                self.ledger
                    .build_transfer(self.wallet.unlocked()?, to.clone(), amount, fee)
            } else {
                self.ledger.build_transfer_with_inputs(
                    self.wallet.unlocked()?,
                    to.clone(),
                    amount,
                    fee,
                    outpoints,
                )
            }
        })
    }

    fn build_mine_with_fee_rate(&self, fee_per_byte: Amount) -> Result<(Transaction, FeeEstimate)> {
        converge_fee_by_byte(fee_per_byte, |fee| {
            self.ledger.build_mine_with_fee(self.wallet.address(), fee)
        })
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
        match self.complete_prepared_block(work, vdf_output) {
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
            Err(error) => Some(format!("automatic PoW mining failed: {error:#}")),
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

        match self.prepare_automatic_burn() {
            Ok(tx) => plan.burned = tx,
            Err(error) => {
                plan.skipped_reason = Some(format!("automatic burn failed: {error:#}"));
                return plan;
            }
        }

        let selected_leader = self.ledger.expected_leader_for_next_block();
        if selected_leader
            .as_deref()
            .is_some_and(|leader| leader != self.wallet.address())
        {
            plan.skipped_reason = selected_leader.map(|leader| {
                format!("wallet is waiting for selected leader {leader} to finish the VDF")
            });
            return plan;
        }

        match self
            .ledger
            .prepare_next_block(self.wallet.address(), timestamp_ms)
        {
            Ok(work) => {
                plan.work = Some(work);
            }
            Err(error) => {
                plan.skipped_reason = Some(format!("{error:#}"));
            }
        }

        plan
    }

    fn prepare_automatic_pow_mine(&mut self) -> Result<Option<Transaction>> {
        if !self.pow_mining_enabled {
            return Ok(None);
        }
        let anchor = self
            .ledger
            .chain()
            .last()
            .map(|block| block.hash.clone())
            .context("ledger has no anchor block")?;
        if self.last_auto_pow_mine_anchor.as_deref() == Some(anchor.as_str())
            || self.wallet_has_mine_for_anchor(&anchor)
        {
            return Ok(None);
        }
        let (tx, _) = self.build_mine_with_fee_rate(self.pow_mine_fee)?;
        if self.ledger.submit_transaction(tx.clone())? {
            self.last_auto_pow_mine_anchor = Some(anchor);
            self.outbox.push(GossipEnvelope::Transaction(tx.clone()));
            return Ok(Some(tx));
        }
        Ok(None)
    }

    fn wallet_has_mine_for_anchor(&self, anchor: &str) -> bool {
        self.ledger.pending().iter().any(|tx| {
            matches!(
                tx,
                Transaction::Mine {
                    output,
                    anchor: tx_anchor,
                    ..
                } if tx_anchor == anchor && output.address == self.wallet.address()
            )
        })
    }

    fn prepare_automatic_burn(&mut self) -> Result<Option<Transaction>> {
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
            match self.build_burn_with_fee_rate(amount, fee_per_byte) {
                Ok((tx, estimate)) => {
                    let fits = amount
                        .checked_add(estimate.fee)
                        .is_some_and(|required| required <= balance);
                    if fits {
                        best = Some(tx);
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
        if self.ledger.submit_transaction(tx.clone())? {
            self.outbox.push(GossipEnvelope::Transaction(tx.clone()));
        }
        self.last_auto_burn_height = Some(current_height);
        Ok(Some(tx))
    }

    pub fn mine_one_at(&mut self, timestamp_ms: u64) -> Result<Block> {
        let block = self
            .ledger
            .mine_next_block(self.wallet.unlocked()?, timestamp_ms)?;
        self.ledger.apply_locally_mined_block(block.clone())?;
        self.outbox.push(GossipEnvelope::Block(block.clone()));
        Ok(block)
    }

    pub fn complete_prepared_block(
        &mut self,
        work: PreparedBlock,
        vdf_output: String,
    ) -> Result<Block> {
        let block = work.finish(self.wallet.unlocked()?, vdf_output);
        self.ledger.apply_locally_mined_block(block.clone())?;
        self.outbox.push(GossipEnvelope::Block(block.clone()));
        Ok(block)
    }

    pub fn receive(&mut self, envelope: GossipEnvelope) -> Result<()> {
        match envelope {
            GossipEnvelope::Hello(_)
            | GossipEnvelope::PeerStatus { .. }
            | GossipEnvelope::ChainSnapshotRequest
            | GossipEnvelope::BlockRangeRequest { .. }
            | GossipEnvelope::TransactionRequest { .. }
            | GossipEnvelope::BlockRequest { .. }
            | GossipEnvelope::TransactionAck { .. }
            | GossipEnvelope::Inventory { .. } => Ok(()),
            GossipEnvelope::Transaction(tx) => {
                self.receive_transaction(tx)?;
                Ok(())
            }
            GossipEnvelope::Transactions { transactions } => {
                for tx in transactions {
                    self.receive_transaction(tx)?;
                }
                Ok(())
            }
            GossipEnvelope::Block(block) => {
                let previous_height = self.ledger.height();
                self.ledger.apply_block(block.clone())?;
                if self.ledger.height() > previous_height {
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
                        imported.push(block);
                    }
                }
                for block in imported {
                    self.outbox.push(GossipEnvelope::Block(block));
                }
                Ok(())
            }
            GossipEnvelope::ChainSnapshot(snapshot) => self.import_chain_snapshot(snapshot),
            GossipEnvelope::PeerAnnouncement { .. } | GossipEnvelope::PeerList { .. } => Ok(()),
        }
    }

    pub(crate) fn receive_preverified_block(&mut self, block: Block) -> Result<()> {
        let previous_height = self.ledger.height();
        self.ledger.apply_preverified_block(block.clone())?;
        if self.ledger.height() > previous_height {
            self.outbox.push(GossipEnvelope::Block(block));
        }
        Ok(())
    }

    pub(crate) fn block_requires_vdf_verification(&self, block: &Block) -> Result<bool> {
        self.ledger.block_requires_vdf_verification(block)
    }

    pub fn import_chain_snapshot(&mut self, snapshot: ChainSnapshot) -> Result<()> {
        let previous_height = self.ledger.height();
        let imported = self.ledger.extend_from_snapshot(snapshot)?;
        if imported {
            self.last_auto_burn_height = None;
            self.last_auto_pow_mine_anchor = None;
            self.enqueue_imported_blocks(previous_height);
        }
        Ok(())
    }

    pub(crate) fn import_verified_ledger(&mut self, ledger: Ledger) -> Result<bool> {
        if ledger.genesis_hash() != self.ledger.genesis_hash() {
            anyhow::bail!("chain snapshot genesis does not match local chain");
        }
        let previous_height = self.ledger.height();
        if ledger.height() <= previous_height {
            return Ok(false);
        }

        self.ledger = ledger;
        self.last_auto_burn_height = None;
        self.last_auto_pow_mine_anchor = None;
        self.enqueue_imported_blocks(previous_height);
        Ok(true)
    }

    pub fn drain_outbox(&mut self) -> Vec<GossipEnvelope> {
        std::mem::take(&mut self.outbox)
    }

    fn enqueue_imported_blocks(&mut self, previous_height: u64) {
        if self.ledger.height() <= previous_height {
            return;
        }
        let blocks = self
            .ledger
            .blocks_from(previous_height + 1, IMPORT_REBROADCAST_LIMIT);
        if !blocks.is_empty() {
            self.outbox.push(GossipEnvelope::Blocks { blocks });
        }
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

    pub fn addresses(&self) -> Vec<String> {
        self.peers
            .values()
            .filter(|peer| peer.direction != PeerDirection::Inbound)
            .map(|peer| peer.address.clone())
            .collect()
    }

    pub fn addresses_except(&self, excluded: &str) -> Vec<String> {
        self.addresses()
            .into_iter()
            .filter(|address| address != excluded)
            .collect()
    }

    pub fn list(&self) -> Vec<PeerInfo> {
        self.peers.values().cloned().collect()
    }

    pub fn record_sent(&mut self, address: &str, count: u64) {
        let peer = self.ensure(address, PeerDirection::Outbound);
        peer.messages_sent += count;
        peer.last_error = None;
    }

    pub fn record_status(&mut self, address: &str, height: u64, tip_hash: String) {
        let peer = self.ensure(address, PeerDirection::Outbound);
        peer.last_known_height = Some(height);
        peer.last_known_tip_hash = Some(tip_hash);
    }

    pub fn record_error(&mut self, address: &str, error: impl Into<String>) {
        let peer = self.ensure(address, PeerDirection::Outbound);
        peer.last_error = Some(error.into());
    }

    pub fn record_inbound_error(&mut self, address: &str, error: impl Into<String>) {
        let peer = self.ensure(address, PeerDirection::Inbound);
        peer.last_error = Some(error.into());
    }

    pub fn record_received(&mut self, address: &str, count: u64) {
        let peer = self.ensure(address, PeerDirection::Inbound);
        peer.messages_received += count;
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
    pub last_error: Option<String>,
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
            last_error: None,
        }
    }
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

fn converge_fee_by_byte(
    fee_per_byte: Amount,
    mut build: impl FnMut(Amount) -> Result<Transaction>,
) -> Result<(Transaction, FeeEstimate)> {
    let mut fee = 0;
    let mut best = None;
    for _ in 0..64 {
        let tx = build(fee)?;
        let bytes = tx.serialized_size_bytes()?;
        let required_fee = fee_per_byte
            .checked_mul(bytes as Amount)
            .context("fee per byte times transaction bytes overflows")?;
        if fee == required_fee {
            return Ok((tx, FeeEstimate { bytes, fee }));
        }
        if fee > required_fee
            && best
                .as_ref()
                .is_none_or(|(_, estimate): &(Transaction, FeeEstimate)| fee < estimate.fee)
        {
            best = Some((tx, FeeEstimate { bytes, fee }));
        }
        fee = required_fee;
    }

    let tx = build(fee)?;
    let bytes = tx.serialized_size_bytes()?;
    let required_fee = fee_per_byte
        .checked_mul(bytes as Amount)
        .context("fee per byte times transaction bytes overflows")?;
    if fee >= required_fee {
        if best
            .as_ref()
            .is_none_or(|(_, estimate): &(Transaction, FeeEstimate)| fee < estimate.fee)
        {
            best = Some((tx, FeeEstimate { bytes, fee }));
        }
        if let Some(best) = best {
            return Ok(best);
        }
    }
    let tx = build(required_fee)?;
    let bytes = tx.serialized_size_bytes()?;
    let final_required_fee = fee_per_byte
        .checked_mul(bytes as Amount)
        .context("fee per byte times transaction bytes overflows")?;
    if required_fee < final_required_fee {
        bail!("fee per byte did not converge");
    }
    Ok((
        tx,
        FeeEstimate {
            bytes,
            fee: required_fee,
        },
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::domain::{MICRO_LUUN, Transaction, Wallet};

    use super::{NodeConfig, NodeCore};

    #[test]
    fn same_height_verified_import_does_not_reset_auto_burn_guard() {
        let alice = Wallet::from_seed("same-height-import-alice");
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 1_000);
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
    fn automatic_pow_mining_queues_one_mine_action_per_anchor() {
        let wallet = Wallet::from_seed("automatic-pow-mining-wallet");
        let mut node = NodeCore::new(NodeConfig {
            wallet: wallet.clone(),
            genesis_allocations: BTreeMap::new(),
            vdf_rounds: 10,
            burn_per_block: 0,
            burn_fee: 0,
        });

        let disabled = node.prepare_automatic_mining(1);
        assert!(disabled.pow_mined.is_none());
        assert_eq!(
            disabled.skipped_reason.as_deref(),
            Some("automatic mining is off")
        );

        node.set_pow_mining_enabled(true);
        let first = node.prepare_automatic_mining(2);
        let first_mine = first.pow_mined.as_ref().expect("PoW should be queued");
        let Transaction::Mine {
            anchor,
            output,
            difficulty_bits,
            fee,
            ..
        } = first_mine
        else {
            panic!("expected mine transaction");
        };
        assert_eq!(anchor, &node.chain().last().unwrap().hash);
        assert_eq!(output.address, wallet.address());
        let minimum_fee = first_mine.serialized_size_bytes().unwrap() as u64;
        assert!(*fee >= minimum_fee);
        assert!(*fee <= minimum_fee + 1);
        assert_eq!(
            *difficulty_bits,
            node.ledger().current_mine_difficulty_bits()
        );
        assert_eq!(node.ledger().pending().len(), 1);

        let second = node.prepare_automatic_mining(3);
        assert!(second.pow_mined.is_none());
        assert_eq!(node.ledger().pending().len(), 1);
    }

    #[test]
    fn automatic_pow_mining_uses_configured_mine_fee() {
        let wallet = Wallet::from_seed("automatic-pow-mining-fee-wallet");
        let mut node = NodeCore::new(NodeConfig {
            wallet,
            genesis_allocations: BTreeMap::new(),
            vdf_rounds: 10,
            burn_per_block: 0,
            burn_fee: 0,
        });

        let configured_fee_per_byte = 2;
        node.set_pow_mining_settings(true, configured_fee_per_byte)
            .unwrap();
        let plan = node.prepare_automatic_mining(1);
        let mine = plan.pow_mined.expect("PoW should be queued");

        let minimum_fee = mine.serialized_size_bytes().unwrap() as u64 * configured_fee_per_byte;
        assert!(mine.fee() >= minimum_fee);
        assert!(mine.fee() <= minimum_fee + configured_fee_per_byte);
        assert_eq!(
            mine.amount(),
            node.ledger().status().mine_reward - mine.fee()
        );
        assert_eq!(
            node.status().mining.automatic_pow_mine_fee,
            configured_fee_per_byte
        );
    }

    #[test]
    fn automatic_pow_mining_reports_fee_rate_above_reward() {
        let wallet = Wallet::from_seed("automatic-pow-mining-too-high-fee-wallet");
        let mut node = NodeCore::new(NodeConfig {
            wallet,
            genesis_allocations: BTreeMap::new(),
            vdf_rounds: 10,
            burn_per_block: 0,
            burn_fee: 0,
        });

        node.set_pow_mining_settings(true, MICRO_LUUN).unwrap();
        let plan = node.prepare_automatic_mining(1);

        assert!(plan.pow_mined.is_none());
        assert!(
            plan.skipped_reason
                .as_deref()
                .unwrap_or_default()
                .contains("fee exceeds reward")
        );
        assert!(node.status().mining.pow_mining_enabled);
    }

    #[test]
    fn fee_rate_transfer_and_burn_pay_at_least_bytes_times_rate() {
        let alice = Wallet::from_seed("fee-rate-alice");
        let bob = Wallet::from_seed("fee-rate-bob");
        let mut genesis = BTreeMap::new();
        genesis.insert(alice.address().to_string(), 10 * MICRO_LUUN);
        let ledger = crate::domain::Ledger::new(genesis, 1);
        let mut node = NodeCore::from_ledger(alice, ledger, 0);

        let (transfer, _) = node
            .transfer_with_fee_rate(bob.address(), MICRO_LUUN, 2, &[])
            .unwrap();
        let minimum_transfer_fee = transfer.serialized_size_bytes().unwrap() as u64 * 2;
        assert!(transfer.fee() >= minimum_transfer_fee);

        let (burn, _) = node.burn_with_fee_rate(MICRO_LUUN, 3).unwrap();
        let minimum_burn_fee = burn.serialized_size_bytes().unwrap() as u64 * 3;
        assert!(burn.fee() >= minimum_burn_fee);
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
                        node.receive(envelope.clone())?;
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
