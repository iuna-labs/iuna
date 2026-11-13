//! Application ports for the IUNA consensus domain.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::atomic::{AtomicBool, AtomicU64},
};

use anyhow::{Context, Result, bail};

use crate::consensus::{
    Address, CandidateHeader, Chain, ConsensusState, Hash, HeartbeatBlock,
    MAX_HEARTBEAT_BLOCK_BYTES, MAX_LEDGER_SIDECAR_BYTES, OutPoint, Output, PROTOCOL_VERSION,
    PublicKey, Sidecar, TicketTransaction, Transaction, Winner, address_from_public_key,
    inclusion_winners, merkle_root, mine_child, mine_child_until, mine_child_until_counted,
    mine_child_with_workers, tracked_ticket_capacity, validate_pending_candidate,
    validate_pending_ticket, validate_pending_tickets, validate_pending_transaction,
    validate_pending_transactions,
};

const MAX_PENDING_LEDGER_TRANSACTIONS: usize = 10_000;
/// Mempool relay policy, not consensus: one unit per started 1,000 canonical bytes, so a typical
/// transaction needs at least 1 unit and free transactions cannot fill the mempool.
pub const MIN_RELAY_FEE_PER_KB: u64 = 1;
const MAX_PENDING_TICKETS: usize = 4_096;
const MAX_PENDING_CANDIDATES: usize = 256;
// Header encoding is currently below 256 bytes. Reserving twice that amount keeps mempool policy
// independent of future non-consensus header fields while `mine_child` remains the exact check.
const HEARTBEAT_TEMPLATE_OVERHEAD_BYTES: usize = 512;

/// Persistence port. The application owns commit/rollback ordering; SQLite remains an adapter.
pub trait ChainRepository {
    fn load(&self) -> Result<Option<Chain>>;
    fn save(&self, chain: &Chain) -> Result<()>;

    /// The chain to start from when `load` finds none.
    fn new_chain(&self) -> Result<Chain> {
        Chain::from_genesis().context("creating chain")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainStatus {
    pub height: u64,
    pub tip: [u8; 32],
    pub ledger_blocks: u64,
    pub minted: u64,
    pub tracked_tickets: usize,
    pub utxos: usize,
}

/// Small hexagonal application core around the pure consensus state machine.
pub struct Node<R> {
    repository: R,
    chain: Chain,
    pending_transactions: BTreeMap<Hash, Transaction>,
    pending_tickets: BTreeMap<Hash, TicketTransaction>,
    pending_candidates: BTreeMap<(Hash, u64, Hash), Sidecar>,
    reserved_inputs: BTreeSet<OutPoint>,
    owner_index: OwnerIndex,
    used_address_index: UsedAddressIndex,
    /// Bumped on every mempool change, so callers can persist it only when it changed.
    mempool_generation: u64,
}

/// Outputs per owner at one tip. Rebuilt lazily once per new tip, so public wallet lookups cost
/// O(outputs of that key) instead of a scan of the whole UTXO set under the node lock.
#[derive(Default)]
struct OwnerIndex {
    tip: Option<Hash>,
    outputs: HashMap<Address, Vec<OutPoint>>,
}

#[derive(Default)]
struct UsedAddressIndex {
    tip: Option<Hash>,
    chain_addresses: HashSet<Address>,
    /// Rebuilt separately so mempool churn never forces another walk of the whole chain.
    mempool_generation: Option<u64>,
    mempool_addresses: HashSet<Address>,
}

pub fn minimum_relay_fee(canonical_size: usize) -> u64 {
    (canonical_size as u64).div_ceil(1_000) * MIN_RELAY_FEE_PER_KB
}

/// Immutable work package. Hashing can run outside the node lock; submission revalidates the
/// finished block against the indexed parent and current fork choice.
pub struct PreparedHeartbeat {
    parent: ConsensusState,
    tickets: Vec<TicketTransaction>,
    sidecar: Option<Sidecar>,
}

impl PreparedHeartbeat {
    /// The earliest valid header timestamp at or after `now`: consensus requires it to exceed
    /// the parent's median time past.
    pub fn timestamp(&self, now: u64) -> Result<u64> {
        let earliest = self
            .parent
            .median_time_past()
            .context("reading median time past")?
            .checked_add(1)
            .context("Heartbeat timestamp overflow")?;
        Ok(now.max(earliest))
    }

    /// Search for a nonce until one is found (`Some`) or `stop` is set (`None`).
    pub fn mine_until(
        self,
        timestamp: u64,
        miner_public_key: PublicKey,
        workers: usize,
        stop: &AtomicBool,
    ) -> Result<Option<HeartbeatBlock>> {
        mine_child_until(
            &self.parent,
            timestamp,
            miner_public_key,
            self.tickets,
            self.sidecar,
            workers,
            stop,
        )
        .map(|mined| mined.map(|(block, _)| block))
        .context("mining prepared Heartbeat")
    }

    /// Search for a nonce while exposing the actual attempted-hash count to the local operator.
    pub fn mine_until_counted(
        self,
        timestamp: u64,
        miner_public_key: PublicKey,
        workers: usize,
        stop: &AtomicBool,
        hashes: &AtomicU64,
    ) -> Result<Option<HeartbeatBlock>> {
        mine_child_until_counted(
            &self.parent,
            timestamp,
            miner_public_key,
            self.tickets,
            self.sidecar,
            workers,
            stop,
            hashes,
        )
        .map(|mined| mined.map(|(block, _)| block))
        .context("mining prepared Heartbeat")
    }

    pub fn mine(self, timestamp: u64, miner_public_key: PublicKey) -> Result<HeartbeatBlock> {
        mine_child(
            &self.parent,
            timestamp,
            miner_public_key,
            self.tickets,
            self.sidecar,
        )
        .map(|(block, _)| block)
        .context("mining prepared Heartbeat")
    }

    pub fn mine_with_workers(
        self,
        timestamp: u64,
        miner_public_key: PublicKey,
        workers: usize,
    ) -> Result<HeartbeatBlock> {
        mine_child_with_workers(
            &self.parent,
            timestamp,
            miner_public_key,
            self.tickets,
            self.sidecar,
            workers,
        )
        .map(|(block, _)| block)
        .context("mining prepared Heartbeat")
    }
}

impl<R: ChainRepository> Node<R> {
    pub fn open(repository: R) -> Result<Self> {
        let chain = match repository.load().context("loading chain")? {
            Some(chain) => chain,
            None => repository.new_chain()?,
        };
        Ok(Self {
            repository,
            chain,
            pending_transactions: BTreeMap::new(),
            pending_tickets: BTreeMap::new(),
            pending_candidates: BTreeMap::new(),
            reserved_inputs: BTreeSet::new(),
            owner_index: OwnerIndex::default(),
            used_address_index: UsedAddressIndex::default(),
            mempool_generation: 0,
        })
    }

    pub fn chain(&self) -> &Chain {
        &self.chain
    }

    pub fn status(&self) -> ChainStatus {
        status_from(self.chain.tip())
    }

    pub fn pending_transaction_count(&self) -> usize {
        self.pending_transactions.len()
    }

    pub fn pending_ticket_count(&self) -> usize {
        self.pending_tickets.len()
    }

    pub fn pending_transactions(&self) -> Vec<Transaction> {
        self.pending_transactions.values().cloned().collect()
    }

    pub fn pending_tickets(&self) -> Vec<TicketTransaction> {
        self.pending_tickets.values().cloned().collect()
    }

    pub fn pending_candidates(&self) -> Vec<Sidecar> {
        self.pending_candidates.values().cloned().collect()
    }

    /// Up to `limit` pending transactions and tickets, without cloning the whole mempool.
    pub fn mempool_preview(&self, limit: usize) -> (Vec<&Transaction>, Vec<&TicketTransaction>) {
        (
            self.pending_transactions.values().take(limit).collect(),
            self.pending_tickets.values().take(limit).collect(),
        )
    }

    pub fn reserved_inputs(&self) -> BTreeSet<OutPoint> {
        self.reserved_inputs.clone()
    }

    pub fn is_input_reserved(&self, point: &OutPoint) -> bool {
        self.reserved_inputs.contains(point)
    }

    /// Confirmed outputs owned by `owner` at the current tip, in outpoint order.
    pub fn owned_outputs(&mut self, owner: &Address) -> Vec<(OutPoint, Output)> {
        let tip = self.chain.tip();
        if self.owner_index.tip != Some(tip.tip) {
            let mut outputs: HashMap<Address, Vec<OutPoint>> = HashMap::new();
            for (point, output) in &tip.utxos {
                outputs.entry(output.owner).or_default().push(point.clone());
            }
            self.owner_index = OwnerIndex {
                tip: Some(tip.tip),
                outputs,
            };
        }
        self.owner_index
            .outputs
            .get(owner)
            .into_iter()
            .flatten()
            .filter_map(|point| {
                tip.utxos
                    .get(point)
                    .map(|output| (point.clone(), output.clone()))
            })
            .collect()
    }

    /// Whether an address has appeared on the active chain or in the local mempool. Unlike the
    /// UTXO index this remains true after its outputs are spent, which lets a recovered
    /// deterministic wallet discover the next child address safely.
    pub fn address_used(&mut self, address: &Address) -> bool {
        let tip = self.chain.tip().tip;
        if self.used_address_index.tip != Some(tip) {
            let mut addresses = HashSet::new();
            let mut cursor = Some(tip);
            while let Some(hash) = cursor {
                let Some(block) = self.chain.block(&hash) else {
                    break;
                };
                addresses.insert(address_from_public_key(&block.header.miner_public_key));
                for ticket in &block.tickets {
                    addresses.insert(address_from_public_key(&ticket.mining_public_key));
                    addresses.extend(ticket.change.iter().map(|output| output.owner));
                }
                for transaction in block
                    .sidecar
                    .iter()
                    .flat_map(|sidecar| sidecar.transactions.iter())
                {
                    addresses.extend(transaction.outputs.iter().map(|output| output.owner));
                }
                cursor = block.header.parent;
            }
            self.used_address_index.tip = Some(tip);
            self.used_address_index.chain_addresses = addresses;
        }
        if self.used_address_index.mempool_generation != Some(self.mempool_generation) {
            let mut addresses = HashSet::new();
            for transaction in self.pending_transactions.values() {
                addresses.extend(transaction.outputs.iter().map(|output| output.owner));
            }
            for ticket in self.pending_tickets.values() {
                addresses.extend(ticket.change.iter().map(|output| output.owner));
            }
            self.used_address_index.mempool_generation = Some(self.mempool_generation);
            self.used_address_index.mempool_addresses = addresses;
        }
        self.used_address_index.chain_addresses.contains(address)
            || self.used_address_index.mempool_addresses.contains(address)
    }

    pub fn has_pending_transaction(&self, id: &Hash) -> bool {
        self.pending_transactions.contains_key(id)
    }

    pub fn pending_transaction(&self, id: &Hash) -> Option<&Transaction> {
        self.pending_transactions.get(id)
    }

    pub fn has_pending_ticket(&self, id: &Hash) -> bool {
        self.pending_tickets.contains_key(id)
    }

    pub fn pending_ticket(&self, id: &Hash) -> Option<&TicketTransaction> {
        self.pending_tickets.get(id)
    }

    pub fn has_pending_candidate(&self, hash: &Hash) -> bool {
        self.pending_candidates
            .values()
            .any(|candidate| candidate.header.hash() == *hash)
    }

    pub fn submit_transaction(&mut self, transaction: Transaction) -> Result<u64> {
        if self.pending_transactions.len() >= MAX_PENDING_LEDGER_TRANSACTIONS {
            bail!("Ledger transaction mempool is full");
        }
        let id = transaction.id();
        if self.pending_transactions.contains_key(&id) {
            bail!("Ledger transaction is already pending");
        }
        let inputs = transaction
            .inputs
            .iter()
            .map(|input| input.previous_output.clone())
            .collect::<Vec<_>>();
        self.ensure_inputs_available(&inputs)?;
        let fee = validate_pending_transaction(self.chain.tip(), &transaction)
            .context("validating pending Ledger transaction")?;
        let minimum_fee = minimum_relay_fee(transaction.canonical_size());
        if fee < minimum_fee {
            bail!("Ledger transaction fee is below the relay minimum of {minimum_fee} units");
        }
        self.reserved_inputs.extend(inputs);
        self.pending_transactions.insert(id, transaction);
        self.mempool_generation += 1;
        Ok(fee)
    }

    pub fn submit_ticket(&mut self, ticket: TicketTransaction) -> Result<u64> {
        if self.pending_tickets.len() >= MAX_PENDING_TICKETS {
            bail!("ticket mempool is full");
        }
        let id = ticket.id();
        if self.pending_tickets.contains_key(&id) {
            bail!("mining ticket is already pending");
        }
        let inputs = ticket
            .inputs
            .iter()
            .map(|input| input.previous_output.clone())
            .collect::<Vec<_>>();
        self.ensure_inputs_available(&inputs)?;
        let fee = validate_pending_ticket(self.chain.tip(), &ticket)
            .context("validating pending mining ticket")?;
        self.reserved_inputs.extend(inputs);
        self.pending_tickets.insert(id, ticket);
        self.mempool_generation += 1;
        Ok(fee)
    }

    /// Keeps the best signed candidate seen for each winning ticket slot. Higher total fees win;
    /// the candidate hash is the deterministic tie-break. This bounds equivocation without making
    /// off-chain arrival order part of consensus.
    pub fn submit_candidate(&mut self, candidate: Sidecar) -> Result<bool> {
        let fee = validate_pending_candidate(self.chain.tip(), &candidate)
            .context("validating pending signed Ledger candidate")?;
        let key = (
            candidate.header.ticket_id,
            candidate.header.slot,
            candidate.header.parent_anchor,
        );
        let hash = candidate.header.hash();
        if let Some(existing) = self.pending_candidates.get(&key) {
            let existing_fee = validate_pending_candidate(self.chain.tip(), existing)
                .context("revalidating pending signed Ledger candidate")?;
            let existing_hash = existing.header.hash();
            if fee < existing_fee || (fee == existing_fee && hash >= existing_hash) {
                return Ok(false);
            }
        } else if self.pending_candidates.len() >= MAX_PENDING_CANDIDATES {
            bail!("signed Ledger candidate pool is full");
        }
        self.pending_candidates.insert(key, candidate);
        self.mempool_generation += 1;
        Ok(true)
    }

    /// Index a valid block and durably publish a new active tip. A persistence failure removes
    /// that one submission again, so callers never observe an unpersisted tip.
    pub fn submit_block(&mut self, block: HeartbeatBlock, now: u64) -> Result<bool> {
        let previous_tip = self.chain.tip().tip;
        let block_hash = block.hash();
        let activated = self.chain.submit(block, now).context("validating block")?;
        if activated && let Err(error) = self.repository.save(&self.chain) {
            self.chain.rollback_submission(block_hash, previous_tip);
            return Err(error).context("persisting activated chain");
        }
        if activated {
            let (transactions, tickets) = self.disconnected_objects(&previous_tip);
            self.reconcile_mempool();
            // What the new branch does not contain is valid again; the rest is refused here.
            self.restore_mempool(transactions, tickets, Vec::new());
        }
        Ok(activated)
    }

    pub fn mine_block(
        &mut self,
        timestamp: u64,
        miner_public_key: PublicKey,
        tickets: Vec<TicketTransaction>,
        sidecar: Option<Sidecar>,
    ) -> Result<HeartbeatBlock> {
        let (block, _) = mine_child(
            self.chain.tip(),
            timestamp,
            miner_public_key,
            tickets,
            sidecar,
        )
        .context("building block")?;
        if !self.submit_block(block.clone(), timestamp)? {
            anyhow::bail!("locally mined block did not add chainwork");
        }
        Ok(block)
    }

    /// Mine the next Heartbeat using pending tickets, the best signed Ledger candidate, or the
    /// deterministic empty deadline fallback.
    pub fn mine_next(
        &mut self,
        timestamp: u64,
        miner_public_key: PublicKey,
    ) -> Result<HeartbeatBlock> {
        let block = self.prepare_next()?.mine(timestamp, miner_public_key)?;
        if !self.submit_block(block.clone(), timestamp)? {
            anyhow::bail!("locally mined block did not add chainwork");
        }
        Ok(block)
    }

    pub fn prepare_next(&self) -> Result<PreparedHeartbeat> {
        let next_height = self
            .chain
            .tip()
            .height
            .checked_add(1)
            .context("Heartbeat height overflow")?;
        let winners =
            inclusion_winners(self.chain.tip(), next_height).context("selecting Ledger winner")?;
        let sidecar = match winners.required {
            Some(winner) => Some(fallback_sidecar(&winner)),
            None => self
                .pending_candidates
                .values()
                .filter_map(|candidate| {
                    validate_pending_candidate(self.chain.tip(), candidate)
                        .ok()
                        .map(|fee| (fee, candidate.header.hash(), candidate))
                })
                .max_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)))
                .map(|(_, _, candidate)| candidate.clone()),
        };
        let tickets = self.select_tickets();
        Ok(PreparedHeartbeat {
            parent: self.chain.tip().clone(),
            tickets,
            sidecar,
        })
    }

    /// Builds unsigned drafts for every next-block winner controlled by `mining_public_key`.
    /// The wallet signs and submits them; Heartbeat miners never receive the private key.
    pub fn candidate_drafts(&self, mining_public_key: PublicKey) -> Result<Vec<Sidecar>> {
        let next_height = self
            .chain
            .tip()
            .height
            .checked_add(1)
            .context("Heartbeat height overflow")?;
        let winners = inclusion_winners(self.chain.tip(), next_height)
            .context("selecting signed Ledger candidate winner")?;
        if winners.required.is_some() {
            return Ok(Vec::new());
        }
        let transactions = self.select_ledger_transactions();
        if transactions.is_empty() {
            return Ok(Vec::new());
        }
        let transaction_ids = transactions.iter().map(Transaction::id).collect::<Vec<_>>();
        let transactions_root = merkle_root(&transaction_ids);
        Ok(winners
            .available
            .into_iter()
            .filter(|winner| winner.mining_public_key == mining_public_key)
            .map(|winner| Sidecar {
                header: CandidateHeader {
                    version: PROTOCOL_VERSION,
                    lottery_heartbeat: winner.heartbeat,
                    lottery_height: winner.heartbeat_height,
                    parent_anchor: winner.parent_anchor,
                    ticket_id: winner.ticket_id,
                    slot: winner.slot,
                    lottery_output: winner.output,
                    transactions_root,
                    signature: Vec::new(),
                },
                transactions: transactions.clone(),
            })
            .collect())
    }

    fn select_ledger_transactions(&self) -> Vec<Transaction> {
        let mut candidates = self
            .pending_transactions
            .iter()
            .filter_map(|(id, transaction)| {
                validate_pending_transaction(self.chain.tip(), transaction)
                    .ok()
                    .map(|fee| (fee, *id, transaction))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

        let empty_sidecar = Sidecar {
            header: CandidateHeader {
                version: PROTOCOL_VERSION,
                lottery_heartbeat: [0; 32],
                lottery_height: 0,
                parent_anchor: [0; 32],
                ticket_id: [0; 32],
                slot: 0,
                lottery_output: [0; 32],
                transactions_root: merkle_root(&[]),
                signature: Vec::new(),
            },
            transactions: Vec::new(),
        };
        let mut encoded_size = empty_sidecar.canonical_size();
        let mut selected = Vec::new();
        for (_, _, transaction) in candidates {
            let transaction_size = std::mem::size_of::<u32>() + transaction.canonical_size();
            if encoded_size + transaction_size > MAX_LEDGER_SIDECAR_BYTES {
                continue;
            }
            if select_canonical_if_valid(
                &mut selected,
                transaction.clone(),
                Transaction::id,
                |candidate| validate_pending_transactions(self.chain.tip(), candidate).is_ok(),
            ) {
                encoded_size += transaction_size;
            }
        }
        selected
    }

    fn select_tickets(&self) -> Vec<TicketTransaction> {
        let mut candidates = self
            .pending_tickets
            .iter()
            .filter_map(|(id, ticket)| {
                validate_pending_ticket(self.chain.tip(), ticket)
                    .ok()
                    .map(|fee| (fee, *id, ticket))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

        // Excess tickets stay pending: a template over the tracked cap could never be mined.
        let capacity = tracked_ticket_capacity(self.chain.tip()).unwrap_or(0);
        let payload_limit = MAX_HEARTBEAT_BLOCK_BYTES - HEARTBEAT_TEMPLATE_OVERHEAD_BYTES;
        let mut payload_size = std::mem::size_of::<u32>();
        let mut selected = Vec::new();
        for (_, _, ticket) in candidates {
            if selected.len() >= capacity {
                break;
            }
            let ticket_size = std::mem::size_of::<u32>() + ticket.canonical_size();
            if payload_size + ticket_size > payload_limit {
                continue;
            }
            if select_canonical_if_valid(
                &mut selected,
                ticket.clone(),
                TicketTransaction::id,
                |candidate| validate_pending_tickets(self.chain.tip(), candidate).is_ok(),
            ) {
                payload_size += ticket_size;
            }
        }
        selected
    }

    fn ensure_inputs_available(&self, inputs: &[OutPoint]) -> Result<()> {
        let mut unique = BTreeSet::new();
        for input in inputs {
            if !unique.insert(input) {
                bail!("object spends the same input more than once");
            }
            if self.reserved_inputs.contains(input) {
                bail!("input is already reserved by the mempool");
            }
        }
        Ok(())
    }

    pub fn mempool_generation(&self) -> u64 {
        self.mempool_generation
    }

    /// Re-submits saved mempool entries through the normal validation path. Entries that were
    /// mined or became invalid while the node was stopped are dropped; returns how many remain.
    pub fn restore_mempool(
        &mut self,
        transactions: Vec<Transaction>,
        tickets: Vec<TicketTransaction>,
        candidates: Vec<Sidecar>,
    ) -> usize {
        let mut restored = 0;
        for ticket in tickets {
            restored += usize::from(self.submit_ticket(ticket).is_ok());
        }
        for transaction in transactions {
            restored += usize::from(self.submit_transaction(transaction).is_ok());
        }
        for candidate in candidates {
            restored += usize::from(self.submit_candidate(candidate).unwrap_or(false));
        }
        restored
    }

    fn disconnected_objects(
        &self,
        previous_tip: &Hash,
    ) -> (Vec<Transaction>, Vec<TicketTransaction>) {
        let disconnected = self.chain.blocks_off_active_chain(previous_tip);
        let transactions = disconnected
            .iter()
            .filter_map(|block| block.sidecar.as_ref())
            .flat_map(|sidecar| sidecar.transactions.iter().cloned())
            .collect();
        let tickets = disconnected
            .iter()
            .flat_map(|block| block.tickets.iter().cloned())
            .collect();
        (transactions, tickets)
    }

    fn reconcile_mempool(&mut self) {
        self.mempool_generation += 1;
        self.pending_transactions.retain(|_, transaction| {
            validate_pending_transaction(self.chain.tip(), transaction).is_ok()
        });
        self.pending_tickets
            .retain(|_, ticket| validate_pending_ticket(self.chain.tip(), ticket).is_ok());
        self.pending_candidates
            .retain(|_, candidate| validate_pending_candidate(self.chain.tip(), candidate).is_ok());
        self.reserved_inputs.clear();
        self.reserved_inputs.extend(
            self.pending_transactions
                .values()
                .flat_map(|transaction| transaction.inputs.iter())
                .map(|input| input.previous_output.clone()),
        );
        self.reserved_inputs.extend(
            self.pending_tickets
                .values()
                .flat_map(|ticket| ticket.inputs.iter())
                .map(|input| input.previous_output.clone()),
        );
    }
}

fn fallback_sidecar(winner: &Winner) -> Sidecar {
    Sidecar {
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
    }
}

/// Insert into canonical order only after the complete candidate set validates. In particular,
/// rejecting an item must not remove a different item after sorting.
fn select_canonical_if_valid<T: Clone>(
    selected: &mut Vec<T>,
    item: T,
    id: impl Fn(&T) -> Hash,
    valid: impl FnOnce(&[T]) -> bool,
) -> bool {
    let mut candidate = selected.clone();
    candidate.push(item);
    candidate.sort_by_key(id);
    if !valid(&candidate) {
        return false;
    }
    *selected = candidate;
    true
}

fn status_from(state: &ConsensusState) -> ChainStatus {
    ChainStatus {
        height: state.height,
        tip: state.tip,
        ledger_blocks: state.ledger_blocks,
        minted: state.minted,
        tracked_tickets: state.tickets.len(),
        utxos: state.utxos.len(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ed25519_dalek::{Signer, SigningKey};

    use super::*;
    use crate::consensus::{
        GENESIS_MINER, GENESIS_REWARD, GENESIS_TIMESTAMP, HEARTBEAT_TARGET_INTERVAL_SECS,
        INITIAL_LEDGER_SUBSIDY, Input, Output, TicketInput, minimum_ticket_budget,
    };

    const GENESIS_SECRET: [u8; 32] = [
        0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c,
        0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03, 0x1c, 0xae,
        0x7f, 0x60,
    ];

    #[derive(Clone, Default)]
    struct MemoryRepository {
        state: Arc<Mutex<Option<Chain>>>,
        fail_load: Arc<Mutex<bool>>,
        fail_new_chain: Arc<Mutex<bool>>,
        fail_save: Arc<Mutex<bool>>,
    }

    impl ChainRepository for MemoryRepository {
        fn load(&self) -> Result<Option<Chain>> {
            if *self.fail_load.lock().unwrap() {
                anyhow::bail!("injected load failure");
            }
            Ok(self.state.lock().unwrap().clone())
        }

        fn save(&self, chain: &Chain) -> Result<()> {
            if *self.fail_save.lock().unwrap() {
                anyhow::bail!("injected save failure");
            }
            *self.state.lock().unwrap() = Some(chain.clone());
            Ok(())
        }

        fn new_chain(&self) -> Result<Chain> {
            if *self.fail_new_chain.lock().unwrap() {
                anyhow::bail!("injected new-chain failure");
            }
            Chain::from_genesis().context("creating chain")
        }
    }

    fn signed_genesis_transaction(node: &Node<MemoryRepository>) -> Transaction {
        let signing_key = SigningKey::from_bytes(&GENESIS_SECRET);
        let mut transaction = Transaction {
            version: PROTOCOL_VERSION,
            inputs: vec![Input {
                previous_output: node.chain().tip().utxos.keys().next().unwrap().clone(),
                public_key: GENESIS_MINER,
                signature: Vec::new(),
            }],
            outputs: vec![Output {
                owner: [41; 32],
                value: GENESIS_REWARD - 1,
            }],
        };
        transaction.inputs[0].signature = signing_key
            .sign(&transaction.signing_digest(0, &[GENESIS_REWARD]).unwrap())
            .to_bytes()
            .to_vec();
        transaction
    }

    fn signed_genesis_ticket(node: &Node<MemoryRepository>) -> TicketTransaction {
        let signing_key = SigningKey::from_bytes(&GENESIS_SECRET);
        let budget = minimum_ticket_budget(1, INITIAL_LEDGER_SUBSIDY).unwrap();
        let mut ticket = TicketTransaction {
            version: PROTOCOL_VERSION,
            inputs: vec![TicketInput {
                previous_output: node.chain().tip().utxos.keys().next().unwrap().clone(),
                public_key: GENESIS_MINER,
                signature: Vec::new(),
            }],
            budget,
            duration: 1,
            mining_public_key: [42; 32],
            lottery_seed: [43; 32],
            change: Some(Output {
                owner: crate::consensus::address_from_public_key(&GENESIS_MINER),
                value: GENESIS_REWARD - budget - 1,
            }),
        };
        ticket.inputs[0].signature = signing_key
            .sign(&ticket.signing_digest(0, &[GENESIS_REWARD]).unwrap())
            .to_bytes()
            .to_vec();
        ticket
    }

    #[test]
    fn open_mine_and_resume_use_the_repository_port() {
        let repository = MemoryRepository::default();
        let mut node = Node::open(repository.clone()).unwrap();
        node.mine_block(
            GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS,
            [31; 32],
            Vec::new(),
            None,
        )
        .unwrap();
        let resumed = Node::open(repository).unwrap();
        assert_eq!(resumed.status(), node.status());
        assert_eq!(resumed.status().height, 1);
    }

    #[test]
    fn repository_open_failures_are_propagated() {
        let repository = MemoryRepository::default();
        *repository.fail_load.lock().unwrap() = true;
        assert!(Node::open(repository).is_err());

        let repository = MemoryRepository::default();
        *repository.fail_new_chain.lock().unwrap() = true;
        assert!(Node::open(repository).is_err());
    }

    #[test]
    fn persistence_failure_rolls_back_the_in_memory_tip() {
        let repository = MemoryRepository::default();
        let mut node = Node::open(repository.clone()).unwrap();
        *repository.fail_save.lock().unwrap() = true;
        let before = node.status();
        let block = node
            .prepare_next()
            .unwrap()
            .mine(GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS, [32; 32])
            .unwrap();
        let block_hash = block.hash();
        assert!(
            node.submit_block(block, GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS)
                .is_err()
        );
        assert_eq!(node.status(), before);
        assert!(node.chain().block(&block_hash).is_none());
        assert_eq!(Node::open(repository).unwrap().status(), before);
    }

    #[test]
    fn address_usage_cache_is_invalidated_when_a_tip_drops_a_mempool_object() {
        let mut node = Node::open(MemoryRepository::default()).unwrap();
        let transaction = signed_genesis_transaction(&node);
        let recipient = transaction.outputs[0].owner;
        node.submit_transaction(transaction).unwrap();
        assert!(node.address_used(&recipient));

        let mut competing = Node::open(MemoryRepository::default()).unwrap();
        let ticket = signed_genesis_ticket(&competing);
        competing.submit_ticket(ticket).unwrap();
        let block = competing
            .mine_next(GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS, [44; 32])
            .unwrap();
        assert!(
            node.submit_block(block, GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS)
                .unwrap()
        );

        assert_eq!(node.pending_transaction_count(), 0);
        assert!(!node.address_used(&recipient));
    }

    #[test]
    fn relay_fee_rounds_up_at_each_kilobyte_boundary() {
        assert_eq!(minimum_relay_fee(0), 0);
        assert_eq!(minimum_relay_fee(1), 1);
        assert_eq!(minimum_relay_fee(1_000), 1);
        assert_eq!(minimum_relay_fee(1_001), 2);
    }

    #[test]
    fn mempool_reservations_prevent_cross_pool_double_spends() {
        let mut node = Node::open(MemoryRepository::default()).unwrap();
        let transaction = signed_genesis_transaction(&node);
        let ticket = signed_genesis_ticket(&node);

        assert_eq!(node.submit_transaction(transaction.clone()).unwrap(), 1);
        assert!(node.submit_transaction(transaction).is_err());
        assert!(node.submit_ticket(ticket).is_err());
        assert_eq!(node.pending_transaction_count(), 1);
        assert_eq!(node.pending_ticket_count(), 0);
    }

    #[test]
    fn mined_tickets_leave_the_mempool_after_activation() {
        let mut node = Node::open(MemoryRepository::default()).unwrap();
        let ticket = signed_genesis_ticket(&node);
        assert_eq!(node.submit_ticket(ticket).unwrap(), 1);

        let block = node
            .mine_next(GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS, [44; 32])
            .unwrap();

        assert_eq!(block.tickets.len(), 1);
        assert_eq!(node.pending_ticket_count(), 0);
        assert_eq!(node.status().height, 1);
    }

    #[test]
    fn objects_from_disconnected_blocks_return_to_the_mempool() {
        let mut node = Node::open(MemoryRepository::default()).unwrap();
        let ticket = signed_genesis_ticket(&node);
        node.submit_ticket(ticket.clone()).unwrap();
        let first = GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS;
        let mined = node.mine_next(first, [44; 32]).unwrap();
        assert_eq!(mined.tickets, std::slice::from_ref(&ticket));

        let mut other = Node::open(MemoryRepository::default()).unwrap();
        let heavier = [
            other.mine_next(first, [45; 32]).unwrap(),
            other
                .mine_next(first + HEARTBEAT_TARGET_INTERVAL_SECS, [45; 32])
                .unwrap(),
        ];
        let now = first + 2 * HEARTBEAT_TARGET_INTERVAL_SECS;
        assert_eq!(node.pending_ticket_count(), 0);
        for block in heavier {
            node.submit_block(block, now).unwrap();
        }

        assert_eq!(node.status().tip, other.status().tip);
        assert!(node.has_pending_ticket(&ticket.id()));
        assert_eq!(node.reserved_inputs().len(), 1);
    }

    #[test]
    fn a_saved_mempool_is_revalidated_when_restored() {
        let mut node = Node::open(MemoryRepository::default()).unwrap();
        let transaction = signed_genesis_transaction(&node);
        let ticket = signed_genesis_ticket(&node);
        let before = node.mempool_generation();
        node.submit_transaction(transaction.clone()).unwrap();
        assert!(node.mempool_generation() > before);

        // Restoring revalidates every entry: tickets go first, so the saved transaction that
        // spends the same genesis output conflicts and is dropped instead of restored.
        let mut restarted = Node::open(MemoryRepository::default()).unwrap();
        assert_eq!(
            restarted.restore_mempool(node.pending_transactions(), vec![ticket], Vec::new()),
            1
        );
        assert_eq!(restarted.pending_ticket_count(), 1);
        assert_eq!(restarted.pending_transaction_count(), 0);
        assert_eq!(
            restarted.restore_mempool(vec![transaction], Vec::new(), Vec::new()),
            0
        );
    }

    #[test]
    fn rejected_template_item_does_not_replace_an_earlier_selection() {
        let high_id = [0xff; 32];
        let low_id = [0x00; 32];
        let mut selected = vec![high_id];

        assert!(!select_canonical_if_valid(
            &mut selected,
            low_id,
            |id| *id,
            |candidate| candidate.len() <= 1,
        ));
        assert_eq!(selected, vec![high_id]);
    }
}
