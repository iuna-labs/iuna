use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};

use super::ledger_ops::{
    block_reward, credit_reward_outputs, ensure_block_has_burn, ensure_outputs_do_not_overflow,
    ensure_single_input_owner, ensure_valid_recovery_block, validate_block_fee_policy,
    verify_leader_proof,
};
use super::mine_policy::ensure_mine_anchor_limit;
use super::ticket::{
    apply_finalizer_ticket_effects, ticket_block_min_timestamp, tickets_created_by_block,
};
use super::transaction::transaction_inputs_available;
use super::{
    Amount, BLOCK_MEDIAN_TIME_PAST_WINDOW, Block, BurnBundleSection, FinalizerMode, Ledger,
    MAX_BLOCK_TIMESTAMP_FUTURE_DRIFT_MS, Transaction, insert_output_with_lineage,
    output_lineage_root_for_transaction, spend_inputs_with_lineage, unix_now_ms, verify_vdf,
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

        let reward_committee = self.burn_committee_for_block(&block);
        let mut utxos = self.utxos.clone();
        let mut utxo_lineage = self.utxo_lineage.clone();
        let mut lineage_values = self.lineage_values.clone();
        let mut lineage_owners = self.lineage_owners.clone();
        let signing_domain = self.transaction_signing_domain_at(block.height);
        let mut signatures = BTreeSet::new();
        for tx in &block.transactions {
            if !signatures.insert(tx.signature()) {
                bail!("duplicate transaction in block");
            }
            self.validate_transaction_terms(tx)?;
            apply_transaction_with_lineage(
                tx,
                block.height,
                &mut utxos,
                &mut utxo_lineage,
                &mut lineage_values,
                &mut lineage_owners,
                &signing_domain,
            )?;
        }
        let expected_reward = block_reward(&block.transactions, 0)?;
        if block.reward != expected_reward {
            bail!("block reward is invalid");
        }
        let mined_signatures = block
            .transactions
            .iter()
            .map(|tx| tx.signature().to_string())
            .collect::<BTreeSet<_>>();
        let mut tickets = self.tickets.clone();
        apply_finalizer_ticket_effects(self.tip(), &block, &mut tickets)?;
        tickets.extend(tickets_created_by_block(&block, &self.launch_profile)?);
        credit_reward_outputs(&mut utxos, &block, &reward_committee)?;
        self.compact_block_context.append_block(&block)?;
        self.utxos = utxos;
        self.utxo_lineage = utxo_lineage;
        self.lineage_values = lineage_values;
        self.lineage_owners = lineage_owners;
        self.tickets = tickets;
        self.chain.push(block);
        let next_signing_domain = self.transaction_signing_domain();
        let available = self.utxos.clone();
        let pending = std::mem::take(&mut self.pending);
        self.pending = pending
            .into_iter()
            .filter(|tx| {
                !mined_signatures.contains(tx.signature())
                    && transaction_inputs_available(tx, &available)
                    && self.validate_transaction_terms(tx).is_ok()
                    && tx.verify_signature(&next_signing_domain).is_ok()
            })
            .collect();
        let orphans = std::mem::take(&mut self.orphans);
        self.orphans = orphans
            .into_iter()
            .filter(|tx| {
                !mined_signatures.contains(tx.signature())
                    && self.validate_transaction_terms(tx).is_ok()
                    && tx.verify_signature(&next_signing_domain).is_ok()
            })
            .collect();
        self.refresh_pending_pool_byte_counters()?;
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
        if block.reward != self.expected_reward_for_block(block)? {
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
        if self.consensus_block_size_bytes(block)? > self.launch_profile.max_block_bytes {
            bail!("block exceeds max block size");
        }
        ensure_mine_anchor_limit(block.height, &block.transactions)?;
        ensure_block_has_burn(&block.transactions)?;
        validate_block_fee_policy(block)?;
        self.validate_burn_bundle_section_for_block(block)?;
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
        _burn_bundle_section: &BurnBundleSection,
    ) -> Result<Amount> {
        block_reward(transactions, 0)
    }

    fn expected_reward_for_block(&self, block: &Block) -> Result<Amount> {
        block_reward(&block.transactions, 0)
    }
}

fn apply_transaction_with_lineage(
    transaction: &Transaction,
    block_height: u64,
    utxos: &mut std::collections::BTreeMap<super::OutPoint, super::TxOutput>,
    utxo_lineage: &mut std::collections::BTreeMap<super::OutPoint, super::UtxoLineageRoot>,
    lineage_values: &mut std::collections::BTreeMap<super::UtxoLineageRoot, Amount>,
    lineage_owners: &mut super::LineageOwnerValues,
    signing_domain: &super::TransactionSigningDomain,
) -> Result<()> {
    transaction.verify_signature(signing_domain)?;
    if matches!(transaction, Transaction::Mine { .. }) {
        let output = transaction.outputs().remove(0);
        ensure_outputs_do_not_overflow(utxos, std::slice::from_ref(&output))?;
        insert_output_with_lineage(
            super::OutPoint {
                txid: transaction.signature().to_string(),
                index: 0,
            },
            output,
            output_lineage_root_for_transaction(transaction, block_height, None),
            utxos,
            utxo_lineage,
            lineage_values,
            lineage_owners,
        )?;
        return Ok(());
    }

    ensure_single_input_owner(transaction)?;
    let (input_total, inherited_root) = spend_inputs_with_lineage(
        transaction,
        utxos,
        utxo_lineage,
        lineage_values,
        lineage_owners,
    )?;
    let outputs = transaction.outputs();
    let output_total = outputs.iter().try_fold(0_u64, |total, output| {
        total
            .checked_add(output.amount)
            .context("transaction outputs overflow")
    })?;
    let required = output_total
        .checked_add(transaction.fee())
        .context("transaction outputs plus fee overflow")?
        .checked_add(match transaction {
            Transaction::Burn { amount, .. } => *amount,
            Transaction::Transfer { .. } | Transaction::Mine { .. } => 0,
        })
        .context("transaction outputs plus burn overflow")?;
    if input_total != required {
        bail!("transaction inputs do not balance outputs, burn, and fee");
    }
    ensure_outputs_do_not_overflow(utxos, &outputs)?;
    for (index, output) in outputs.into_iter().enumerate() {
        insert_output_with_lineage(
            super::OutPoint {
                txid: transaction.signature().to_string(),
                index: index as u32,
            },
            output,
            inherited_root.clone(),
            utxos,
            utxo_lineage,
            lineage_values,
            lineage_owners,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn median_time_past_uses_the_median_of_the_latest_eleven_blocks() {
        let mut ledger = Ledger::new(BTreeMap::new(), 1);
        let template = ledger.tip().clone();
        let timestamps = [5, 500, 20, 400, 30, 300, 40, 200, 50, 100, 60, 1_000];
        ledger.chain = timestamps
            .into_iter()
            .enumerate()
            .map(|(index, timestamp_ms)| {
                let mut block = template.clone();
                block.height = index as u64;
                block.timestamp_ms = timestamp_ms;
                block.hash = format!("{:064x}", index + 1);
                block
            })
            .collect();

        // The oldest value (5) falls outside the 11-block window. The sorted
        // window is 20,30,40,50,60,100,200,300,400,500,1000.
        assert_eq!(ledger.median_time_past(), 100);
    }
}
