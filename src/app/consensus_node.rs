//! Application ports for the IUNA consensus domain.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64},
    },
};

use anyhow::{Context, Result, bail};

use crate::consensus::{
    Address, CandidateHeader, Chain, ConsensusError, ConsensusState, Hash, HeartbeatBlock,
    InclusionWinners, MAX_HEARTBEAT_BLOCK_BYTES, MAX_LEDGER_SIDECAR_BYTES, OutPoint, Output,
    PROTOCOL_VERSION, PublicKey, Sidecar, TicketTransaction, Transaction, Winner,
    address_from_public_key, inclusion_winners, merkle_root, mine_child, mine_child_until,
    mine_child_until_counted, mine_child_with_workers, tracked_ticket_capacity,
    validate_pending_candidate_with, validate_pending_ticket, validate_pending_tickets,
    validate_pending_transaction, validate_pending_transactions,
};

const MAX_PENDING_LEDGER_TRANSACTIONS: usize = 10_000;
/// The pool's canonical bytes, 64 full Ledger blocks. Without it the count alone let a pool of
/// block-sized transactions reach about 5 GiB, which is cloned and saved under the node lock.
const MAX_PENDING_LEDGER_BYTES: usize = 64 * MAX_LEDGER_SIDECAR_BYTES;
/// Mempool relay policy, not consensus: one unit per started 1,000 canonical bytes, so a typical
/// transaction needs at least 1 unit and free transactions cannot fill the mempool.
pub const MIN_RELAY_FEE_PER_KB: u64 = 1;
const MAX_PENDING_TICKETS: usize = 4_096;
const MAX_PENDING_CANDIDATES: usize = 256;
// Header encoding is currently below 256 bytes. Reserving twice that amount keeps mempool policy
// independent of future non-consensus header fields while `mine_child` remains the exact check.
const HEARTBEAT_TEMPLATE_OVERHEAD_BYTES: usize = 512;
// The signed candidate header is also below 256 bytes; the same margin applies to Ledger blocks.
const LEDGER_TEMPLATE_OVERHEAD_BYTES: usize = 512;
/// Largest Ledger transaction a block template can still hold next to the candidate header, the
/// transaction count and its own length prefix. A larger one would wait in the mempool forever.
pub const MAX_LEDGER_TRANSACTION_BYTES: usize =
    MAX_LEDGER_SIDECAR_BYTES - LEDGER_TEMPLATE_OVERHEAD_BYTES - 2 * std::mem::size_of::<u32>();
/// Largest ticket a Heartbeat template can still hold next to the ticket count and its own
/// length prefix.
pub const MAX_TICKET_BYTES: usize =
    MAX_HEARTBEAT_BLOCK_BYTES - HEARTBEAT_TEMPLATE_OVERHEAD_BYTES - 2 * std::mem::size_of::<u32>();

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
    /// The fee and canonical size of each pending transaction and ticket, for evicting the
    /// cheapest per byte when a pool is full without encoding every entry again.
    pending_fees: BTreeMap<Hash, (u64, usize)>,
    /// Canonical bytes of all pending Ledger transactions, kept with `pending_transactions`.
    pending_transaction_bytes: usize,
    pending_candidates: BTreeMap<(Hash, u64, Hash), Sidecar>,
    reserved_inputs: BTreeSet<OutPoint>,
    owner_index: OwnerIndex,
    used_address_index: UsedAddressIndex,
    balance_ranking: BalanceRanking,
    /// Bumped on every mempool change, so callers can persist it only when it changed.
    mempool_generation: u64,
    canonical_index: Mutex<CanonicalIndex>,
    /// Winners that may fill the next block, which depend only on the tip. Computing them runs the
    /// lottery over every tracked ticket, so candidate checks share one result per tip.
    next_winners: Mutex<Option<(Hash, Arc<InclusionWinners>)>>,
}

/// Canonical block hashes by height, for peer sync requests. Without it every locator request
/// walked and hashed the whole chain under the node lock. It follows the tip incrementally: a new
/// tip only walks back to the first block the index already holds.
#[derive(Default)]
struct CanonicalIndex {
    tip: Option<Hash>,
    hashes: Arc<Vec<Hash>>,
}

/// Outputs per owner at one tip. Rebuilt lazily once per new tip, so public wallet lookups cost
/// O(outputs of that key) instead of a scan of the whole UTXO set under the node lock.
#[derive(Default)]
struct OwnerIndex {
    tip: Option<Hash>,
    outputs: HashMap<Address, Vec<OutPoint>>,
}

/// Most an address-balance ranking keeps. Public statistics pages show far fewer.
pub const MAX_RANKED_BALANCES: usize = 100;

/// Confirmed balance of one address at the current tip.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddressBalance {
    pub address: Address,
    pub amount: u64,
    pub outputs: usize,
}

/// The largest balances at one tip, rebuilt lazily once per new tip like `OwnerIndex`.
#[derive(Default)]
struct BalanceRanking {
    tip: Option<Hash>,
    holders: usize,
    top: Vec<AddressBalance>,
}

#[derive(Default)]
struct UsedAddressIndex {
    tip: Option<Hash>,
    chain_addresses: HashSet<Address>,
    /// Rebuilt separately so mempool churn never forces another walk of the whole chain.
    mempool_generation: Option<u64>,
    mempool_addresses: HashSet<Address>,
}

/// Every address a block shows as used: its miner, its tickets' keys and change, and the outputs
/// of its Ledger transactions.
fn insert_block_addresses(addresses: &mut HashSet<Address>, block: &HeartbeatBlock) {
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
}

/// The fee a transaction pays if its signatures hold: its inputs in `tip` minus its outputs. It is
/// cheap next to signature checks, so a transaction that could never pay the relay minimum is
/// refused before them. `None` means validation will refuse it anyway.
fn unverified_fee(tip: &ConsensusState, transaction: &Transaction) -> Option<u64> {
    let inputs = transaction.inputs.iter().try_fold(0u64, |total, input| {
        total.checked_add(tip.utxos.get(&input.previous_output)?.value)
    })?;
    let outputs = transaction
        .outputs
        .iter()
        .try_fold(0u64, |total, output| total.checked_add(output.value))?;
    inputs.checked_sub(outputs)
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
            pending_fees: BTreeMap::new(),
            pending_transaction_bytes: 0,
            pending_candidates: BTreeMap::new(),
            reserved_inputs: BTreeSet::new(),
            owner_index: OwnerIndex::default(),
            used_address_index: UsedAddressIndex::default(),
            balance_ranking: BalanceRanking::default(),
            mempool_generation: 0,
            canonical_index: Mutex::new(CanonicalIndex::default()),
            next_winners: Mutex::new(None),
        })
    }

    pub fn chain(&self) -> &Chain {
        &self.chain
    }

    fn next_winners(&self) -> std::result::Result<Arc<InclusionWinners>, ConsensusError> {
        let tip = self.chain.tip_hash();
        let mut cached = self
            .next_winners
            .lock()
            .expect("next winners lock poisoned");
        if let Some((cached_tip, winners)) = cached.as_ref()
            && *cached_tip == tip
        {
            return Ok(Arc::clone(winners));
        }
        let state = self.chain.tip();
        let height = state
            .height
            .checked_add(1)
            .ok_or(ConsensusError::Arithmetic)?;
        let winners = Arc::new(inclusion_winners(state, height)?);
        *cached = Some((tip, Arc::clone(&winners)));
        Ok(winners)
    }

    fn pending_candidate_fee(
        &self,
        candidate: &Sidecar,
    ) -> std::result::Result<u64, ConsensusError> {
        validate_pending_candidate_with(self.chain.tip(), candidate, || self.next_winners())
    }

    /// The hash of every canonical block, indexed by height, genesis first.
    pub fn canonical_hashes(&self) -> Arc<Vec<Hash>> {
        let tip = self.chain.tip_hash();
        let mut index = self
            .canonical_index
            .lock()
            .expect("canonical index lock poisoned");
        if index.tip == Some(tip) {
            return Arc::clone(&index.hashes);
        }
        // Forget the tip first: an index left empty by a panic below must not pass for current.
        index.tip = None;
        let mut hashes = std::mem::take(&mut index.hashes);
        let hashes_mut = Arc::make_mut(&mut hashes);
        let mut suffix = Vec::new();
        let mut cursor = tip;
        loop {
            let block = self
                .chain
                .block(&cursor)
                .expect("canonical block is indexed");
            let height = usize::try_from(block.header.height).expect("height fits in memory");
            if hashes_mut.get(height) == Some(&cursor) {
                hashes_mut.truncate(height + 1);
                break;
            }
            suffix.push(cursor);
            match block.header.parent {
                Some(parent) => cursor = parent,
                None => {
                    hashes_mut.clear();
                    break;
                }
            }
        }
        hashes_mut.extend(suffix.into_iter().rev());
        index.tip = Some(tip);
        index.hashes = Arc::clone(&hashes);
        hashes
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

    /// The owners of every output that pending transactions and tickets would create.
    pub fn pending_output_owners(&self) -> HashSet<Address> {
        let transactions = self
            .pending_transactions
            .values()
            .flat_map(|transaction| transaction.outputs.iter());
        let tickets = self
            .pending_tickets
            .values()
            .flat_map(|ticket| ticket.change.iter());
        transactions
            .chain(tickets)
            .map(|output| output.owner)
            .collect()
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

    /// The number of addresses holding confirmed outputs, and the largest balances among them in
    /// descending order (ties by address), at most `MAX_RANKED_BALANCES`.
    pub fn top_balances(&mut self) -> (usize, &[AddressBalance]) {
        let tip = self.chain.tip();
        if self.balance_ranking.tip != Some(tip.tip) {
            let mut balances: HashMap<Address, (u64, usize)> = HashMap::new();
            for output in tip.utxos.values() {
                let entry = balances.entry(output.owner).or_default();
                // The supply invariant bounds every sum by MAX_MONEY.
                entry.0 = entry.0.saturating_add(output.value);
                entry.1 += 1;
            }
            let holders = balances.len();
            let mut top: Vec<AddressBalance> = balances
                .into_iter()
                .map(|(address, (amount, outputs))| AddressBalance {
                    address,
                    amount,
                    outputs,
                })
                .collect();
            top.sort_unstable_by(|left, right| {
                right
                    .amount
                    .cmp(&left.amount)
                    .then_with(|| left.address.cmp(&right.address))
            });
            top.truncate(MAX_RANKED_BALANCES);
            self.balance_ranking = BalanceRanking {
                tip: Some(tip.tip),
                holders,
                top,
            };
        }
        (self.balance_ranking.holders, &self.balance_ranking.top)
    }

    /// Whether an address has appeared on the active chain or in the local mempool. Unlike the
    /// UTXO index this remains true after its outputs are spent, which lets a recovered
    /// deterministic wallet discover the next child address safely.
    pub fn address_used(&mut self, address: &Address) -> bool {
        let tip = self.chain.tip().tip;
        if self.used_address_index.tip != Some(tip) {
            // A tip that extends the indexed one only adds its new blocks. Anything else, a reorg
            // or the first call, walks the whole chain again.
            let indexed = self
                .used_address_index
                .tip
                .and_then(|hash| self.chain.block(&hash))
                .map(|block| (block.hash(), block.header.height));
            let mut added = HashSet::new();
            let mut cursor = Some(tip);
            let mut extends = false;
            while let Some(hash) = cursor {
                if indexed.is_some_and(|(indexed, _)| indexed == hash) {
                    extends = true;
                    break;
                }
                let Some(block) = self.chain.block(&hash) else {
                    break;
                };
                if indexed.is_some_and(|(_, height)| block.header.height <= height) {
                    break;
                }
                insert_block_addresses(&mut added, block);
                cursor = block.header.parent;
            }
            if extends {
                self.used_address_index.chain_addresses.extend(added);
            } else {
                let mut addresses = HashSet::new();
                let mut cursor = Some(tip);
                while let Some(hash) = cursor {
                    let Some(block) = self.chain.block(&hash) else {
                        break;
                    };
                    insert_block_addresses(&mut addresses, block);
                    cursor = block.header.parent;
                }
                self.used_address_index.chain_addresses = addresses;
            }
            self.used_address_index.tip = Some(tip);
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
        let id = transaction.id();
        if self.pending_transactions.contains_key(&id) {
            bail!("Ledger transaction is already pending");
        }
        let inputs = transaction
            .inputs
            .iter()
            .map(|input| input.previous_output.clone())
            .collect::<Vec<_>>();
        // A transaction that cannot fit in a Ledger block can never be mined, and its size is
        // what signature checking costs, so it is refused before any of that work.
        let size = transaction.canonical_size();
        if size > MAX_LEDGER_TRANSACTION_BYTES {
            bail!("Ledger transaction is larger than a Ledger block");
        }
        self.ensure_inputs_available(&inputs)?;
        let minimum_fee = minimum_relay_fee(size);
        if unverified_fee(self.chain.tip(), &transaction).is_some_and(|fee| fee < minimum_fee) {
            bail!("Ledger transaction fee is below the relay minimum of {minimum_fee} units");
        }
        let fee = validate_pending_transaction(self.chain.tip(), &transaction)
            .context("validating pending Ledger transaction")?;
        if fee < minimum_fee {
            bail!("Ledger transaction fee is below the relay minimum of {minimum_fee} units");
        }
        let evictions = evictions_for(
            self.pending_transactions.keys(),
            &self.pending_fees,
            (fee, size),
            Budget {
                count: self.pending_transactions.len(),
                max_count: MAX_PENDING_LEDGER_TRANSACTIONS,
                bytes: self.pending_transaction_bytes,
                max_bytes: MAX_PENDING_LEDGER_BYTES,
            },
        )
        .context(
            "Ledger transaction mempool is full of transactions paying at least as much per byte",
        )?;
        for cheapest in evictions {
            let evicted = self
                .pending_transactions
                .remove(&cheapest)
                .expect("cheapest entry is pending");
            self.pending_transaction_bytes -= evicted.canonical_size();
            self.release(
                &cheapest,
                evicted.inputs.iter().map(|input| &input.previous_output),
            );
        }
        self.reserved_inputs.extend(inputs);
        self.pending_fees.insert(id, (fee, size));
        self.pending_transaction_bytes += size;
        self.pending_transactions.insert(id, transaction);
        self.mempool_generation += 1;
        Ok(fee)
    }

    pub fn submit_ticket(&mut self, ticket: TicketTransaction) -> Result<u64> {
        let id = ticket.id();
        if self.pending_tickets.contains_key(&id) {
            bail!("mining ticket is already pending");
        }
        let inputs = ticket
            .inputs
            .iter()
            .map(|input| input.previous_output.clone())
            .collect::<Vec<_>>();
        if ticket.canonical_size() > MAX_TICKET_BYTES {
            bail!("mining ticket is larger than a Heartbeat block");
        }
        self.ensure_inputs_available(&inputs)?;
        let fee = validate_pending_ticket(self.chain.tip(), &ticket)
            .context("validating pending mining ticket")?;
        let evictions = evictions_for(
            self.pending_tickets.keys(),
            &self.pending_fees,
            (fee, ticket.canonical_size()),
            Budget {
                count: self.pending_tickets.len(),
                max_count: MAX_PENDING_TICKETS,
                bytes: 0,
                max_bytes: usize::MAX,
            },
        )
        .context("ticket mempool is full of tickets paying at least as much per byte")?;
        for cheapest in evictions {
            let evicted = self
                .pending_tickets
                .remove(&cheapest)
                .expect("cheapest entry is pending");
            self.release(
                &cheapest,
                evicted.inputs.iter().map(|input| &input.previous_output),
            );
        }
        self.reserved_inputs.extend(inputs);
        self.pending_fees.insert(id, (fee, ticket.canonical_size()));
        self.pending_tickets.insert(id, ticket);
        self.mempool_generation += 1;
        Ok(fee)
    }

    /// Keeps the best signed candidate seen for each winning ticket slot. Higher total fees win;
    /// the candidate hash is the deterministic tie-break. This bounds equivocation without making
    /// off-chain arrival order part of consensus.
    pub fn submit_candidate(&mut self, candidate: Sidecar) -> Result<bool> {
        let fee = self
            .pending_candidate_fee(&candidate)
            .context("validating pending signed Ledger candidate")?;
        let key = (
            candidate.header.ticket_id,
            candidate.header.slot,
            candidate.header.parent_anchor,
        );
        let hash = candidate.header.hash();
        if let Some(existing) = self.pending_candidates.get(&key) {
            let existing_fee = self
                .pending_candidate_fee(existing)
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
        let winners = self.next_winners().context("selecting Ledger winner")?;
        let sidecar = match &winners.required {
            Some(winner) => Some(fallback_sidecar(winner)),
            None => self
                .pending_candidates
                .values()
                .filter_map(|candidate| {
                    self.pending_candidate_fee(candidate)
                        .ok()
                        .map(|fee| (fee, candidate.header.hash(), candidate))
                })
                .max_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)))
                .map(|(_, _, candidate)| candidate.clone()),
        };
        // Consensus applies the sidecar before the tickets, so a ticket spending an output the
        // sidecar already spends would make the whole template invalid. It stays pending instead.
        let sidecar_spent = sidecar
            .iter()
            .flat_map(|sidecar| sidecar.transactions.iter())
            .flat_map(|transaction| transaction.inputs.iter())
            .map(|input| &input.previous_output)
            .collect::<BTreeSet<_>>();
        let tickets = self.select_tickets(&sidecar_spent);
        Ok(PreparedHeartbeat {
            parent: self.chain.tip().clone(),
            tickets,
            sidecar,
        })
    }

    /// Builds unsigned drafts for every next-block winner controlled by `mining_public_key`.
    /// The wallet signs and submits them; Heartbeat miners never receive the private key.
    pub fn candidate_drafts(&self, mining_public_key: PublicKey) -> Result<Vec<Sidecar>> {
        let winners = self
            .next_winners()
            .context("selecting signed Ledger candidate winner")?;
        if winners.required.is_some() {
            return Ok(Vec::new());
        }
        // Selecting transactions is the expensive part, and most Heartbeats have no winner here.
        let own_winners = winners
            .available
            .iter()
            .filter(|winner| winner.mining_public_key == mining_public_key)
            .cloned()
            .collect::<Vec<_>>();
        if own_winners.is_empty() {
            return Ok(Vec::new());
        }
        let transactions = self.select_ledger_transactions();
        if transactions.is_empty() {
            return Ok(Vec::new());
        }
        let transaction_ids = transactions.iter().map(Transaction::id).collect::<Vec<_>>();
        let transactions_root = merkle_root(&transaction_ids);
        Ok(own_winners
            .into_iter()
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

        // Like Heartbeat templates, reserve the header up front: the winner's signature is added
        // after selection, so counting an unsigned header could overfill the signed block.
        select_template(
            &candidates,
            Template {
                capacity: usize::MAX,
                used_bytes: std::mem::size_of::<u32>(),
                byte_limit: MAX_LEDGER_SIDECAR_BYTES - LEDGER_TEMPLATE_OVERHEAD_BYTES,
            },
            |transaction| {
                transaction
                    .inputs
                    .iter()
                    .map(|input| &input.previous_output)
                    .collect()
            },
            Transaction::canonical_size,
            Transaction::id,
            |candidate| validate_pending_transactions(self.chain.tip(), candidate).is_ok(),
        )
    }

    fn select_tickets(&self, excluded_inputs: &BTreeSet<&OutPoint>) -> Vec<TicketTransaction> {
        let mut candidates = self
            .pending_tickets
            .iter()
            .filter(|(_, ticket)| {
                ticket
                    .inputs
                    .iter()
                    .all(|input| !excluded_inputs.contains(&input.previous_output))
            })
            .filter_map(|(id, ticket)| {
                validate_pending_ticket(self.chain.tip(), ticket)
                    .ok()
                    .map(|fee| (fee, *id, ticket))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

        // Excess tickets stay pending: a template over the tracked cap could never be mined.
        select_template(
            &candidates,
            Template {
                capacity: tracked_ticket_capacity(self.chain.tip()).unwrap_or(0),
                used_bytes: std::mem::size_of::<u32>(),
                byte_limit: MAX_HEARTBEAT_BLOCK_BYTES - HEARTBEAT_TEMPLATE_OVERHEAD_BYTES,
            },
            |ticket| {
                ticket
                    .inputs
                    .iter()
                    .map(|input| &input.previous_output)
                    .collect()
            },
            TicketTransaction::canonical_size,
            TicketTransaction::id,
            |candidate| validate_pending_tickets(self.chain.tip(), candidate).is_ok(),
        )
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

    /// Drops an evicted entry's fee and hands its inputs back.
    fn release<'a>(&mut self, id: &Hash, inputs: impl IntoIterator<Item = &'a OutPoint>) {
        self.pending_fees.remove(id);
        for input in inputs {
            self.reserved_inputs.remove(input);
        }
    }

    fn reconcile_mempool(&mut self) {
        self.mempool_generation += 1;
        let tip = self.chain.tip();
        let mut fees = BTreeMap::new();
        self.pending_transactions.retain(|id, transaction| {
            validate_pending_transaction(tip, transaction)
                .map(|fee| fees.insert(*id, (fee, transaction.canonical_size())))
                .is_ok()
        });
        self.pending_tickets.retain(|id, ticket| {
            validate_pending_ticket(tip, ticket)
                .map(|fee| fees.insert(*id, (fee, ticket.canonical_size())))
                .is_ok()
        });
        self.pending_transaction_bytes = self
            .pending_transactions
            .keys()
            .filter_map(|id| fees.get(id))
            .map(|(_, size)| size)
            .sum();
        self.pending_fees = fees;
        let mut candidates = std::mem::take(&mut self.pending_candidates);
        candidates.retain(|_, candidate| self.pending_candidate_fee(candidate).is_ok());
        self.pending_candidates = candidates;
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

/// A pool's current use and limits.
struct Budget {
    count: usize,
    max_count: usize,
    bytes: usize,
    max_bytes: usize,
}

/// The entries to evict from a pool using `budget` so `candidate` (fee, size) fits: the cheapest
/// per byte first, each paying strictly less per byte than the candidate. `None` when the
/// candidate cannot fit that way, so nothing is evicted for an object that is then refused. Ties
/// go to the higher id, so eviction does not depend on arrival order.
fn evictions_for<'a>(
    ids: impl IntoIterator<Item = &'a Hash>,
    costs: &BTreeMap<Hash, (u64, usize)>,
    candidate: (u64, usize),
    budget: Budget,
) -> Option<Vec<Hash>> {
    let fits = |count: usize, bytes: usize| {
        count < budget.max_count
            && bytes
                .checked_add(candidate.1)
                .is_some_and(|total| total <= budget.max_bytes)
    };
    let (mut count, mut bytes) = (budget.count, budget.bytes);
    if fits(count, bytes) {
        return Some(Vec::new());
    }
    if candidate.1 > budget.max_bytes {
        return None;
    }
    let rate = |(fee, bytes): (u64, usize)| (u128::from(fee), bytes.max(1) as u128);
    // a pays less per byte than b when fee_a * size_b < fee_b * size_a.
    let less = |(left_fee, left_size): (u128, u128), (right_fee, right_size): (u128, u128)| {
        left_fee * right_size < right_fee * left_size
    };
    let candidate_rate = rate(candidate);
    let mut cheaper = ids
        .into_iter()
        .map(|id| (*id, costs.get(id).copied().unwrap_or((0, 1))))
        .filter(|(_, cost)| less(rate(*cost), candidate_rate))
        .collect::<Vec<_>>();
    cheaper.sort_by(|(left_id, left), (right_id, right)| {
        let (left, right) = (rate(*left), rate(*right));
        if less(left, right) {
            std::cmp::Ordering::Less
        } else if less(right, left) {
            std::cmp::Ordering::Greater
        } else {
            right_id.cmp(left_id)
        }
    });
    let mut evicted = Vec::new();
    for (id, (_, size)) in cheaper {
        evicted.push(id);
        count -= 1;
        bytes = bytes.saturating_sub(size);
        if fits(count, bytes) {
            return Some(evicted);
        }
    }
    None
}

struct Template {
    /// Most items the template may hold.
    capacity: usize,
    /// Encoded bytes the template uses before any item.
    used_bytes: usize,
    byte_limit: usize,
}

/// Fills a template from `candidates`, sorted best first, each already valid on its own against
/// the tip. A batch reads every input from the tip, so items that spend disjoint inputs stay valid
/// together; one pass with a spent set then gives the same selection as re-validating the growing
/// set after every item, without its quadratic cost. The result is still validated once, and the
/// slow selection runs only if that ever fails.
fn select_template<T: Clone>(
    candidates: &[(u64, Hash, &T)],
    template: Template,
    inputs: impl Fn(&T) -> Vec<&OutPoint>,
    size: impl Fn(&T) -> usize,
    id: impl Fn(&T) -> Hash + Copy,
    valid: impl Fn(&[T]) -> bool,
) -> Vec<T> {
    let item_size = |item: &T| std::mem::size_of::<u32>() + size(item);
    let mut used_bytes = template.used_bytes;
    let mut fees = 0u64;
    let mut spent = BTreeSet::new();
    let mut selected = Vec::new();
    for (fee, _, item) in candidates {
        if selected.len() >= template.capacity {
            break;
        }
        let item_bytes = item_size(item);
        let item_inputs = inputs(item);
        let Some(total) = fees.checked_add(*fee) else {
            continue;
        };
        if used_bytes + item_bytes > template.byte_limit
            || item_inputs.iter().any(|point| spent.contains(*point))
        {
            continue;
        }
        spent.extend(item_inputs);
        fees = total;
        used_bytes += item_bytes;
        selected.push((*item).clone());
    }
    selected.sort_by_key(id);
    if valid(&selected) {
        return selected;
    }
    eprintln!(
        "warning: a template of independent mempool objects failed validation; selecting one by one"
    );

    let mut used_bytes = template.used_bytes;
    let mut selected = Vec::new();
    for (_, _, item) in candidates {
        if selected.len() >= template.capacity {
            break;
        }
        let item_bytes = item_size(item);
        if used_bytes + item_bytes > template.byte_limit {
            continue;
        }
        if select_canonical_if_valid(&mut selected, (*item).clone(), id, |candidate| {
            valid(candidate)
        }) {
            used_bytes += item_bytes;
        }
    }
    selected
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

    #[test]
    fn template_overheads_cover_the_signed_headers() {
        let sidecar = Sidecar {
            header: CandidateHeader {
                version: PROTOCOL_VERSION,
                lottery_heartbeat: [0; 32],
                lottery_height: u64::MAX,
                parent_anchor: [0; 32],
                ticket_id: [0; 32],
                slot: u64::MAX,
                lottery_output: [0; 32],
                transactions_root: merkle_root(&[]),
                signature: vec![0; 64],
            },
            transactions: Vec::new(),
        };
        // The empty sidecar already holds its transaction count.
        assert!(sidecar.canonical_size() <= LEDGER_TEMPLATE_OVERHEAD_BYTES);
        // The largest admitted transaction is exactly what a template still selects on its own.
        let template_room = MAX_LEDGER_SIDECAR_BYTES - LEDGER_TEMPLATE_OVERHEAD_BYTES;
        assert_eq!(4 + 4 + MAX_LEDGER_TRANSACTION_BYTES, template_room);
        assert_eq!(4 + 4 + MAX_TICKET_BYTES, MAX_HEARTBEAT_BLOCK_BYTES - 512);
    }

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
    fn a_new_tip_does_not_verify_pending_signatures_again() {
        let mut node = Node::open(MemoryRepository::default()).unwrap();
        let transaction = signed_genesis_transaction(&node);
        let mut tampered = transaction.clone();
        node.submit_transaction(transaction).unwrap();

        let before = crate::consensus::signature_verifications();
        node.mine_next(GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS, [44; 32])
            .unwrap();

        assert_eq!(node.pending_transaction_count(), 1);
        assert_eq!(crate::consensus::signature_verifications(), before);

        // The cache holds only what verified: another signature on the same message is checked.
        tampered.inputs[0].signature[0] ^= 1;
        let mut fresh = Node::open(MemoryRepository::default()).unwrap();
        assert!(fresh.submit_transaction(tampered).is_err());
        assert_eq!(crate::consensus::signature_verifications(), before + 1);
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
    fn address_usage_follows_new_blocks_and_reorgs() {
        let mut node = Node::open(MemoryRepository::default()).unwrap();
        let mut fork = Node::open(MemoryRepository::default()).unwrap();
        assert!(!node.address_used(&address_from_public_key(&[44; 32])));

        let first = GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS;
        node.mine_next(first, [44; 32]).unwrap();
        assert!(node.address_used(&address_from_public_key(&[44; 32])));
        node.mine_next(first + HEARTBEAT_TARGET_INTERVAL_SECS, [45; 32])
            .unwrap();
        assert!(node.address_used(&address_from_public_key(&[45; 32])));

        // A heavier branch from genesis replaces both blocks.
        for (index, miner) in [[46; 32], [47; 32], [48; 32]].into_iter().enumerate() {
            let timestamp = first + index as u64 * HEARTBEAT_TARGET_INTERVAL_SECS;
            let block = fork.mine_next(timestamp, miner).unwrap();
            node.submit_block(block, timestamp).unwrap();
        }
        assert_eq!(node.chain().tip().tip, fork.chain().tip().tip);
        assert!(node.address_used(&address_from_public_key(&[48; 32])));
        assert!(!node.address_used(&address_from_public_key(&[44; 32])));
        assert!(!node.address_used(&address_from_public_key(&[45; 32])));
    }

    #[test]
    fn balance_ranking_follows_the_tip() {
        let mut node = Node::open(MemoryRepository::default()).unwrap();
        let (holders, top) = node.top_balances();
        assert_eq!(holders, 1);
        assert_eq!(
            top,
            [AddressBalance {
                address: address_from_public_key(&GENESIS_MINER),
                amount: GENESIS_REWARD,
                outputs: 1,
            }]
        );

        let ticket = signed_genesis_ticket(&node);
        node.submit_ticket(ticket).unwrap();
        node.mine_next(GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS, [44; 32])
            .unwrap();
        let tip = node.chain().tip().clone();
        let (holders, top) = node.top_balances();
        let spendable: u64 = tip.utxos.values().map(|output| output.value).sum();
        assert_eq!(holders, top.len());
        assert_eq!(top.iter().map(|entry| entry.amount).sum::<u64>(), spendable);
        assert!(top.windows(2).all(|pair| pair[0].amount >= pair[1].amount));
        assert!(
            top.iter()
                .any(|entry| entry.address == address_from_public_key(&[44; 32]))
        );
    }

    #[test]
    fn relay_fee_rounds_up_at_each_kilobyte_boundary() {
        assert_eq!(minimum_relay_fee(0), 0);
        assert_eq!(minimum_relay_fee(1), 1);
        assert_eq!(minimum_relay_fee(1_000), 1);
        assert_eq!(minimum_relay_fee(1_001), 2);
    }

    #[test]
    fn unpayable_or_oversized_transactions_are_refused_before_signature_checks() {
        let mut node = Node::open(MemoryRepository::default()).unwrap();
        let mut free = signed_genesis_transaction(&node);
        free.outputs[0].value = GENESIS_REWARD;
        free.inputs[0].signature = vec![0; 64];
        let error = node.submit_transaction(free).unwrap_err();
        assert!(
            format!("{error:#}").contains("below the relay minimum"),
            "{error:#}"
        );

        let mut oversized = signed_genesis_transaction(&node);
        oversized.outputs = vec![
            Output {
                owner: [41; 32],
                value: 1,
            };
            MAX_LEDGER_SIDECAR_BYTES / 40
        ];
        let error = node.submit_transaction(oversized).unwrap_err();
        assert!(
            format!("{error:#}").contains("larger than a Ledger block"),
            "{error:#}"
        );
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
    fn canonical_hashes_follow_new_tips_and_reorgs() {
        let canonical = |node: &Node<MemoryRepository>| {
            node.chain()
                .canonical_blocks()
                .into_iter()
                .map(HeartbeatBlock::hash)
                .collect::<Vec<_>>()
        };
        let mut node = Node::open(MemoryRepository::default()).unwrap();
        assert_eq!(*node.canonical_hashes(), canonical(&node));
        let first = GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS;
        node.mine_next(first, [44; 32]).unwrap();
        let held = node.canonical_hashes();
        assert_eq!(*held, canonical(&node));

        let mut other = Node::open(MemoryRepository::default()).unwrap();
        let heavier = [
            other.mine_next(first, [45; 32]).unwrap(),
            other
                .mine_next(first + HEARTBEAT_TARGET_INTERVAL_SECS, [45; 32])
                .unwrap(),
        ];
        for block in heavier {
            node.submit_block(block, first + 2 * HEARTBEAT_TARGET_INTERVAL_SECS)
                .unwrap();
        }

        assert_eq!(*node.canonical_hashes(), canonical(&other));
        assert_eq!(node.canonical_hashes().len(), 3);
        // A caller holding an earlier index keeps the view it was given.
        assert_eq!(held.len(), 2);
        assert_ne!(held[1], node.canonical_hashes()[1]);
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
    fn template_selection_skips_spent_inputs_and_respects_its_limits() {
        #[derive(Clone, Debug, Eq, PartialEq)]
        struct Item {
            id: Hash,
            input: OutPoint,
            size: usize,
        }
        let point = |index| OutPoint {
            transaction_id: [index; 32],
            output_index: 0,
        };
        let item = |id, input, size| Item {
            id: [id; 32],
            input: point(input),
            size,
        };
        let items = [
            item(9, 1, 10),
            item(8, 1, 10),
            item(7, 2, 1_000),
            item(6, 3, 10),
        ];
        let candidates = items
            .iter()
            .map(|item| (1, item.id, item))
            .collect::<Vec<_>>();
        let template = |capacity| Template {
            capacity,
            used_bytes: 0,
            byte_limit: 100,
        };
        let select = |capacity, valid: &dyn Fn(&[Item]) -> bool| {
            select_template(
                &candidates,
                template(capacity),
                |item: &Item| vec![&item.input],
                |item: &Item| item.size,
                |item: &Item| item.id,
                valid,
            )
        };

        // The second spender of input 1 and the oversized item are skipped; ids come out sorted.
        assert_eq!(
            select(usize::MAX, &|_| true),
            vec![items[3].clone(), items[0].clone()]
        );
        assert_eq!(select(1, &|_| true), vec![items[0].clone()]);
        // Fees that would overflow together leave the later item out.
        let overflowing = [
            (u64::MAX, items[0].id, &items[0]),
            (1, items[3].id, &items[3]),
        ];
        assert_eq!(
            select_template(
                &overflowing,
                template(usize::MAX),
                |item: &Item| vec![&item.input],
                |item: &Item| item.size,
                |item: &Item| item.id,
                |_: &[Item]| true,
            ),
            vec![items[0].clone()]
        );
        // If the combined set were ever refused, the item-by-item selection decides instead.
        assert_eq!(
            select(usize::MAX, &|selected: &[Item]| selected.len() <= 1),
            vec![items[0].clone()]
        );
    }

    /// A pool holding `count` entries that is full by count.
    fn full(count: usize) -> Budget {
        Budget {
            count,
            max_count: count,
            bytes: 0,
            max_bytes: usize::MAX,
        }
    }

    #[test]
    fn a_full_mempool_evicts_only_for_a_higher_fee_per_byte() {
        // 10 units over 100 bytes, 10 over 200 and 30 over 100: [2; 32] pays least per byte.
        let costs = BTreeMap::from([
            ([1; 32], (10, 100)),
            ([2; 32], (10, 200)),
            ([3; 32], (30, 100)),
        ]);
        let ids = [[1; 32], [2; 32], [3; 32]];

        assert_eq!(
            evictions_for(&ids, &costs, (6, 100), full(3)),
            Some(vec![[2; 32]])
        );
        assert_eq!(evictions_for(&ids, &costs, (5, 100), full(3)), None);
        assert_eq!(evictions_for(&ids, &costs, (10, 200), full(3)), None);
        // Only the given pool is scanned.
        assert_eq!(
            evictions_for(&[[1; 32]], &costs, (11, 100), full(1)),
            Some(vec![[1; 32]])
        );
        // Equal rates evict the higher id, whatever arrived first.
        let tied = BTreeMap::from([
            ([1; 32], (10, 100)),
            ([2; 32], (20, 200)),
            ([3; 32], (30, 100)),
        ]);
        assert_eq!(
            evictions_for(&ids, &tied, (11, 100), full(3)),
            Some(vec![[2; 32]])
        );
        assert_eq!(evictions_for(&[], &costs, (1, 1), full(0)), None);
        // A pool with room evicts nothing.
        let room = Budget {
            max_count: 4,
            ..full(3)
        };
        assert_eq!(evictions_for(&ids, &costs, (1, 1), room), Some(Vec::new()));
    }

    #[test]
    fn a_mempool_full_by_bytes_evicts_until_the_candidate_fits() {
        let costs = BTreeMap::from([
            ([1; 32], (10, 100)),
            ([2; 32], (10, 200)),
            ([3; 32], (30, 100)),
        ]);
        let ids = [[1; 32], [2; 32], [3; 32]];
        let budget = |max_bytes| Budget {
            count: 3,
            max_count: 10,
            bytes: 400,
            max_bytes,
        };

        // 250 more bytes in a 500-byte pool free 150: [2; 32] alone frees 200.
        assert_eq!(
            evictions_for(&ids, &costs, (100, 250), budget(500)),
            Some(vec![[2; 32]])
        );
        // 350 more bytes need 250 freed: [2; 32], then [1; 32].
        assert_eq!(
            evictions_for(&ids, &costs, (100, 350), budget(500)),
            Some(vec![[2; 32], [1; 32]])
        );
        // Room would need [3; 32] too, which pays as much per byte, so nothing is evicted.
        assert_eq!(evictions_for(&ids, &costs, (60, 200), budget(250)), None);
        // An object larger than the whole budget never fits.
        assert_eq!(
            evictions_for(&ids, &costs, (u64::MAX, 600), budget(500)),
            None
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

    /// A signed candidate and a pending ticket that spend the same output are each valid against
    /// the tip, but not together. The template must leave the ticket out so mining continues.
    fn mine_past_a_ticket_conflicting_with_the_candidate(ticket_first: bool) {
        use crate::consensus::subsidy;

        let mut node = Node::open(MemoryRepository::default()).unwrap();
        let genesis_key = SigningKey::from_bytes(&GENESIS_SECRET);
        let genesis_address = address_from_public_key(&GENESIS_MINER);
        let candidate_key = SigningKey::from_bytes(&[27; 32]);
        let genesis_point = node.chain().tip().utxos.keys().next().unwrap().clone();

        let duration = 200;
        let budget = minimum_ticket_budget(duration, INITIAL_LEDGER_SUBSIDY).unwrap();
        let mut lottery_ticket = TicketTransaction {
            version: PROTOCOL_VERSION,
            inputs: vec![TicketInput {
                previous_output: genesis_point,
                public_key: GENESIS_MINER,
                signature: Vec::new(),
            }],
            budget,
            duration,
            mining_public_key: candidate_key.verifying_key().to_bytes(),
            lottery_seed: [43; 32],
            change: Some(Output {
                owner: genesis_address,
                value: GENESIS_REWARD - budget - 1_000,
            }),
        };
        lottery_ticket.inputs[0].signature = genesis_key
            .sign(&lottery_ticket.signing_digest(0, &[GENESIS_REWARD]).unwrap())
            .to_bytes()
            .to_vec();
        let lottery_ticket_id = lottery_ticket.id();
        node.submit_ticket(lottery_ticket).unwrap();

        let mut timestamp = GENESIS_TIMESTAMP;
        let winner = loop {
            timestamp += HEARTBEAT_TARGET_INTERVAL_SECS;
            node.mine_next(timestamp, GENESIS_MINER).unwrap();
            let tip = node.chain().tip();
            assert!(tip.height < 190, "no pre-deadline win found");
            let winners = inclusion_winners(tip, tip.height + 1).unwrap();
            if winners.required.is_none()
                && let Some(winner) = winners
                    .available
                    .into_iter()
                    .find(|winner| winner.ticket_id == lottery_ticket_id)
            {
                break winner;
            }
        };
        let tip = node.chain().tip().clone();
        let (spent, spent_output) = tip
            .utxos
            .iter()
            .filter(|(_, output)| output.owner == genesis_address)
            .max_by_key(|(_, output)| output.value)
            .map(|(point, output)| (point.clone(), output.clone()))
            .unwrap();

        let mut transaction = Transaction {
            version: PROTOCOL_VERSION,
            inputs: vec![Input {
                previous_output: spent.clone(),
                public_key: GENESIS_MINER,
                signature: Vec::new(),
            }],
            outputs: vec![Output {
                owner: [41; 32],
                value: spent_output.value - 5_000,
            }],
        };
        transaction.inputs[0].signature = genesis_key
            .sign(
                &transaction
                    .signing_digest(0, &[spent_output.value])
                    .unwrap(),
            )
            .to_bytes()
            .to_vec();
        let mut candidate = Sidecar {
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
        candidate.header.signature = candidate_key
            .sign(&candidate.header.signing_digest())
            .to_bytes()
            .to_vec();

        let ticket_budget =
            minimum_ticket_budget(1, subsidy(tip.ledger_blocks, tip.minted).unwrap()).unwrap();
        let mut conflicting_ticket = TicketTransaction {
            version: PROTOCOL_VERSION,
            inputs: vec![TicketInput {
                previous_output: spent,
                public_key: GENESIS_MINER,
                signature: Vec::new(),
            }],
            budget: ticket_budget,
            duration: 1,
            mining_public_key: [42; 32],
            lottery_seed: [44; 32],
            change: Some(Output {
                owner: genesis_address,
                value: spent_output.value - ticket_budget - 1_000,
            }),
        };
        conflicting_ticket.inputs[0].signature = genesis_key
            .sign(
                &conflicting_ticket
                    .signing_digest(0, &[spent_output.value])
                    .unwrap(),
            )
            .to_bytes()
            .to_vec();

        if ticket_first {
            node.submit_ticket(conflicting_ticket).unwrap();
            assert!(node.submit_candidate(candidate).unwrap());
        } else {
            assert!(node.submit_candidate(candidate).unwrap());
            node.submit_ticket(conflicting_ticket).unwrap();
        }

        timestamp += HEARTBEAT_TARGET_INTERVAL_SECS;
        let block = node.mine_next(timestamp, GENESIS_MINER).unwrap();
        assert!(
            block
                .sidecar
                .is_some_and(|sidecar| sidecar.transactions.len() == 1)
        );
        assert!(block.tickets.is_empty());
        assert_eq!(node.chain().tip().height, tip.height + 1);
    }

    #[test]
    fn template_skips_a_ticket_that_spends_a_candidate_input_submitted_before_it() {
        mine_past_a_ticket_conflicting_with_the_candidate(false);
    }

    #[test]
    fn template_skips_a_ticket_that_spends_a_candidate_input_submitted_after_it() {
        mine_past_a_ticket_conflicting_with_the_candidate(true);
    }
}
