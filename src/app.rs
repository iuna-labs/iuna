use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::domain::{
    Amount, Block, ChainSnapshot, ChainStatus, Ledger, PreparedBlock, Transaction,
    VDF_TARGET_BLOCK_MS, Wallet, run_vdf,
};

pub type SharedNode = Arc<Mutex<NodeCore>>;
pub type SharedPeerBook = Arc<Mutex<PeerBook>>;

pub const DEFAULT_BURN_PER_BLOCK: Amount = 0;
pub const DEFAULT_VDF_ROUNDS: u32 = 67_000_000;
pub const PROTOCOL_VERSION: u32 = 1;
pub const NETWORK_ID: &str = "mivora-devnet-v0";
pub const BLOCK_REQUEST_LIMIT: usize = 128;
const IMPORT_REBROADCAST_LIMIT: usize = 128;

#[derive(Clone, Debug)]
pub struct NodeConfig {
    pub wallet: Wallet,
    pub genesis_allocations: BTreeMap<String, Amount>,
    pub vdf_rounds: u32,
    pub burn_per_block: Amount,
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
pub struct NodeStatus {
    pub wallet_address: String,
    pub wallet_balance: Amount,
    pub launch_profile: LaunchProfileStatus,
    pub mining: MiningStatus,
    pub chain: ChainStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LaunchProfileStatus {
    pub profile_id: String,
    pub profile_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MiningStatus {
    pub automatic: bool,
    pub burn_per_block: Amount,
    pub vdf_rounds: u32,
    pub vdf_target_block_ms: u64,
    pub current_leader: Option<String>,
    pub wallet_is_current_leader: bool,
    pub last_auto_burn_height: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutoMineOutcome {
    pub burned: Option<Transaction>,
    pub block: Option<Block>,
    pub skipped_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AutoMinePlan {
    pub burned: Option<Transaction>,
    pub work: Option<PreparedBlock>,
    pub skipped_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NodeCore {
    wallet: Wallet,
    ledger: Ledger,
    burn_per_block: Amount,
    last_auto_burn_height: Option<u64>,
    outbox: Vec<GossipEnvelope>,
}

impl NodeCore {
    pub fn new(config: NodeConfig) -> Self {
        let ledger = Ledger::new(config.genesis_allocations, config.vdf_rounds);
        Self::from_ledger(config.wallet, ledger, config.burn_per_block)
    }

    pub fn from_ledger(wallet: Wallet, ledger: Ledger, burn_per_block: Amount) -> Self {
        Self {
            wallet,
            ledger,
            burn_per_block,
            last_auto_burn_height: None,
            outbox: Vec::new(),
        }
    }

    pub fn wallet_address(&self) -> &str {
        self.wallet.address()
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
        let txs = self
            .ledger
            .pending()
            .iter()
            .map(|tx| tx.signature().to_string())
            .collect::<Vec<_>>();
        if txs.is_empty() {
            Vec::new()
        } else {
            vec![GossipEnvelope::Inventory {
                txs,
                blocks: Vec::new(),
            }]
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
            launch_profile: LaunchProfileStatus {
                profile_id: launch_profile.profile_id.clone(),
                profile_hash: chain.launch_profile_hash.clone(),
            },
            mining: MiningStatus {
                automatic: true,
                burn_per_block: self.burn_per_block,
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
        let was_disabled = self.burn_per_block == 0;
        self.burn_per_block = amount;
        if was_disabled && amount > 0 {
            self.last_auto_burn_height = None;
        }
        self.prepare_automatic_burn()
    }

    pub fn burn(&mut self, amount: Amount) -> Result<Transaction> {
        let tx = self
            .wallet
            .burn(amount, self.ledger.next_nonce(self.wallet.address()));
        if self.ledger.submit_transaction(tx.clone())? {
            self.outbox.push(GossipEnvelope::Transaction(tx.clone()));
        }
        Ok(tx)
    }

    pub fn transfer(&mut self, to: impl Into<String>, amount: Amount) -> Result<Transaction> {
        let tx = self
            .wallet
            .transfer(to, amount, self.ledger.next_nonce(self.wallet.address()));
        if self.ledger.submit_transaction(tx.clone())? {
            self.outbox.push(GossipEnvelope::Transaction(tx.clone()));
        }
        Ok(tx)
    }

    pub fn mine_one(&mut self) -> Result<Block> {
        self.mine_one_at(now_ms())
    }

    pub fn automatic_mine_once(&mut self, timestamp_ms: u64) -> AutoMineOutcome {
        let plan = self.prepare_automatic_mining(timestamp_ms);
        let mut outcome = AutoMineOutcome {
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
            burned: None,
            work: None,
            skipped_reason: None,
        };

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

    fn prepare_automatic_burn(&mut self) -> Result<Option<Transaction>> {
        let current_height = self.ledger.status().height;
        if self.burn_per_block == 0 {
            self.last_auto_burn_height = Some(current_height);
            return Ok(None);
        }
        if self.last_auto_burn_height == Some(current_height) {
            return Ok(None);
        }

        let tx = self.burn(self.burn_per_block)?;
        self.last_auto_burn_height = Some(current_height);
        Ok(Some(tx))
    }

    pub fn mine_one_at(&mut self, timestamp_ms: u64) -> Result<Block> {
        let block = self.ledger.mine_next_block(&self.wallet, timestamp_ms)?;
        self.ledger.apply_locally_mined_block(block.clone())?;
        self.outbox.push(GossipEnvelope::Block(block.clone()));
        Ok(block)
    }

    pub fn complete_prepared_block(
        &mut self,
        work: PreparedBlock,
        vdf_output: String,
    ) -> Result<Block> {
        let block = work.finish(&self.wallet, vdf_output);
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
            | GossipEnvelope::Inventory { .. } => Ok(()),
            GossipEnvelope::Transaction(tx) => {
                if self.ledger.submit_transaction(tx.clone())? {
                    self.outbox.push(GossipEnvelope::Transaction(tx));
                }
                Ok(())
            }
            GossipEnvelope::Transactions { transactions } => {
                for tx in transactions {
                    if self.ledger.submit_transaction(tx.clone())? {
                        self.outbox.push(GossipEnvelope::Transaction(tx));
                    }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::domain::Wallet;

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
