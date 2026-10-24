//! Incrementally maintained indexes over confirmed history.
//!
//! The live ledger may retain only genesis plus a recent block window, so queries that used to
//! scan `self.chain` must instead read state that every apply path keeps up to date. Snapshot
//! restoration replays every persisted block, which rebuilds these indexes after a restart.

use std::{collections::BTreeSet, sync::Arc};

use anyhow::{Context, Result};

use super::ledger_builders::push_unique_address;
use super::ticket::{
    BurnTicket, draw_parent_randomness, ranked_tickets_for_parent_randomness,
    ticket_is_eligible_for_height,
};
use super::{
    AddressNetwork, AddressVersion, Block, BurnLeaderRank, Ledger, SignatureScheme, Transaction,
    TransactionV2, TransactionV2Input, V2SpendingAuthorization, VersionedAddress,
    decode_versioned_address,
};

/// Ticket draw inputs are retained for more blocks than the app keeps resident, so a UI
/// projection that briefly lags behind the tip can still derive burn leader ranks.
pub(super) const RESIDENT_TICKET_DRAW_BLOCKS: u64 = 128;

/// The burn tickets eligible for a block, captured just before that block was applied.
#[derive(Debug)]
pub(super) struct ResidentTicketDraw {
    block_hash: String,
    parent_randomness: String,
    eligible_tickets: Vec<BurnTicket>,
}

/// Hybrid-address effects of one block, committed only after the block applied successfully.
#[derive(Default)]
pub(super) struct BlockAddressEffects {
    used: Vec<VersionedAddress>,
    spent: Vec<VersionedAddress>,
    authored: Vec<([u8; 32], VersionedAddress)>,
}

impl BlockAddressEffects {
    pub(super) fn for_block_outputs(block: &Block, network: AddressNetwork) -> Self {
        let mut effects = Self::default();
        if let Some(address) = block.reward_address.as_deref()
            && let Ok(address) = decode_versioned_address(address, network)
            && address.version == AddressVersion::HybridKeyCommitment
        {
            effects.used.push(address);
        }
        for transaction in &block.transactions {
            collect_legacy_hybrid_outputs(transaction, network, &mut effects.used);
        }
        effects
    }

    pub(super) fn record_transaction_v2(&mut self, transaction: &TransactionV2) {
        collect_v2_output_addresses(transaction, &mut self.used);
        collect_v2_input_addresses(transaction, &mut self.spent);
        match transaction {
            TransactionV2::Migration {
                outputs,
                authorizations,
                ..
            } => {
                for authorization in authorizations {
                    if authorization.scheme() == SignatureScheme::Ed25519
                        && let Ok(legacy_key) =
                            <[u8; 32]>::try_from(authorization.public_key().as_bytes())
                    {
                        self.authored
                            .extend(outputs.iter().map(|output| (legacy_key, output.address)));
                    }
                }
            }
            TransactionV2::Transfer {
                inputs,
                outputs,
                authorizations,
                ..
            } => {
                // Native and browser builders place the external recipient first and
                // deterministic wallet change, when present, in subsequent outputs.
                let change = outputs.iter().skip(1).map(|output| output.address);
                self.record_authored_spend(inputs, authorizations, change);
            }
            TransactionV2::Burn {
                inputs,
                change,
                authorizations,
                ..
            } => {
                let change = change.iter().map(|output| output.address);
                self.record_authored_spend(inputs, authorizations, change);
            }
            TransactionV2::Mine { .. } => {}
        }
    }

    fn record_authored_spend(
        &mut self,
        inputs: &[TransactionV2Input],
        authorizations: &[V2SpendingAuthorization],
        change: impl Iterator<Item = VersionedAddress> + Clone,
    ) {
        for authorization in authorizations {
            if authorization.scheme() != SignatureScheme::HybridEd25519MlDsa44 {
                continue;
            }
            let Some(legacy_key) = authorization
                .public_key()
                .as_bytes()
                .get(..32)
                .and_then(|prefix| <[u8; 32]>::try_from(prefix).ok())
            else {
                continue;
            };
            let Ok(owner) = authorization.authorized_address() else {
                continue;
            };
            if !inputs.iter().any(|input| input.owner == owner) {
                continue;
            }
            self.authored.push((legacy_key, owner));
            self.authored
                .extend(change.clone().map(|address| (legacy_key, address)));
        }
    }
}

/// Membership test over pending addresses plus the confirmed index, without merging them.
pub(super) struct HybridAddressLookup<'a> {
    pending: BTreeSet<VersionedAddress>,
    confirmed: Option<&'a BTreeSet<VersionedAddress>>,
}

impl<'a> HybridAddressLookup<'a> {
    pub(super) fn new(
        pending: BTreeSet<VersionedAddress>,
        confirmed: &'a BTreeSet<VersionedAddress>,
    ) -> Self {
        Self {
            pending,
            confirmed: Some(confirmed),
        }
    }

    pub(super) fn pending_only(pending: BTreeSet<VersionedAddress>) -> Self {
        Self {
            pending,
            confirmed: None,
        }
    }

    pub(super) fn contains(&self, address: &VersionedAddress) -> bool {
        self.pending.contains(address)
            || self
                .confirmed
                .is_some_and(|confirmed| confirmed.contains(address))
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = VersionedAddress> + '_ {
        self.pending.iter().copied().chain(
            self.confirmed
                .into_iter()
                .flatten()
                .copied()
                .filter(|address| !self.pending.contains(address)),
        )
    }
}

/// Hybrid addresses a legacy transaction pays to. Shared by the pending pool and the
/// confirmed index so both classify transactions the same way.
pub(super) fn collect_legacy_hybrid_outputs(
    transaction: &Transaction,
    network: AddressNetwork,
    used: &mut Vec<VersionedAddress>,
) {
    for output in transaction.outputs() {
        if let Ok(address) = decode_versioned_address(&output.address, network)
            && address.version == AddressVersion::HybridKeyCommitment
        {
            push_unique_address(used, address);
        }
    }
}

pub(super) fn collect_v2_output_addresses(
    transaction: &TransactionV2,
    used: &mut Vec<VersionedAddress>,
) {
    let outputs = match transaction {
        TransactionV2::Migration { outputs, .. } | TransactionV2::Transfer { outputs, .. } => {
            outputs.as_slice()
        }
        TransactionV2::Burn { change, .. } => change.as_slice(),
        TransactionV2::Mine { recipient, .. } => {
            push_unique_address(used, *recipient);
            return;
        }
    };
    for output in outputs {
        push_unique_address(used, output.address);
    }
}

pub(super) fn collect_v2_input_addresses(
    transaction: &TransactionV2,
    spent: &mut Vec<VersionedAddress>,
) {
    let inputs = match transaction {
        TransactionV2::Transfer { inputs, .. } | TransactionV2::Burn { inputs, .. } => {
            inputs.as_slice()
        }
        TransactionV2::Migration { .. } | TransactionV2::Mine { .. } => return,
    };
    for input in inputs {
        push_unique_address(spent, input.owner);
    }
}

impl Ledger {
    pub(super) fn commit_block_address_effects(&mut self, effects: BlockAddressEffects) {
        if !effects.used.is_empty() {
            self.confirmed_hybrid_used_addresses.extend(effects.used);
        }
        if !effects.spent.is_empty() {
            self.confirmed_hybrid_spent_addresses.extend(effects.spent);
        }
        if effects.authored.is_empty() {
            return;
        }
        let authored = &mut *self.confirmed_hybrid_authored_addresses;
        for (legacy_key, address) in effects.authored {
            let addresses = authored.entry(legacy_key).or_default();
            if !addresses.contains(&address) {
                addresses.push(address);
            }
        }
    }

    /// Index a block whose v2 envelopes have not been decoded by an apply path (genesis).
    pub(super) fn index_unapplied_block_addresses(&mut self, block: &Block) -> Result<()> {
        let network = AddressNetwork::from_profile_id(&self.launch_profile.profile_id);
        let mut effects = BlockAddressEffects::for_block_outputs(block, network);
        if block.transactions_v2.is_empty() {
            self.commit_block_address_effects(effects);
            return Ok(());
        }
        let domain = self.transaction_v2_domain()?;
        for envelope in &block.transactions_v2 {
            let bytes =
                super::hex::decode_hex(envelope).context("invalid confirmed transaction-v2 hex")?;
            let (decoded_domain, transaction) = TransactionV2::decode(&bytes)?;
            if decoded_domain == domain {
                effects.record_transaction_v2(&transaction);
            }
        }
        self.commit_block_address_effects(effects);
        Ok(())
    }

    pub(super) fn confirmed_hybrid_used_addresses(&self) -> &BTreeSet<VersionedAddress> {
        &self.confirmed_hybrid_used_addresses
    }

    pub(super) fn confirmed_hybrid_spent_addresses(&self) -> &BTreeSet<VersionedAddress> {
        &self.confirmed_hybrid_spent_addresses
    }

    pub(super) fn confirmed_hybrid_authored_addresses(
        &self,
        legacy_public_key: &[u8; 32],
    ) -> Vec<VersionedAddress> {
        self.confirmed_hybrid_authored_addresses
            .get(legacy_public_key)
            .cloned()
            .unwrap_or_default()
    }

    /// Tell a snapshot replay which height it is heading for, so it skips capturing ticket
    /// draws that would be evicted before anyone reads them.
    pub(crate) fn set_ticket_draw_horizon(&mut self, height: u64) {
        self.ticket_draw_horizon = height;
    }

    /// Capture the ticket set a block is drawn from. Call before the block mutates `tickets`.
    pub(super) fn ticket_draw_before(&self, block: &Block) -> Option<Arc<ResidentTicketDraw>> {
        if block.height.saturating_add(RESIDENT_TICKET_DRAW_BLOCKS) <= self.ticket_draw_horizon {
            return None;
        }
        Some(Arc::new(ResidentTicketDraw {
            block_hash: block.hash.clone(),
            parent_randomness: draw_parent_randomness(self.tip(), block.height),
            eligible_tickets: self
                .tickets
                .iter()
                .filter(|ticket| ticket_is_eligible_for_height(ticket, block.height))
                .cloned()
                .collect(),
        }))
    }

    pub(super) fn record_ticket_draw(
        &mut self,
        height: u64,
        draw: Option<Arc<ResidentTicketDraw>>,
    ) {
        let Some(draw) = draw else {
            return;
        };
        let draws = &mut *self.resident_ticket_draws;
        draws.insert(height, draw);
        let oldest_retained = height.saturating_sub(RESIDENT_TICKET_DRAW_BLOCKS - 1);
        while draws
            .first_key_value()
            .is_some_and(|(retained, _)| *retained < oldest_retained)
        {
            draws.pop_first();
        }
    }

    /// Burn leader ranks from the retained draw inputs, without replaying history. The draw is
    /// only used when it was captured for exactly this block.
    pub(super) fn cached_burn_leader_ranks(
        &self,
        height: u64,
        block_hash: &str,
    ) -> Option<Vec<BurnLeaderRank>> {
        let draw = self.resident_ticket_draws.get(&height)?;
        if draw.block_hash != block_hash {
            return None;
        }
        Some(
            ranked_tickets_for_parent_randomness(
                &draw.parent_randomness,
                height,
                &draw.eligible_tickets,
            )
            .into_iter()
            .enumerate()
            .map(|(rank, ticket)| BurnLeaderRank {
                rank: rank as u32,
                ticket_id: ticket.id,
                owner: ticket.owner,
                amount: ticket.amount,
                eligible_from_height: ticket.eligible_from_height,
                eligible_until_height: ticket.eligible_until_height,
            })
            .collect(),
        )
    }

    /// Burn leader ranks keyed by block hash, for blocks that need not be resident (for example
    /// blocks streamed from the chain store). Blocks that are neither covered by a retained draw
    /// nor replayable from resident history are omitted.
    pub fn burn_leader_ranks_by_block_hash(
        &self,
        blocks: &[Block],
    ) -> Result<std::collections::BTreeMap<String, Vec<BurnLeaderRank>>> {
        let mut ranks = std::collections::BTreeMap::new();
        let mut replay = Vec::new();
        for block in blocks {
            if block.height == 0 {
                ranks.insert(block.hash.clone(), Vec::new());
            } else if let Some(cached) = self.cached_burn_leader_ranks(block.height, &block.hash) {
                ranks.insert(block.hash.clone(), cached);
            } else if !self.history_pruned
                && self
                    .resident_block_at_height(block.height)
                    .is_some_and(|resident| resident.hash == block.hash)
            {
                replay.push(block);
            }
        }
        if !replay.is_empty() {
            let mut replayed =
                self.burn_leader_ranks_for_blocks(replay.iter().map(|block| block.height))?;
            for block in replay {
                if let Some(block_ranks) = replayed.remove(&block.height) {
                    ranks.insert(block.hash.clone(), block_ranks);
                }
            }
        }
        Ok(ranks)
    }

    /// Reattach pruned history from a ledger rebuilt from the chain store. Blocks this ledger
    /// applied after the store's tip are replayed onto the rebuilt copy first. Returns false
    /// when the rebuilt copy is not an ancestor of the resident tip, so callers can retry.
    pub fn restore_pruned_history(&mut self, mut complete: Ledger) -> Result<bool> {
        if !self.history_pruned {
            return Ok(true);
        }
        if complete.history_pruned {
            anyhow::bail!("cannot restore history from a pruned ledger");
        }
        if complete.genesis_hash() != self.genesis_hash() {
            anyhow::bail!("restored history belongs to a different genesis");
        }
        let restored_tip = complete.tip();
        let extends_restored_tip = match self.resident_block_at_height(restored_tip.height) {
            Some(block) => block.hash == restored_tip.hash,
            None => self
                .resident_block_at_height(restored_tip.height.saturating_add(1))
                .is_some_and(|block| block.prev_hash == restored_tip.hash),
        };
        if !extends_restored_tip {
            return Ok(false);
        }
        let newer = self
            .chain
            .iter()
            .filter(|block| block.height > restored_tip.height)
            .cloned()
            .collect::<Vec<_>>();
        for block in newer {
            complete.apply_trusted_block_at(block)?;
        }
        if complete.tip_hash() != self.tip_hash() {
            return Ok(false);
        }
        self.chain = complete.chain;
        self.compact_block_context = complete.compact_block_context;
        self.history_pruned = false;
        Ok(true)
    }

    /// Look up a block by height in the resident window (genesis plus a contiguous tail).
    pub fn resident_block_at_height(&self, height: u64) -> Option<&Block> {
        let genesis = self.chain.first()?;
        if height == genesis.height {
            return Some(genesis);
        }
        let first_tail = self.chain.get(1)?;
        let offset = usize::try_from(height.checked_sub(first_tail.height)?).ok()?;
        self.chain
            .get(offset.checked_add(1)?)
            .filter(|block| block.height == height)
    }

    /// Height of the oldest non-genesis block still resident in memory.
    pub fn first_resident_tail_height(&self) -> Option<u64> {
        self.chain.get(1).map(|block| block.height)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::domain::{GenesisBurn, VDF_TARGET_BLOCK_MS, Wallet};

    fn mined_ledger(seed: &str, blocks: u64) -> (Ledger, Wallet) {
        let wallet = Wallet::from_seed(seed);
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), 1_000);
        let mut ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(wallet.address(), 10)],
            1,
        )
        .unwrap();
        for _ in 0..blocks {
            mine_block(&mut ledger, &wallet);
        }
        (ledger, wallet)
    }

    fn mine_block(ledger: &mut Ledger, wallet: &Wallet) {
        mine_block_after(ledger, wallet, 1_000);
    }

    fn mine_block_after(ledger: &mut Ledger, wallet: &Wallet, delay_ms: u64) {
        let timestamp_ms = ledger.tip().timestamp_ms + delay_ms;
        let burn = ledger.build_burn(wallet, 1, 1).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let block = ledger.mine_next_block(wallet, timestamp_ms).unwrap();
        ledger.apply_locally_mined_block(block).unwrap();
    }

    #[test]
    fn vdf_retarget_does_not_depend_on_resident_history() {
        let (mut ledger, wallet) = mined_ledger("vdf-retarget-window", 0);
        for delay_ms in [
            VDF_TARGET_BLOCK_MS / 2,
            VDF_TARGET_BLOCK_MS,
            VDF_TARGET_BLOCK_MS * 3 / 2,
            VDF_TARGET_BLOCK_MS * 2,
            VDF_TARGET_BLOCK_MS * 3,
            VDF_TARGET_BLOCK_MS * 4,
        ] {
            mine_block_after(&mut ledger, &wallet, delay_ms);
        }
        let full_chain_average =
            super::super::vdf::recent_vdf_retarget_average_observed_block_ms(&ledger.chain);
        assert_eq!(
            super::super::vdf::average_vdf_retarget_observation(&ledger.vdf_retarget_observations),
            full_chain_average
        );
        let expected = ledger.next_vdf_rounds_after_tip();

        ledger.retain_recent_chain_blocks(2);

        assert_ne!(
            super::super::vdf::recent_vdf_retarget_average_observed_block_ms(&ledger.chain),
            full_chain_average,
            "the test chain must make the pruned window disagree with full history"
        );
        assert_eq!(ledger.next_vdf_rounds_after_tip(), expected);
    }

    #[test]
    fn retained_ticket_draws_rank_like_a_genesis_replay() {
        let (ledger, _) = mined_ledger("ticket-draw-ranks", 6);
        let heights = 1..=ledger.height();
        let cached = ledger
            .burn_leader_ranks_for_blocks(heights.clone())
            .unwrap();

        let mut replay_only = ledger.clone();
        replay_only.resident_ticket_draws = Default::default();
        let replayed = replay_only.burn_leader_ranks_for_blocks(heights).unwrap();

        assert_eq!(cached, replayed);
        assert!(cached.values().any(|ranks| !ranks.is_empty()));
    }

    #[test]
    fn pruned_ledger_still_ranks_resident_blocks() {
        let (mut ledger, _) = mined_ledger("pruned-ticket-draw-ranks", 6);
        let expected = ledger.burn_leader_ranks_for_blocks([4, 5, 6]).unwrap();
        ledger.retain_recent_chain_blocks(3);

        let ranks = ledger.burn_leader_ranks_for_blocks([2, 4, 5, 6]).unwrap();

        // Height 4 is the oldest resident block; its retained draw still ranks it although its
        // parent is pruned. Height 2 is no longer resident, so it is omitted.
        assert!(!ranks.contains_key(&2));
        assert_eq!(ranks.get(&4), expected.get(&4));
        assert_eq!(ranks.get(&5), expected.get(&5));
        assert_eq!(ranks.get(&6), expected.get(&6));
    }

    #[test]
    fn retained_draws_rank_blocks_that_are_no_longer_resident() {
        let (mut ledger, _) = mined_ledger("non-resident-ticket-draw-ranks", 6);
        let early_blocks = ledger.chain[1..4].to_vec();
        let expected = ledger
            .burn_leader_ranks_by_block_hash(&early_blocks)
            .unwrap();
        ledger.retain_recent_chain_blocks(2);

        let ranks = ledger
            .burn_leader_ranks_by_block_hash(&early_blocks)
            .unwrap();

        assert_eq!(ranks.len(), 3);
        assert_eq!(ranks, expected);
        let mut foreign = early_blocks[0].clone();
        foreign.hash = "a-block-from-another-branch".to_string();
        assert!(
            ledger
                .burn_leader_ranks_by_block_hash(&[foreign])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn snapshot_replay_skips_draws_that_would_be_evicted() {
        let (ledger, _) = mined_ledger("replay-ticket-draw-horizon", 4);
        let mut replayed = Ledger::from_preverified_snapshot(ledger.snapshot()).unwrap();
        assert_eq!(
            replayed.resident_ticket_draws.keys().collect::<Vec<_>>(),
            ledger.resident_ticket_draws.keys().collect::<Vec<_>>()
        );

        replayed.set_ticket_draw_horizon(4 + RESIDENT_TICKET_DRAW_BLOCKS);
        let next = replayed.chain[1].clone();
        assert!(replayed.ticket_draw_before(&next).is_none());
    }

    #[test]
    fn pruned_ledger_never_returns_ranges_with_a_gap() {
        let (mut ledger, _) = mined_ledger("pruned-block-ranges", 6);
        ledger.retain_recent_chain_blocks(3);
        let heights =
            |blocks: Vec<Block>| blocks.iter().map(|block| block.height).collect::<Vec<_>>();

        assert_eq!(ledger.first_contiguous_resident_height(), 4);
        assert!(ledger.blocks_from(0, 10).is_empty());
        assert!(ledger.blocks_from(3, 10).is_empty());
        assert_eq!(heights(ledger.blocks_from(5, 10)), vec![5, 6]);
        assert_eq!(heights(ledger.recent_blocks(10)), vec![6, 5, 4]);
        assert_eq!(heights(ledger.blocks_before(6, 10)), vec![5, 4]);
        assert_eq!(
            ledger.resident_block_at_height(5).map(|block| block.height),
            Some(5)
        );
        assert!(ledger.resident_block_at_height(3).is_none());
        assert_eq!(
            ledger.resident_block_at_height(0).map(|block| block.height),
            Some(0)
        );
    }

    #[test]
    fn restoring_history_replays_blocks_newer_than_the_stored_copy() {
        let (mut ledger, wallet) = mined_ledger("restore-pruned-history", 5);
        let stored = ledger.clone();
        ledger.retain_recent_chain_blocks(2);
        mine_block(&mut ledger, &wallet);
        let tip = ledger.tip_hash().to_string();

        assert!(ledger.restore_pruned_history(stored).unwrap());

        assert!(!ledger.history_is_pruned());
        assert_eq!(ledger.chain.len(), 7);
        assert_eq!(ledger.tip_hash(), tip);
        assert!(
            ledger
                .chain
                .windows(2)
                .all(|pair| pair[1].prev_hash == pair[0].hash)
        );
    }

    #[test]
    fn restoring_history_rejects_a_copy_from_another_branch() {
        let (mut ledger, wallet) = mined_ledger("restore-other-branch", 4);
        let mut other_branch = ledger.clone();
        mine_block(&mut other_branch, &wallet);
        mine_block_after(&mut ledger, &wallet, 2_000);
        ledger.retain_recent_chain_blocks(2);

        assert!(!ledger.restore_pruned_history(other_branch).unwrap());
        assert!(ledger.history_is_pruned());
    }
}
