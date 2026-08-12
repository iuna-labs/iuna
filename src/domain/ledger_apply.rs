use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};

use super::blinded::{
    ActiveBlindedTransaction, credit_blinded_fee_outputs, credit_expired_blinded_outputs,
};
use super::ledger_ops::{
    apply_transaction, block_reward, credit_reward_output, ensure_block_has_burn,
    ensure_valid_recovery_block, spend_blinded_inputs, validate_block_blinded_items,
    verify_leader_proof,
};
use super::mine_policy::ensure_mine_anchor_limit;
use super::ticket::{
    apply_finalizer_ticket_effects, ticket_block_min_timestamp, tickets_created_by_block,
    tickets_created_by_transactions,
};
use super::transaction::{blinded_transaction_inputs_available, transaction_inputs_available};
use super::{
    Amount, BLOCK_MEDIAN_TIME_PAST_WINDOW, Block, FinalizerMode, Ledger,
    MAX_BLOCK_TIMESTAMP_FUTURE_DRIFT_MS, RevealBundleSection, Transaction,
    blinded_reveal_finalizer_fee, unix_now_ms, verify_vdf,
};

impl Ledger {
    pub fn apply_block(&mut self, block: Block) -> Result<()> {
        self.apply_block_at(block, unix_now_ms())
    }

    pub(crate) fn apply_block_at(&mut self, block: Block, now_ms: u64) -> Result<()> {
        self.apply_block_with_vdf_policy(block, true, now_ms)
    }

    pub(crate) fn block_requires_vdf_verification_at(
        &self,
        block: &Block,
        now_ms: u64,
    ) -> Result<bool> {
        self.precheck_block_without_vdf_at(block, now_ms)
    }

    pub fn apply_locally_mined_block(&mut self, block: Block) -> Result<()> {
        self.apply_self_produced_block_at(block, unix_now_ms())
    }

    pub(crate) fn apply_self_produced_block_at(&mut self, block: Block, now_ms: u64) -> Result<()> {
        self.verify_self_produced_block_at(&block, now_ms)?;
        self.apply_preverified_block_at(block, now_ms)
    }

    pub(crate) fn verify_self_produced_block_at(&self, block: &Block, now_ms: u64) -> Result<()> {
        let mut verifier = self.clone();
        verifier.apply_preverified_block_at(block.clone(), now_ms)?;
        Ok(())
    }

    pub(crate) fn apply_preverified_block_at(&mut self, block: Block, now_ms: u64) -> Result<()> {
        self.apply_block_with_vdf_policy(block, false, now_ms)
    }

    fn apply_block_with_vdf_policy(
        &mut self,
        block: Block,
        should_verify_vdf: bool,
        now_ms: u64,
    ) -> Result<()> {
        if !self.precheck_block_without_vdf_at(&block, now_ms)? {
            return Ok(());
        }

        if should_verify_vdf && !verify_vdf(&block.vdf_seed(), block.vdf_rounds, &block.vdf_output)
        {
            bail!("block VDF output is invalid");
        }

        let reveal_bundle_slot_count = self.reveal_committee_for_height(block.height).len();
        let mut utxos = self.utxos.clone();
        let mut signatures = BTreeSet::new();
        let mut revealed_transactions = Vec::new();
        let mut aggregated_reveal_finalizer_fees = 0_u64;
        for tx in &block.transactions {
            if !signatures.insert(tx.signature()) {
                bail!("duplicate transaction in block");
            }
            self.validate_transaction_terms(tx)?;
            apply_transaction(tx, &mut utxos)?;
        }
        let mut revealed_commitments = BTreeSet::new();
        for reveal in block.all_blinded_reveals() {
            if !revealed_commitments.insert(reveal.commitment.clone()) {
                bail!("duplicate blinded reveal in block");
            }
            let active = self
                .active_blinded
                .get(&reveal.commitment)
                .context("blinded reveal does not reference an active blinded transaction")?
                .clone();
            let tx = self.decrypt_active_blinded(&active, reveal)?;
            self.apply_revealed_blinded_transaction(&active, &tx, &mut utxos)?;
            credit_blinded_fee_outputs(
                &mut utxos,
                &active,
                &block.miner,
                &tx,
                &block.reveal_bundle_section.signatures,
                reveal_bundle_slot_count,
                true,
            )?;
            aggregated_reveal_finalizer_fees = aggregated_reveal_finalizer_fees
                .checked_add(blinded_reveal_finalizer_fee(
                    tx.fee(),
                    block.included_reveal_bundle_count(),
                    reveal_bundle_slot_count,
                ))
                .context("aggregated reveal finalizer fees overflow")?;
            revealed_transactions.push(tx);
        }
        for (commitment, active) in &self.active_blinded {
            if !revealed_commitments.contains(commitment)
                && block.height >= active.transaction.expires_at_height
            {
                credit_expired_blinded_outputs(&mut utxos, active)?;
            }
        }
        let expected_reward = block_reward(&block.transactions, aggregated_reveal_finalizer_fees)?;
        if block.reward != expected_reward {
            bail!("block reward is invalid");
        }
        let mined_signatures = block
            .transactions
            .iter()
            .map(|tx| tx.signature().to_string())
            .collect::<BTreeSet<_>>();
        let included_blinded = block
            .blinded_transactions
            .iter()
            .map(|transaction| transaction.commitment.clone())
            .collect::<BTreeSet<_>>();
        let revealed_blinded = block
            .all_blinded_reveals()
            .into_iter()
            .map(|reveal| reveal.commitment.clone())
            .collect::<BTreeSet<_>>();
        let mut new_active_blinded = Vec::new();
        for transaction in &block.blinded_transactions {
            let locked_outputs = spend_blinded_inputs(transaction, &mut utxos)?;
            new_active_blinded.push((
                transaction.commitment.clone(),
                ActiveBlindedTransaction {
                    transaction: transaction.clone(),
                    locked_outputs,
                    included_height: block.height,
                    included_by: block.miner.clone(),
                },
            ));
        }
        let mut tickets = self.tickets.clone();
        apply_finalizer_ticket_effects(self.tip(), &block, &mut tickets)?;
        tickets.extend(tickets_created_by_block(&block, &self.launch_profile)?);
        tickets.extend(tickets_created_by_transactions(
            block.height,
            &revealed_transactions,
            &self.launch_profile,
        )?);
        credit_reward_output(&mut utxos, &block)?;
        self.utxos = utxos;
        self.tickets = tickets;
        self.chain.push(block);
        let new_height = self.height();
        self.active_blinded.retain(|commitment, active| {
            !revealed_blinded.contains(commitment)
                && new_height < active.transaction.expires_at_height
        });
        for (commitment, active) in new_active_blinded {
            self.active_blinded.insert(commitment, active);
        }
        let available = self.utxos.clone();
        let pending = std::mem::take(&mut self.pending);
        self.pending = pending
            .into_iter()
            .filter(|tx| {
                !mined_signatures.contains(tx.signature())
                    && transaction_inputs_available(tx, &available)
                    && self.validate_transaction_terms(tx).is_ok()
            })
            .collect();
        let orphans = std::mem::take(&mut self.orphans);
        self.orphans = orphans
            .into_iter()
            .filter(|tx| {
                !mined_signatures.contains(tx.signature())
                    && self.validate_transaction_terms(tx).is_ok()
            })
            .collect();
        let pending_blinded = std::mem::take(&mut self.pending_blinded);
        self.pending_blinded = pending_blinded
            .into_iter()
            .filter(|transaction| {
                !included_blinded.contains(&transaction.commitment)
                    && new_height < transaction.expires_at_height
                    && blinded_transaction_inputs_available(transaction, &available)
                    && self.validate_blinded_transaction(transaction).is_ok()
            })
            .collect();
        let pending_reveals = std::mem::take(&mut self.pending_reveals);
        self.pending_reveals = pending_reveals
            .into_iter()
            .filter(|reveal| {
                !revealed_blinded.contains(&reveal.commitment)
                    && self.pending_reveal_transaction(reveal).is_ok()
            })
            .collect();
        self.promote_orphan_transactions()?;
        self.vdf_rounds = self.next_vdf_rounds_after_tip();
        Ok(())
    }

    fn precheck_block_without_vdf_at(&self, block: &Block, now_ms: u64) -> Result<bool> {
        if block.height <= self.tip().height {
            let existing = self
                .chain
                .get(block.height as usize)
                .with_context(|| format!("local chain has no block at height {}", block.height))?;
            if existing.hash == block.hash {
                return Ok(false);
            }
            bail!(
                "block at height {} conflicts with local chain",
                block.height
            );
        }

        let expected_height = self.tip().height + 1;
        if block.height != expected_height {
            bail!(
                "expected block height {expected_height}, got {}",
                block.height
            );
        }
        if block.prev_hash != self.tip().hash {
            bail!("block does not extend local tip");
        }
        if block.compute_hash() != block.hash {
            bail!("block hash is invalid");
        }
        let reveal_bundle_slot_count = self.reveal_committee_for_height(block.height).len();
        if block.reward != self.expected_reward_for_block(block, reveal_bundle_slot_count)? {
            bail!("block reward is invalid");
        }
        let expected_vdf_rounds = self.expected_vdf_rounds_for_block(block)?;
        if block.vdf_rounds != expected_vdf_rounds {
            bail!("block VDF rounds are invalid");
        }
        if block.timestamp_ms <= self.tip().timestamp_ms {
            bail!("block timestamp must increase");
        }
        if block.finalizer_mode == FinalizerMode::Ticket {
            let min_timestamp = ticket_block_min_timestamp(self.tip(), block.finalizer_rank)?;
            if block.timestamp_ms < min_timestamp {
                bail!(
                    "block timestamp is before finalizer rank {} time slot {min_timestamp}",
                    block.finalizer_rank
                );
            }
        }
        let median_time_past = self.median_time_past();
        if block.timestamp_ms <= median_time_past {
            bail!("block timestamp must exceed median time past");
        }
        let max_future_timestamp = now_ms.saturating_add(MAX_BLOCK_TIMESTAMP_FUTURE_DRIFT_MS);
        if block.timestamp_ms > max_future_timestamp {
            bail!("block timestamp is too far in the future");
        }
        if block.transactions.len() > self.launch_profile.max_block_transactions {
            bail!("block has too many transactions");
        }
        let block_item_count = block.transactions.len()
            + block.blinded_transactions.len()
            + block.all_blinded_reveals().len();
        if block_item_count > self.launch_profile.max_block_transactions {
            bail!("block has too many transaction items");
        }
        if block.serialized_size_bytes()? > self.launch_profile.max_block_bytes {
            bail!("block exceeds max block size");
        }
        ensure_mine_anchor_limit(block.height, &block.transactions)?;
        ensure_block_has_burn(&block.transactions)?;
        self.validate_reveal_bundle_section_for_block(
            block.height,
            &block.prev_hash,
            &block.reveal_bundle_section,
        )?;
        validate_block_blinded_items(block, self)?;
        match block.finalizer_mode {
            FinalizerMode::Ticket => {
                let selected_ticket = self
                    .ticket_for_finalizer_rank(block.height, block.finalizer_rank)
                    .context("no selected ticket for block finalizer rank")?;
                if selected_ticket.owner != block.miner {
                    bail!(
                        "block finalizer {} is not selected for rank {}",
                        block.miner,
                        block.finalizer_rank
                    );
                }
                if block
                    .leader_proof
                    .as_ref()
                    .is_none_or(|proof| proof.ticket_id != selected_ticket.id)
                {
                    bail!("block does not prove the selected leader ticket");
                }
                verify_leader_proof(block, &self.tickets)?;
            }
            FinalizerMode::Recovery => {
                ensure_valid_recovery_block(block, self.tip())?;
            }
        }

        Ok(true)
    }

    fn median_time_past(&self) -> u64 {
        let mut timestamps = self
            .chain
            .iter()
            .rev()
            .take(BLOCK_MEDIAN_TIME_PAST_WINDOW)
            .map(|block| block.timestamp_ms)
            .collect::<Vec<_>>();
        timestamps.sort_unstable();
        timestamps[timestamps.len() / 2]
    }

    pub(super) fn expected_reward_for_next_block(
        &self,
        transactions: &[Transaction],
        reveal_bundle_section: &RevealBundleSection,
    ) -> Result<Amount> {
        let height = self.tip().height + 1;
        let reveal_bundle_slot_count = self.reveal_committee_for_height(height).len();
        let aggregate =
            self.aggregate_reveal_finalizer_fees(reveal_bundle_section, reveal_bundle_slot_count)?;
        block_reward(transactions, aggregate)
    }

    fn expected_reward_for_block(
        &self,
        block: &Block,
        reveal_bundle_slot_count: usize,
    ) -> Result<Amount> {
        let aggregate = self.aggregate_reveal_finalizer_fees(
            &block.reveal_bundle_section,
            reveal_bundle_slot_count,
        )?;
        block_reward(&block.transactions, aggregate)
    }

    fn aggregate_reveal_finalizer_fees(
        &self,
        reveal_bundle_section: &RevealBundleSection,
        reveal_bundle_slot_count: usize,
    ) -> Result<Amount> {
        reveal_bundle_section
            .all_reveals()
            .into_iter()
            .try_fold(0_u64, |total, reveal| {
                let active = self
                    .active_blinded
                    .get(&reveal.commitment)
                    .context("blinded reveal does not reference an active blinded transaction")?;
                total
                    .checked_add(blinded_reveal_finalizer_fee(
                        active.transaction.fee,
                        reveal_bundle_section.included_bundle_count(),
                        reveal_bundle_slot_count,
                    ))
                    .context("aggregated reveal finalizer fees overflow")
            })
    }
}
