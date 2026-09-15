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
    Amount, BLOCK_MEDIAN_TIME_PAST_WINDOW, Block, BurnBundleSection, FinalityCheckpoint,
    FinalizerMode, Ledger, MAX_BLOCK_TIMESTAMP_FUTURE_DRIFT_MS,
    TRANSACTION_REPLAY_PROTECTION_ACTIVATION_HEIGHT, Transaction, insert_output_with_lineage,
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
        let certified_parent = self
            .block_certifies_parent(&block, reward_committee.len())
            .then(|| FinalityCheckpoint {
                height: self.tip().height,
                hash: self.tip().hash.clone(),
            });
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
            self.validate_transaction_anchor_for_block(tx, block.height)?;
            apply_transaction_with_lineage(
                tx,
                block.height,
                &mut utxos,
                &mut utxo_lineage,
                &mut lineage_values,
                &mut lineage_owners,
                Some(&signing_domain),
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
        self.mined_transaction_ids
            .extend(mined_signatures.iter().cloned());
        self.chain.push(block);
        self.update_mine_difficulty_cache_after_tip();
        if let Some(checkpoint) = certified_parent {
            self.objective_finality_checkpoint = Some(checkpoint);
        }
        let available = self.utxos.clone();
        let pending = std::mem::take(&mut self.pending);
        self.pending = pending
            .into_iter()
            .filter(|tx| {
                !mined_signatures.contains(tx.signature())
                    && transaction_inputs_available(tx, &available)
                    && self.validate_transaction_terms(tx).is_ok()
                    && self.validate_transaction_anchor_for_pending(tx).is_ok()
                    && tx
                        .verify_signature(&self.transaction_signing_domain_for_pending(tx))
                        .is_ok()
            })
            .collect();
        let orphans = std::mem::take(&mut self.orphans);
        self.orphans = orphans
            .into_iter()
            .filter(|tx| {
                !mined_signatures.contains(tx.signature())
                    && self.validate_transaction_terms(tx).is_ok()
                    && self.validate_transaction_anchor_for_pending(tx).is_ok()
                    && tx
                        .verify_signature(&self.transaction_signing_domain_for_pending(tx))
                        .is_ok()
            })
            .collect();
        self.refresh_pending_pool_byte_counters()?;
        self.promote_orphan_transactions()?;
        self.revalidate_pending_v2()?;
        self.vdf_rounds = self.next_vdf_rounds_after_tip();
        Ok(())
    }

    /// Rebuild derived state from a block that this node previously validated and persisted.
    /// This is deliberately private to snapshot restoration: network and newly produced blocks
    /// must always use one of the validating apply paths above.
    pub(super) fn apply_trusted_block_at(&mut self, block: Block) -> Result<()> {
        debug_assert!(self.pending.is_empty() && self.orphans.is_empty());

        let reward_committee = self.burn_committee_for_block(&block);
        let certified_parent = self
            .block_certifies_parent(&block, reward_committee.len())
            .then(|| FinalityCheckpoint {
                height: self.tip().height,
                hash: self.tip().hash.clone(),
            });
        for transaction in &block.transactions {
            apply_transaction_with_lineage(
                transaction,
                block.height,
                &mut self.utxos,
                &mut self.utxo_lineage,
                &mut self.lineage_values,
                &mut self.lineage_owners,
                None,
            )?;
        }

        let mined_signatures = block
            .transactions
            .iter()
            .map(|transaction| transaction.signature().to_string())
            .collect::<BTreeSet<_>>();
        let parent = self.tip().clone();
        apply_finalizer_ticket_effects(&parent, &block, &mut self.tickets)?;
        self.tickets
            .extend(tickets_created_by_block(&block, &self.launch_profile)?);
        credit_reward_outputs(&mut self.utxos, &block, &reward_committee)?;
        self.compact_block_context.append_trusted_block(&block)?;
        self.mined_transaction_ids.extend(mined_signatures);
        self.chain.push(block);
        self.update_mine_difficulty_cache_after_tip();
        if let Some(checkpoint) = certified_parent {
            self.objective_finality_checkpoint = Some(checkpoint);
        }
        self.vdf_rounds = self.next_vdf_rounds_after_tip();
        Ok(())
    }

    fn ensure_block_transactions_are_not_replays(&self, block: &Block) -> Result<()> {
        if block.height < TRANSACTION_REPLAY_PROTECTION_ACTIVATION_HEIGHT {
            return Ok(());
        }
        if block
            .transactions
            .iter()
            .any(|transaction| self.mined_transaction_ids.contains(transaction.signature()))
        {
            bail!("block replays a previously mined transaction");
        }
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
            return Err(super::ValidationError::BlockConflictsWithLocalChain {
                height: block.height,
            }
            .into());
        }

        let expected_height = self.tip().height + 1;
        if block.height != expected_height {
            return Err(super::ValidationError::UnexpectedBlockHeight {
                expected: expected_height,
                actual: block.height,
            }
            .into());
        }
        if block.prev_hash != self.tip().hash {
            return Err(super::ValidationError::BlockDoesNotExtendLocalTip.into());
        }
        if block.compute_hash() != block.hash {
            bail!("block hash is invalid");
        }
        self.ensure_block_transactions_are_not_replays(block)?;
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
                return Err(super::ValidationError::BlockBeforeFinalizerRankSlot {
                    rank: block.finalizer_rank,
                    min_timestamp,
                }
                .into());
            }
        }
        let median_time_past = self.median_time_past();
        if block.timestamp_ms <= median_time_past {
            bail!("block timestamp must exceed median time past");
        }
        let max_future_timestamp = now_ms.saturating_add(MAX_BLOCK_TIMESTAMP_FUTURE_DRIFT_MS);
        if block.timestamp_ms > max_future_timestamp {
            return Err(super::ValidationError::BlockTimestampTooFarInFuture.into());
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
    signing_domain: Option<&super::TransactionSigningDomain>,
) -> Result<()> {
    if let Some(signing_domain) = signing_domain {
        transaction.verify_signature(signing_domain)?;
    }
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
    use crate::domain::{BurnTicket, GenesisBurn, MICRO_IUNA, Wallet, run_vdf};

    fn mine_transaction(signature: &str) -> Transaction {
        Transaction::Mine {
            recipient: "1".repeat(64),
            anchor: "2".repeat(64),
            salt: 1,
            nonce: 1,
            difficulty_bits: 10,
            proof_header: None,
            signature: signature.to_string(),
        }
    }

    #[test]
    fn historical_mine_replay_is_rejected_from_height_1000() {
        let signature = "3".repeat(64);
        let mut ledger = Ledger::new(BTreeMap::new(), 1);
        ledger.mined_transaction_ids.insert(signature.clone());
        let mut block = ledger.tip().clone();
        block.transactions = vec![mine_transaction(&signature)];

        block.height = TRANSACTION_REPLAY_PROTECTION_ACTIVATION_HEIGHT - 1;
        ledger
            .ensure_block_transactions_are_not_replays(&block)
            .unwrap();

        block.height = TRANSACTION_REPLAY_PROTECTION_ACTIVATION_HEIGHT;
        assert!(
            ledger
                .ensure_block_transactions_are_not_replays(&block)
                .unwrap_err()
                .to_string()
                .contains("replays a previously mined transaction")
        );
    }

    #[test]
    fn activated_block_validation_rejects_replayed_mine_proof() {
        let wallet = Wallet::from_seed("activated-mine-replay-wallet");
        let mut ledger = Ledger::new_with_genesis_burns(
            BTreeMap::from([(wallet.address().to_string(), 10 * MICRO_IUNA)]),
            vec![GenesisBurn::new(wallet.address(), MICRO_IUNA)],
            1,
        )
        .unwrap();
        ledger.chain.last_mut().unwrap().height =
            TRANSACTION_REPLAY_PROTECTION_ACTIVATION_HEIGHT - 1;
        ledger.tickets = vec![BurnTicket {
            id: "5".repeat(64),
            owner: wallet.address().to_string(),
            amount: MICRO_IUNA,
            eligible_from_height: TRANSACTION_REPLAY_PROTECTION_ACTIVATION_HEIGHT,
            eligible_until_height: TRANSACTION_REPLAY_PROTECTION_ACTIVATION_HEIGHT,
        }];

        let burn = ledger.build_burn_for_next_block(&wallet, 1, 1).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let mine = ledger.build_mine(wallet.address()).unwrap();
        let replayed_id = mine.signature().to_string();
        ledger.submit_transaction(mine).unwrap();
        let prepared = ledger.prepare_next_block(wallet.address(), 1).unwrap();
        let block = prepared.finish(&wallet, "test-vdf-output".to_string());
        assert_eq!(
            block.height,
            TRANSACTION_REPLAY_PROTECTION_ACTIVATION_HEIGHT
        );

        ledger.mined_transaction_ids.insert(replayed_id);
        let error = ledger
            .apply_preverified_block_at(block, 1)
            .unwrap_err()
            .to_string();

        assert!(error.contains("replays a previously mined transaction"));
    }

    #[test]
    fn queued_burn_survives_one_applied_block_and_is_included_in_the_following_block() {
        let finalizer = Wallet::from_seed("queued-burn-finalizer");
        let burner = Wallet::from_seed("queued-burn-wallet");
        let mut ledger = Ledger::new_with_genesis_burns(
            BTreeMap::from([
                (finalizer.address().to_string(), 10 * MICRO_IUNA),
                (burner.address().to_string(), 10 * MICRO_IUNA),
            ]),
            vec![GenesisBurn::new(finalizer.address(), MICRO_IUNA)],
            1,
        )
        .unwrap();
        ledger.chain.last_mut().unwrap().height =
            super::super::TIP_BOUND_BURN_ACTIVATION_HEIGHT - 2;
        ledger.tickets = vec![BurnTicket {
            id: "6".repeat(64),
            owner: finalizer.address().to_string(),
            amount: MICRO_IUNA,
            eligible_from_height: super::super::TIP_BOUND_BURN_ACTIVATION_HEIGHT - 1,
            eligible_until_height: super::super::TIP_BOUND_BURN_ACTIVATION_HEIGHT - 1,
        }];

        let queued_burn = ledger.build_burn(&burner, 1, 1).unwrap();
        assert_eq!(queued_burn.burn_anchor(), Some(ledger.tip_hash()));
        ledger.submit_transaction(queued_burn.clone()).unwrap();
        let legacy_anchor = ledger.build_burn_for_next_block(&finalizer, 1, 1).unwrap();
        assert_eq!(legacy_anchor.burn_anchor(), None);
        ledger.submit_transaction(legacy_anchor).unwrap();

        let prepared = ledger.prepare_next_block(finalizer.address(), 2).unwrap();
        let first_block = prepared.finish(&finalizer, "first-vdf-output".to_string());
        assert!(
            first_block
                .transactions
                .iter()
                .all(|transaction| transaction.signature() != queued_burn.signature())
        );
        ledger
            .apply_preverified_block_at(first_block, u64::MAX)
            .unwrap();

        assert!(
            ledger
                .pending()
                .iter()
                .any(|transaction| transaction.signature() == queued_burn.signature())
        );
        assert!(ledger.transaction_is_eligible_for_next_block(&queued_burn));

        ledger.tickets.push(BurnTicket {
            id: "7".repeat(64),
            owner: finalizer.address().to_string(),
            amount: MICRO_IUNA,
            eligible_from_height: super::super::TIP_BOUND_BURN_ACTIVATION_HEIGHT,
            eligible_until_height: super::super::TIP_BOUND_BURN_ACTIVATION_HEIGHT,
        });
        let activated_anchor = ledger.build_burn_for_next_block(&finalizer, 1, 1).unwrap();
        assert_eq!(
            activated_anchor.burn_anchor(),
            Some(ledger.tip().prev_hash.as_str())
        );
        ledger.submit_transaction(activated_anchor).unwrap();

        let prepared = ledger.prepare_next_block(finalizer.address(), 3).unwrap();
        let activated_block = prepared.finish(&finalizer, "second-vdf-output".to_string());
        assert_eq!(
            activated_block.height,
            super::super::TIP_BOUND_BURN_ACTIVATION_HEIGHT
        );
        assert!(
            activated_block
                .transactions
                .iter()
                .any(|transaction| transaction.signature() == queued_burn.signature())
        );
        ledger
            .apply_preverified_block_at(activated_block, u64::MAX)
            .unwrap();
        assert!(
            ledger
                .pending()
                .iter()
                .all(|transaction| transaction.signature() != queued_burn.signature())
        );
    }

    #[test]
    fn applied_blocks_and_snapshot_restore_index_mined_transaction_ids() {
        let wallet = Wallet::from_seed("replay-index-genesis-wallet");
        let mut ledger = Ledger::new_with_genesis_burns(
            BTreeMap::from([(wallet.address().to_string(), 10 * MICRO_IUNA)]),
            vec![GenesisBurn::new(wallet.address(), MICRO_IUNA)],
            1,
        )
        .unwrap();
        let genesis_transaction_id = ledger.chain[0].transactions[0].signature().to_string();
        let burn = ledger.build_burn(&wallet, 1, 1).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let mine = ledger.build_mine(wallet.address()).unwrap();
        let mine_transaction_id = mine.signature().to_string();
        ledger.submit_transaction(mine).unwrap();
        let prepared = ledger.prepare_next_block(wallet.address(), 1).unwrap();
        let vdf_output = run_vdf(prepared.vdf_seed(), prepared.vdf_rounds());
        let block = prepared.finish(&wallet, vdf_output);
        ledger.apply_preverified_block_at(block, 1).unwrap();

        assert!(
            ledger
                .mined_transaction_ids
                .contains(&genesis_transaction_id)
        );
        assert!(ledger.mined_transaction_ids.contains(&mine_transaction_id));
        let restored = Ledger::from_persisted_snapshot(ledger.snapshot()).unwrap();
        assert!(
            restored
                .mined_transaction_ids
                .contains(&genesis_transaction_id)
        );
        assert!(
            restored
                .mined_transaction_ids
                .contains(&mine_transaction_id)
        );
        assert!(restored.has_transaction(&genesis_transaction_id));
        assert!(restored.has_transaction(&mine_transaction_id));
    }

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
