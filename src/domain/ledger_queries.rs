use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};

use super::blinded::{
    ActiveBlindedTransaction, blinded_envelope_fee_for_transaction, decrypt_blinded_transaction,
};
use super::genesis::balances_from_utxos;
use super::mine_policy::mine_anchor;
use super::ticket::{
    BurnTicket, apply_finalizer_ticket_effects, genesis_tickets, ranked_tickets_for_height,
    tickets_created_by_block, tickets_created_by_transactions,
};
use super::{
    Amount, BlindedReveal, BlindedTransaction, Block, BurnLeaderRank, ChainSnapshot, ChainStatus,
    LaunchProfile, Ledger, OutPoint, RevealCommitteeMember, RevealedBlindedTransaction,
    Transaction, TxOutput, UNIQUE_OWNER_REVEAL_COMMITTEE_HEIGHT, reveal_committee_slot_count,
    reveal_committee_slot_count_for_height,
};

fn apply_historical_ticket_block(
    parent: &Block,
    block: &Block,
    launch_profile: &LaunchProfile,
    tickets: &mut Vec<BurnTicket>,
    active_blinded: &mut BTreeMap<String, ActiveBlindedTransaction>,
) -> Result<()> {
    apply_finalizer_ticket_effects(parent, block, tickets)?;
    tickets.extend(tickets_created_by_block(block, launch_profile)?);
    let mut revealed_transactions = Vec::new();
    for reveal in block.all_blinded_reveals() {
        let active = active_blinded.get(&reveal.commitment).with_context(|| {
            format!(
                "block {} reveals unknown blinded transaction {}",
                block.height, reveal.commitment
            )
        })?;
        let transaction = decrypt_blinded_transaction(&active.transaction, reveal)?;
        if matches!(transaction, Transaction::Mine { .. }) {
            bail!("mine actions are public and cannot be blinded");
        }
        if blinded_envelope_fee_for_transaction(&transaction) != active.transaction.fee {
            bail!(
                "block {} blinded reveal fee does not match envelope",
                block.height
            );
        }
        revealed_transactions.push(transaction);
        active_blinded.remove(&reveal.commitment);
    }
    tickets.extend(tickets_created_by_transactions(
        block.height,
        &revealed_transactions,
        launch_profile,
    )?);
    active_blinded.retain(|_, active| block.height < active.transaction.expires_at_height);
    for transaction in &block.blinded_transactions {
        active_blinded.insert(
            transaction.commitment.clone(),
            ActiveBlindedTransaction {
                transaction: transaction.clone(),
                locked_outputs: Vec::new(),
                included_height: block.height,
                included_by: block.miner.clone(),
            },
        );
    }
    Ok(())
}

impl Ledger {
    pub fn snapshot(&self) -> ChainSnapshot {
        ChainSnapshot {
            genesis_allocations: self.genesis_allocations.clone(),
            vdf_rounds: self.initial_vdf_rounds,
            launch_profile: self.launch_profile.clone(),
            blocks: self.chain.clone(),
        }
    }

    pub fn status(&self) -> ChainStatus {
        self.status_with_balances(true)
    }

    pub fn light_status(&self) -> ChainStatus {
        self.status_with_balances(false)
    }

    fn status_with_balances(&self, include_balances: bool) -> ChainStatus {
        ChainStatus {
            height: self.tip().height,
            tip_hash: self.tip().hash.clone(),
            next_leader: self.expected_leader_for_next_block(),
            launch_profile_hash: self.launch_profile.hash(),
            mine_reward: self.mine_reward,
            current_mine_difficulty_bits: self.current_mine_difficulty_bits(),
            balances: include_balances
                .then(|| balances_from_utxos(&self.utxos))
                .unwrap_or_default(),
            pending_transactions: self.pending.len()
                + self.pending_blinded.len()
                + self.pending_reveals.len(),
        }
    }

    pub fn tip_hash(&self) -> &str {
        &self.tip().hash
    }

    pub fn chain(&self) -> &[Block] {
        &self.chain
    }

    pub fn burn_leader_ranks_for_block(&self, height: u64) -> Result<Vec<BurnLeaderRank>> {
        Ok(self
            .burn_leader_ranks_for_blocks([height])?
            .remove(&height)
            .unwrap_or_default())
    }

    pub fn burn_leader_ranks_for_blocks<I>(
        &self,
        heights: I,
    ) -> Result<BTreeMap<u64, Vec<BurnLeaderRank>>>
    where
        I: IntoIterator<Item = u64>,
    {
        let mut requested = heights.into_iter().collect::<BTreeSet<_>>();
        let mut ranks_by_height = BTreeMap::new();
        if requested.remove(&0) {
            ranks_by_height.insert(0, Vec::new());
        }
        if requested.is_empty() {
            return Ok(ranks_by_height);
        }

        let mut tickets = genesis_tickets(
            &self.genesis_allocations,
            &self.chain[0],
            &self.launch_profile,
        )?;
        let mut active_blinded = BTreeMap::<String, ActiveBlindedTransaction>::new();
        let mut next_block_index = 1;

        for height in requested {
            let parent_index = height.checked_sub(1).context("block height underflows")? as usize;
            let parent = self
                .chain
                .get(parent_index)
                .with_context(|| format!("missing parent block for height {height}"))?;
            while let Some(block) = self.chain.get(next_block_index) {
                if block.height >= height {
                    break;
                }
                let block_parent = self
                    .chain
                    .get(next_block_index - 1)
                    .with_context(|| format!("missing parent block for height {}", block.height))?;
                apply_historical_ticket_block(
                    block_parent,
                    block,
                    &self.launch_profile,
                    &mut tickets,
                    &mut active_blinded,
                )?;
                next_block_index += 1;
            }

            ranks_by_height.insert(
                height,
                ranked_tickets_for_height(parent, height, &tickets)
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
            );
        }

        Ok(ranks_by_height)
    }

    pub fn reveal_committee_for_next_block(&self) -> Vec<RevealCommitteeMember> {
        self.reveal_committee_for_height(self.tip().height + 1)
    }

    pub fn reveal_committee_for_height(&self, height: u64) -> Vec<RevealCommitteeMember> {
        let ranked = ranked_tickets_for_height(self.tip(), height, &self.tickets);
        let selected = if height < UNIQUE_OWNER_REVEAL_COMMITTEE_HEIGHT {
            let mut selected = Vec::new();
            if !ranked.is_empty() {
                selected.push(0);
            }
            for index in (0..ranked.len()).rev() {
                if selected.len() >= reveal_committee_slot_count(ranked.len()) {
                    break;
                }
                if !selected.contains(&index) {
                    selected.push(index);
                }
            }
            selected
        } else {
            let target_slots = reveal_committee_slot_count_for_height(
                height,
                ranked.len(),
                ranked.iter().map(|ticket| ticket.owner.as_str()),
            );
            let mut seen_owners = BTreeSet::new();
            let mut selected = Vec::new();
            for (index, ticket) in ranked.iter().enumerate() {
                if selected.len() >= target_slots {
                    break;
                }
                if seen_owners.insert(ticket.owner.clone()) {
                    selected.push(index);
                }
            }
            selected
        };
        selected
            .into_iter()
            .enumerate()
            .filter_map(|(slot, rank)| {
                let ticket = ranked.get(rank)?.clone();
                Some(RevealCommitteeMember {
                    slot: u8::try_from(slot).ok()?,
                    rank: u32::try_from(rank).ok()?,
                    ticket_id: ticket.id,
                    owner: ticket.owner,
                    amount: ticket.amount,
                })
            })
            .collect()
    }

    pub fn genesis_hash(&self) -> &str {
        &self.chain[0].hash
    }

    pub fn is_setup_placeholder(&self) -> bool {
        self.height() == 0
            && self.genesis_allocations.is_empty()
            && self.chain[0].transactions.is_empty()
            && self.pending.is_empty()
    }

    pub fn height(&self) -> u64 {
        self.tip().height
    }

    pub fn recent_blocks(&self, limit: usize) -> Vec<Block> {
        self.chain.iter().rev().take(limit).cloned().collect()
    }

    pub fn blocks_before(&self, before_height: u64, limit: usize) -> Vec<Block> {
        self.chain
            .iter()
            .rev()
            .filter(|block| block.height < before_height)
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn blocks_from(&self, from_height: u64, limit: usize) -> Vec<Block> {
        if limit == 0 {
            return Vec::new();
        }
        self.chain
            .iter()
            .filter(|block| block.height >= from_height)
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn block_by_hash(&self, hash: &str) -> Option<Block> {
        self.chain.iter().find(|block| block.hash == hash).cloned()
    }

    pub fn has_block(&self, hash: &str) -> bool {
        self.chain.iter().any(|block| block.hash == hash)
    }

    pub fn pending(&self) -> &[Transaction] {
        &self.pending
    }

    pub fn pending_blinded_transactions(&self) -> &[BlindedTransaction] {
        &self.pending_blinded
    }

    pub fn pending_blinded_reveals(&self) -> &[BlindedReveal] {
        &self.pending_reveals
    }

    pub fn pending_revealed_blinded_transactions(&self) -> Vec<RevealedBlindedTransaction> {
        self.pending_reveals
            .iter()
            .filter_map(|reveal| {
                let active = self.active_blinded.get(&reveal.commitment)?;
                let transaction = self.pending_reveal_transaction(reveal).ok()?;
                Some(RevealedBlindedTransaction {
                    height: self.height().saturating_add(1),
                    commitment: reveal.commitment.clone(),
                    included_by: active.included_by.clone(),
                    transaction,
                })
            })
            .collect()
    }

    pub(crate) fn drop_pending_blinded_conflicting_with_transaction(
        &mut self,
        transaction: &Transaction,
    ) {
        let spent = transaction
            .inputs()
            .iter()
            .map(|input| input.outpoint.clone())
            .collect::<BTreeSet<_>>();
        self.pending_blinded.retain(|blinded| {
            !blinded
                .inputs
                .iter()
                .any(|input| spent.contains(&input.outpoint))
        });
    }

    pub(crate) fn clear_pending_blinded_transactions(&mut self) {
        self.pending_blinded.clear();
    }

    pub(crate) fn clear_pending_transactions(&mut self) {
        self.pending.clear();
    }

    pub fn orphan_transactions(&self) -> &[Transaction] {
        &self.orphans
    }

    pub fn transaction_by_signature(&self, signature: &str) -> Option<Transaction> {
        self.pending
            .iter()
            .chain(self.orphans.iter())
            .chain(
                self.chain
                    .iter()
                    .flat_map(|block| block.transactions.iter()),
            )
            .find(|tx| tx.signature() == signature)
            .cloned()
    }

    pub fn has_transaction(&self, signature: &str) -> bool {
        self.transaction_by_signature(signature).is_some()
    }

    pub fn pending_mine_count_for_anchor(&self, anchor: &str) -> usize {
        self.pending
            .iter()
            .filter(|tx| mine_anchor(tx) == Some(anchor))
            .count()
    }

    pub fn has_blinded_transaction(&self, commitment: &str) -> bool {
        self.pending_blinded
            .iter()
            .any(|transaction| transaction.commitment == commitment)
            || self.active_blinded.contains_key(commitment)
            || self.chain.iter().any(|block| {
                block
                    .blinded_transactions
                    .iter()
                    .any(|tx| tx.commitment == commitment)
            })
    }

    pub fn has_unrevealed_blinded_transaction(&self, commitment: &str) -> bool {
        self.pending_blinded
            .iter()
            .any(|transaction| transaction.commitment == commitment)
            || self.active_blinded.contains_key(commitment)
    }

    pub fn has_active_blinded_transaction(&self, commitment: &str) -> bool {
        self.active_blinded.contains_key(commitment)
    }

    pub fn has_blinded_reveal(&self, commitment: &str) -> bool {
        self.pending_reveals
            .iter()
            .any(|reveal| reveal.commitment == commitment)
            || self.chain.iter().any(|block| {
                block
                    .all_blinded_reveals()
                    .iter()
                    .any(|reveal| reveal.commitment == commitment)
            })
    }

    pub fn vdf_rounds(&self) -> u64 {
        self.vdf_rounds
    }

    pub fn launch_profile(&self) -> &LaunchProfile {
        &self.launch_profile
    }

    pub fn current_mine_difficulty_bits(&self) -> u32 {
        self.mine_difficulty_bits_for_anchor_height(self.tip().height)
    }

    pub fn mine_difficulty_bits_at_height(&self, height: u64) -> u32 {
        self.mine_difficulty_bits_for_anchor_height(height.min(self.tip().height))
    }

    pub fn balance_of(&self, address: &str) -> Amount {
        self.utxos
            .values()
            .filter(|output| output.address == address)
            .map(|output| output.amount)
            .sum()
    }

    pub fn utxos_for_address(&self, address: &str) -> Vec<(OutPoint, TxOutput)> {
        self.utxos
            .iter()
            .filter(|(_, output)| output.address == address)
            .map(|(outpoint, output)| (outpoint.clone(), output.clone()))
            .collect()
    }

    pub fn all_utxos(&self) -> Vec<(OutPoint, TxOutput)> {
        self.utxos
            .iter()
            .map(|(outpoint, output)| (outpoint.clone(), output.clone()))
            .collect()
    }

    pub fn available_utxos_for_address(&self, address: &str) -> Result<Vec<(OutPoint, TxOutput)>> {
        Ok(self
            .utxos_after_spendable_pending()?
            .into_iter()
            .filter(|(_, output)| output.address == address)
            .collect())
    }

    pub fn next_nonce(&self, address: &str) -> u64 {
        self.utxos
            .keys()
            .chain(
                self.pending
                    .iter()
                    .flat_map(|tx| tx.inputs().iter().map(|input| &input.outpoint)),
            )
            .filter(|outpoint| outpoint.txid.contains(address))
            .count() as u64
            + 1
    }
}
