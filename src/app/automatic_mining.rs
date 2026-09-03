use anyhow::{Context, Result, bail};

use super::helpers::{
    allowed_recovery_vdf_rank_count, converge_fee_by_byte, recovery_vdf_sample_percent,
};
use super::{
    AUTO_PLAINTEXT_BURN_BEFORE_RECOVERY_MS, AutoMineOutcome, AutoMinePlan,
    BURN_BUNDLE_COLLECTION_MS, GossipEnvelope, Ledger, MIN_AUTO_BLOCK_ANCHOR_BURN_AMOUNT, NodeCore,
    PreparedBlock, Transaction, run_vdf,
};
use crate::domain::{Amount, BurnCommitteeMember, FinalizerMode};

mod pow;

impl NodeCore {
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

        let pow_error = match self.prepare_automatic_pow_mining() {
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
        let will_run_ticket_vdf = wallet_rank.is_some_and(|rank| self.wallet_rank_runs_vdf(rank));
        let will_run_recovery_vdf =
            !will_run_ticket_vdf && self.should_prepare_recovery_vdf(timestamp_ms);
        if let Some(wait_ms) = self.burn_bundle_collection_wait_ms(
            timestamp_ms,
            will_run_ticket_vdf || will_run_recovery_vdf,
        ) {
            self.request_missing_burn_bundles_for_next_block(timestamp_ms);
            plan.skipped_reason = Some(format!(
                "collecting burns for next block ({:.1}s remaining)",
                wait_ms as f64 / 1000.0
            ));
            return plan;
        }
        if let Err(error) = self.publish_burn_bundle_for_next_block() {
            plan.skipped_reason = Some(format!("{error:#}"));
            return plan;
        }

        if will_run_recovery_vdf {
            match self.prepare_recovery_block_with_local_anchor(timestamp_ms) {
                Ok(work) => {
                    plan.work = Some(work);
                }
                Err(error) => {
                    plan.skipped_reason = Some(format!("{error:#}"));
                }
            }
            return plan;
        }

        if let Some(rank) = wallet_rank {
            if !self.wallet_rank_runs_vdf(rank) {
                plan.skipped_reason = Some(format!(
                    "wallet finalizer rank {rank} is outside the top {}% VDF threshold",
                    self.recovery_vdf_top_rank_percent
                ));
                return plan;
            }
        } else {
            let selected_leader = self.ledger.expected_leader_for_next_block();
            plan.skipped_reason = selected_leader.map(|leader| {
                format!("wallet is waiting for selected finalizer {leader} to finish the VDF")
            });
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

    pub fn prepare_automatic_finalization(&mut self, timestamp_ms: u64) -> AutoMinePlan {
        let mut plan = AutoMinePlan {
            pow_mined: None,
            burned: None,
            work: None,
            skipped_reason: None,
        };

        if self.wallet.is_locked() {
            plan.skipped_reason = Some("wallet is locked".to_string());
            self.last_auto_finalization_status = plan.skipped_reason.clone();
            return plan;
        }

        if !self.automatic_mining_enabled {
            if let Err(error) = self.publish_burn_bundle_for_next_block() {
                plan.skipped_reason = Some(format!("burn committee signing failed: {error:#}"));
                self.last_auto_finalization_status = plan.skipped_reason.clone();
                return plan;
            }
            plan.skipped_reason = Some("automatic mining is off".to_string());
            self.last_auto_finalization_status = plan.skipped_reason.clone();
            return plan;
        }

        match self.prepare_automatic_burn(timestamp_ms) {
            Ok(tx) => plan.burned = tx,
            Err(error) => {
                plan.skipped_reason = Some(format!("automatic burn failed: {error:#}"));
                self.last_auto_finalization_status = plan.skipped_reason.clone();
                return plan;
            }
        }

        let wallet_rank = self
            .ledger
            .finalizer_rank_for_next_block(self.wallet.address());
        let will_run_ticket_vdf = wallet_rank.is_some_and(|rank| self.wallet_rank_runs_vdf(rank));
        let will_run_recovery_vdf =
            !will_run_ticket_vdf && self.should_prepare_recovery_vdf(timestamp_ms);
        if let Some(wait_ms) = self.burn_bundle_collection_wait_ms(
            timestamp_ms,
            will_run_ticket_vdf || will_run_recovery_vdf,
        ) {
            self.request_missing_burn_bundles_for_next_block(timestamp_ms);
            plan.skipped_reason = Some(format!(
                "collecting burns for next block ({:.1}s remaining)",
                wait_ms as f64 / 1000.0
            ));
            self.last_auto_finalization_status = plan.skipped_reason.clone();
            return plan;
        }
        if let Err(error) = self.publish_burn_bundle_for_next_block() {
            plan.skipped_reason = Some(format!("{error:#}"));
            self.last_auto_finalization_status = plan.skipped_reason.clone();
            return plan;
        }

        if will_run_recovery_vdf {
            match self.prepare_recovery_block_with_local_anchor(timestamp_ms) {
                Ok(work) => {
                    self.last_auto_finalization_status = Some(format!(
                        "running recovery VDF for candidate block {} ({} rounds)",
                        work.height(),
                        work.vdf_rounds()
                    ));
                    plan.work = Some(work);
                }
                Err(error) => {
                    plan.skipped_reason = Some(format!("{error:#}"));
                    self.last_auto_finalization_status = plan.skipped_reason.clone();
                }
            }
            return plan;
        }

        if let Some(rank) = wallet_rank {
            if !self.wallet_rank_runs_vdf(rank) {
                plan.skipped_reason = Some(format!(
                    "wallet finalizer rank {rank} is outside the top {}% VDF threshold",
                    self.recovery_vdf_top_rank_percent
                ));
                self.last_auto_finalization_status = plan.skipped_reason.clone();
                return plan;
            }
        } else {
            let selected_leader = self.ledger.expected_leader_for_next_block();
            plan.skipped_reason = selected_leader.map(|leader| {
                format!("wallet is waiting for selected finalizer {leader} to finish the VDF")
            });
            self.last_auto_finalization_status = plan.skipped_reason.clone();
            return plan;
        }

        match self.prepare_next_block_with_local_anchor(timestamp_ms) {
            Ok(work) => {
                self.last_auto_finalization_status = Some(format!(
                    "running VDF for candidate block {} ({} rounds)",
                    work.height(),
                    work.vdf_rounds()
                ));
                plan.work = Some(work);
            }
            Err(error) => {
                plan.skipped_reason = Some(format!("{error:#}"));
                self.last_auto_finalization_status = plan.skipped_reason.clone();
            }
        }

        plan
    }

    pub(super) fn prepare_automatic_burn(
        &mut self,
        timestamp_ms: u64,
    ) -> Result<Option<Transaction>> {
        let current_height = self.ledger.height();
        if !self.automatic_mining_enabled {
            return Ok(None);
        }
        let anchor_burn = self.prepare_automatic_anchor_burn(timestamp_ms)?;
        if self.burn_per_block == 0 {
            self.last_auto_burn_height = Some(current_height);
            return Ok(anchor_burn);
        }
        if anchor_burn
            .as_ref()
            .is_some_and(|burn| burn.amount() >= self.burn_per_block)
        {
            self.last_auto_burn_height = Some(current_height);
            return Ok(anchor_burn);
        }
        if self.last_auto_burn_height == Some(current_height) {
            return Ok(anchor_burn);
        }

        let fee_per_byte = self.burn_fee;
        let balance = self.ledger.balance_of(self.wallet.address());
        let ledger = self.wallet_build_ledger()?;
        let best = self.best_automatic_burn_on_ledger(&ledger, fee_per_byte, balance);
        let Some(tx) = best else {
            self.last_auto_burn_height = Some(current_height);
            return Ok(anchor_burn);
        };
        let burn = tx.clone();
        self.submit_public_transaction(tx)?;
        self.last_auto_burn_height = Some(current_height);
        Ok(Some(burn))
    }

    fn prepare_automatic_anchor_burn(&mut self, timestamp_ms: u64) -> Result<Option<Transaction>> {
        let current_height = self.ledger.height();
        if !self.automatic_burn_needs_plaintext_anchor(timestamp_ms) {
            return Ok(None);
        }
        if self
            .local_block_anchor_burn
            .as_ref()
            .is_some_and(|(height, _)| *height == current_height)
        {
            return Ok(None);
        }
        if self.last_auto_anchor_burn_height == Some(current_height) {
            return Ok(None);
        }

        // Pending wallet burns can themselves satisfy the block-anchor requirement.
        // Pending transfers cannot, so keep their confirmed inputs reserved.
        let pending_transfer_spent_outpoints = self
            .ledger
            .pending()
            .iter()
            .filter_map(|transaction| match transaction {
                Transaction::Transfer { inputs, .. } => Some(inputs),
                Transaction::Burn { .. } | Transaction::Mine { .. } => None,
            })
            .flatten()
            .map(|input| input.outpoint.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let ledger = self.wallet_anchor_build_ledger()?;
        let wallet = self.wallet.unlocked()?;
        let available_utxos = ledger.available_utxos_for_address(wallet.address())?;
        // The plaintext anchor is the block's automatic burn when one is configured.
        // Keep a one-micro-IUNA anchor when automatic finalization is enabled with a
        // zero target, because the finalizer still needs a local burn to anchor.
        let anchor_burn_amount = self.burn_per_block.max(MIN_AUTO_BLOCK_ANCHOR_BURN_AMOUNT);
        let burn = match converge_fee_by_byte(self.burn_fee, |fee| {
            let required = anchor_burn_amount
                .checked_add(fee)
                .context("automatic finalizer anchor burn amount plus fee overflows")?;
            let outpoint = available_utxos
                .iter()
                .filter(|(outpoint, _)| !pending_transfer_spent_outpoints.contains(outpoint))
                .filter(|(_, output)| output.amount >= required)
                .min_by_key(|(_, output)| output.amount)
                .map(|(outpoint, _)| outpoint);
            match outpoint {
                Some(outpoint) => ledger.build_burn_for_next_block_with_inputs(
                    wallet,
                    anchor_burn_amount,
                    fee,
                    std::slice::from_ref(outpoint),
                ),
                None => {
                    let mut total = 0_u64;
                    let outpoints = available_utxos
                        .iter()
                        .filter(|(outpoint, _)| {
                            !pending_transfer_spent_outpoints.contains(outpoint)
                        })
                        .take_while(|(_, output)| {
                            if total >= required {
                                return false;
                            }
                            total = total.saturating_add(output.amount);
                            true
                        })
                        .map(|(outpoint, _)| outpoint.clone())
                        .collect::<Vec<_>>();
                    if total < required {
                        bail!(
                            "automatic finalizer anchor burn has insufficient confirmed funds outside pending transfers"
                        );
                    }
                    ledger.build_burn_for_next_block_with_inputs(
                        wallet,
                        anchor_burn_amount,
                        fee,
                        &outpoints,
                    )
                }
            }
        }) {
            Ok((burn, _)) => burn,
            Err(error) => {
                self.last_auto_anchor_burn_height = Some(current_height);
                return Err(error).context("automatic finalizer anchor burn failed");
            }
        };
        self.local_block_anchor_burn = Some((current_height, burn.clone()));
        self.last_auto_anchor_burn_height = Some(current_height);
        Ok(Some(burn))
    }

    fn best_automatic_burn_on_ledger(
        &self,
        ledger: &Ledger,
        fee_per_byte: Amount,
        balance: Amount,
    ) -> Option<Transaction> {
        let target = self.burn_per_block.min(balance);
        if target == 0 {
            return None;
        }
        let exact_at_fee_rate =
            self.build_burn_with_fee_rate_on_ledger(ledger, target, fee_per_byte);
        if let Ok((built, estimate)) = exact_at_fee_rate {
            if target
                .checked_add(estimate.fee)
                .is_some_and(|required| required <= balance)
            {
                return Some(built);
            }
        }
        if self.burn_per_block <= balance {
            let affordable_fee = balance.saturating_sub(target);
            if let Ok(built) =
                ledger.build_burn(self.wallet.unlocked().ok()?, target, affordable_fee)
            {
                return Some(built);
            }
        }

        let mut low = 1;
        let mut high = target;
        let mut best = None;
        while low <= high {
            let amount = low + (high - low) / 2;
            match self.build_burn_with_fee_rate_on_ledger(ledger, amount, fee_per_byte) {
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
        best
    }

    fn automatic_burn_needs_plaintext_anchor(&self, timestamp_ms: u64) -> bool {
        self.ledger
            .finalizer_rank_for_next_block(self.wallet.address())
            .is_some_and(|rank| self.wallet_rank_runs_vdf(rank))
            || self.should_prepare_recovery_vdf(timestamp_ms)
            || timestamp_ms.saturating_add(AUTO_PLAINTEXT_BURN_BEFORE_RECOVERY_MS)
                >= self.ledger.recovery_block_min_timestamp()
    }

    fn wallet_rank_runs_vdf(&self, rank: u32) -> bool {
        if rank == 0 {
            return true;
        }
        let rank_count = self.ledger.finalizer_rank_count_for_next_block();
        let allowed =
            allowed_recovery_vdf_rank_count(rank_count, self.recovery_vdf_top_rank_percent);
        usize::try_from(rank).is_ok_and(|rank| rank < allowed)
    }

    fn should_prepare_recovery_vdf(&self, timestamp_ms: u64) -> bool {
        if !self.ledger.recovery_block_available_at(timestamp_ms) {
            return false;
        }
        if self.recovery_vdf_top_rank_percent == 100 {
            return true;
        }
        if self.recovery_vdf_top_rank_percent == 0 {
            return false;
        }
        recovery_vdf_sample_percent(self.wallet.address(), self.ledger.tip_hash())
            < self.recovery_vdf_top_rank_percent
    }

    fn burn_bundle_collection_wait_ms(
        &mut self,
        timestamp_ms: u64,
        will_run_vdf: bool,
    ) -> Option<u64> {
        let next_height = self.ledger.height().saturating_add(1);
        let (attestation_ledger, _) = self.ledger_with_local_block_anchor();
        let explicit_signatures_required = if will_run_vdf {
            let wallet_rank =
                attestation_ledger.finalizer_rank_for_next_block(self.wallet.address());
            let will_run_ticket_vdf =
                wallet_rank.is_some_and(|rank| self.wallet_rank_runs_vdf(rank));
            let finalizer_mode =
                if !will_run_ticket_vdf && self.should_prepare_recovery_vdf(timestamp_ms) {
                    FinalizerMode::Recovery
                } else {
                    FinalizerMode::Ticket
                };
            let finalizer_rank = if matches!(finalizer_mode, FinalizerMode::Ticket) {
                wallet_rank.unwrap_or(0)
            } else {
                0
            };
            attestation_ledger.explicit_burn_bundle_signatures_required_for_next_block(
                finalizer_mode,
                finalizer_rank,
                self.wallet.address(),
            )
        } else {
            0
        };
        let wallet_is_committee_member = !self
            .ledger
            .burn_committee_memberships_for_next_block(self.wallet.address())
            .is_empty();
        if explicit_signatures_required == 0 || (!wallet_is_committee_member && !will_run_vdf) {
            if explicit_signatures_required == 0 {
                self.burn_bundle_collection_started = None;
            }
            return None;
        }

        let started_at = match self.burn_bundle_collection_started {
            Some((height, started_at)) if height == next_height => started_at,
            _ => {
                self.burn_bundle_collection_started = Some((next_height, timestamp_ms));
                timestamp_ms
            }
        };
        burn_bundle_collection_remaining_ms(started_at, timestamp_ms)
    }

    fn request_missing_burn_bundles_for_next_block(&mut self, _timestamp_ms: u64) {
        let (attestation_ledger, _) = self.ledger_with_local_block_anchor();
        let Some(finalizer_rank) =
            attestation_ledger.finalizer_rank_for_next_block(self.wallet.address())
        else {
            return;
        };
        if !self.wallet_rank_runs_vdf(finalizer_rank) {
            return;
        }
        let required = attestation_ledger.explicit_burn_bundle_signatures_required_for_next_block(
            FinalizerMode::Ticket,
            finalizer_rank,
            self.wallet.address(),
        );
        if required == 0 {
            return;
        }

        let present_slots = self
            .usable_burn_bundles_for_finalizer_rank(finalizer_rank)
            .into_iter()
            .map(|bundle| bundle.slot)
            .collect::<std::collections::BTreeSet<_>>();
        if present_slots.len() >= required {
            return;
        }

        self.enqueue_missing_burn_bundle_request(
            self.ledger.height().saturating_add(1),
            self.ledger.tip_hash().to_string(),
            required,
            attestation_ledger.burn_committee_for_next_ticket_block(finalizer_rank),
            &present_slots,
        );
    }

    fn enqueue_missing_burn_bundle_request(
        &mut self,
        height: u64,
        prev_hash: String,
        required: usize,
        committee: Vec<BurnCommitteeMember>,
        present_slots: &std::collections::BTreeSet<u8>,
    ) {
        if present_slots.len() >= required {
            return;
        }
        let slots = committee
            .into_iter()
            .map(|member| member.slot)
            .filter(|slot| *slot != 0 && !present_slots.contains(slot))
            .collect::<Vec<_>>();
        if slots.is_empty() {
            return;
        }

        self.outbox.push(GossipEnvelope::BurnBundleRequest {
            height,
            prev_hash,
            slots,
        });
    }

    pub(super) fn prepare_next_block_with_local_anchor(
        &self,
        timestamp_ms: u64,
    ) -> Result<PreparedBlock> {
        let (ledger, required_burn_signature) = self.ledger_with_local_block_anchor();
        let finalizer_rank = ledger
            .finalizer_rank_for_next_block(self.wallet.address())
            .context("cannot prepare ticket block without a mature burn ticket")?;
        ledger.prepare_next_block_with_required_burn_and_burn_bundles(
            self.wallet.address(),
            timestamp_ms,
            self.usable_burn_bundles_for_finalizer_rank(finalizer_rank),
            required_burn_signature.as_deref(),
        )
    }

    fn prepare_recovery_block_with_local_anchor(&self, timestamp_ms: u64) -> Result<PreparedBlock> {
        let (ledger, required_burn_signature) = self.ledger_with_local_block_anchor();
        ledger.prepare_recovery_block_with_required_burn_and_burn_bundles(
            self.wallet.address(),
            timestamp_ms,
            Vec::new(),
            required_burn_signature.as_deref(),
        )
    }

    pub(super) fn ledger_with_local_block_anchor(&self) -> (Ledger, Option<String>) {
        let mut ledger = self.ledger.clone();
        let Some((height, burn)) = &self.local_block_anchor_burn else {
            return (ledger, None);
        };
        let anchor_is_pending = ledger
            .pending()
            .iter()
            .any(|transaction| transaction.signature() == burn.signature());
        if *height == ledger.height() && !anchor_is_pending {
            // `has_transaction` also includes orphans, which cannot anchor a
            // block. Try to promote the local burn, prefer another eligible
            // wallet burn, or rebuild this cloned mempool with the liveness
            // anchor first when a pending transaction spends the same input.
            if ledger.submit_transaction(burn.clone()).unwrap_or(false) {
                return (ledger, Some(burn.signature().to_string()));
            }
            if ledger.pending().iter().any(|transaction| {
                transaction.is_burn()
                    && transaction.sender() == self.wallet.address()
                    && ledger.transaction_is_eligible_for_next_block(transaction)
            }) {
                return (ledger, None);
            }
            if ledger
                .prioritize_transaction_for_block_building(burn.clone())
                .unwrap_or(false)
            {
                return (ledger, Some(burn.signature().to_string()));
            }
        }
        (ledger, None)
    }

    pub(super) fn clear_stale_local_block_anchor(&mut self) {
        if self
            .local_block_anchor_burn
            .as_ref()
            .is_some_and(|(height, _)| *height != self.ledger.height())
        {
            self.local_block_anchor_burn = None;
        }
    }

    pub(super) fn clear_stale_burn_bundle_collection(&mut self) {
        let current_next_height = self.ledger.height().saturating_add(1);
        if self
            .burn_bundle_collection_started
            .is_some_and(|(height, _)| height != current_next_height)
        {
            self.burn_bundle_collection_started = None;
        }
    }
}

fn burn_bundle_collection_remaining_ms(started_at_ms: u64, now_ms: u64) -> Option<u64> {
    let elapsed = now_ms.saturating_sub(started_at_ms);
    (elapsed < BURN_BUNDLE_COLLECTION_MS).then(|| BURN_BUNDLE_COLLECTION_MS.saturating_sub(elapsed))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        adapters::chain_store::SqliteChainStore,
        app::{GossipEnvelope, InMemoryNetwork},
        domain::{
            BurnBundle, BurnCommitteeMember, FinalizerMode, GenesisBurn, Ledger, MICRO_IUNA,
            Transaction, Wallet, run_vdf,
        },
    };
    use tempfile::tempdir;

    use super::{BURN_BUNDLE_COLLECTION_MS, NodeCore, burn_bundle_collection_remaining_ms};

    #[test]
    fn burn_bundle_collection_waits_for_configured_window() {
        let started_at_ms = 1_000;
        let last_waiting_ms = started_at_ms + BURN_BUNDLE_COLLECTION_MS - 1;
        let finished_ms = started_at_ms + BURN_BUNDLE_COLLECTION_MS;

        assert_eq!(
            burn_bundle_collection_remaining_ms(started_at_ms, started_at_ms),
            Some(BURN_BUNDLE_COLLECTION_MS)
        );
        assert_eq!(
            burn_bundle_collection_remaining_ms(started_at_ms, last_waiting_ms),
            Some(1)
        );
        assert_eq!(
            burn_bundle_collection_remaining_ms(started_at_ms, finished_ms),
            None
        );
    }

    fn funded_ledger(wallets: &[Wallet]) -> Ledger {
        let allocations = wallets
            .iter()
            .map(|wallet| (wallet.address().to_string(), 10 * MICRO_IUNA))
            .collect::<BTreeMap<_, _>>();
        let genesis_burns = wallets
            .iter()
            .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
            .collect::<Vec<_>>();
        Ledger::new_with_genesis_burns(allocations, genesis_burns, 1).unwrap()
    }

    fn selected_finalizer(ledger: &Ledger, wallets: &[Wallet]) -> Wallet {
        let leader = ledger
            .expected_leader_for_next_block()
            .expect("test ledger should have a next finalizer");
        wallets
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap_or_else(|| panic!("missing wallet for selected finalizer {leader}"))
            .clone()
    }

    fn non_finalizer_wallet<'a>(wallets: &'a [Wallet], finalizer: &Wallet) -> &'a Wallet {
        wallets
            .iter()
            .find(|wallet| wallet.address() != finalizer.address())
            .expect("test fixture should include a non-finalizer wallet")
    }

    #[test]
    fn automatic_mining_prefers_runnable_ticket_over_available_recovery() {
        let wallet = Wallet::from_seed("ticket-before-recovery-wallet");
        let ledger = funded_ledger(std::slice::from_ref(&wallet));
        assert_eq!(
            ledger.finalizer_rank_for_next_block(wallet.address()),
            Some(0)
        );
        let timestamp_ms = ledger.recovery_block_min_timestamp();
        let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(wallet, ledger, true, 0, 1);

        let outcome = node.automatic_mine_once(timestamp_ms);
        let block = outcome
            .block
            .expect("rank 0 ticket finalizer should produce a block");

        assert_eq!(block.finalizer_mode, FinalizerMode::Ticket);
        assert_eq!(block.finalizer_rank, 0);
        assert!(block.leader_proof.is_some());
    }

    #[test]
    fn same_slot_bundles_for_different_members_do_not_conflict_locally() {
        let wallet = Wallet::from_seed("rank-bundle-cache-node");
        let ledger = funded_ledger(std::slice::from_ref(&wallet));
        let mut node = NodeCore::from_ledger(wallet, ledger, 0);
        let next_height = node.ledger.height() + 1;
        let alpha = BurnBundle {
            height: next_height,
            prev_hash: node.ledger.tip_hash().to_string(),
            slot: 1,
            member: "alpha".to_string(),
            burns: Vec::new(),
            signature: "sig-alpha".to_string(),
        };
        let beta = BurnBundle {
            height: next_height,
            prev_hash: node.ledger.tip_hash().to_string(),
            slot: 1,
            member: "beta".to_string(),
            burns: Vec::new(),
            signature: "sig-beta".to_string(),
        };

        node.burn_bundles
            .insert((alpha.height, alpha.slot, alpha.member.clone()), alpha);
        node.burn_bundles
            .insert((beta.height, beta.slot, beta.member.clone()), beta);
        node.equivocated_burn_bundle_slots
            .insert((next_height, 1, "alpha".to_string()));

        let usable = node.usable_burn_bundles();
        assert!(
            usable
                .iter()
                .all(|bundle| bundle.slot != 1 || bundle.member != "alpha")
        );
        assert!(
            usable
                .iter()
                .any(|bundle| bundle.slot == 1 && bundle.member == "beta")
        );
    }

    #[test]
    fn in_memory_network_survives_adversarial_gossip_restart_and_converges() {
        let alice = Wallet::from_seed("network-adversarial-alice");
        let bob = Wallet::from_seed("network-adversarial-bob");
        let carol = Wallet::from_seed("network-adversarial-carol");
        let wallets = [alice.clone(), bob.clone(), carol.clone()];
        let ledger = funded_ledger(&wallets);
        let finalizer = selected_finalizer(&ledger, &wallets);
        let mut burners = wallets
            .iter()
            .filter(|wallet| wallet.address() != finalizer.address());
        let burn_a = ledger.build_burn(burners.next().unwrap(), 1, 1).unwrap();
        let burn_b = ledger.build_burn(burners.next().unwrap(), 2, 1).unwrap();

        let mut alpha = NodeCore::from_ledger_with_burn_fee_and_enabled(
            finalizer.clone(),
            ledger.clone(),
            true,
            0,
            1,
        );
        let beta = NodeCore::from_ledger_with_burn_fee_and_enabled(
            finalizer.clone(),
            ledger.clone(),
            true,
            0,
            1,
        );
        let gamma = NodeCore::from_ledger(finalizer.clone(), ledger.clone(), 0);
        alpha.receive_transaction(burn_a.clone()).unwrap();
        alpha.receive_transaction(burn_b.clone()).unwrap();

        let mut network = InMemoryNetwork::default();
        network.insert("alpha", alpha);
        network.insert("beta", beta);
        network.insert("gamma", gamma);

        network
            .gossip_mempools_once_filtered(|_, to, envelope| {
                !(to == "gamma" && matches!(envelope, GossipEnvelope::Transactions { .. }))
            })
            .unwrap();
        assert!(
            network
                .node("beta")
                .unwrap()
                .pending_transactions()
                .iter()
                .any(|tx| tx.signature() == burn_a.signature())
        );
        assert!(
            !network
                .node("gamma")
                .unwrap()
                .pending_transactions()
                .iter()
                .any(|tx| tx.signature() == burn_a.signature())
        );

        network.gossip_mempools_once().unwrap();
        assert!(
            network
                .node("gamma")
                .unwrap()
                .pending_transactions()
                .iter()
                .any(|tx| tx.signature() == burn_b.signature())
        );

        network
            .node_mut("alpha")
            .unwrap()
            .publish_burn_bundle_for_next_block()
            .unwrap();
        network
            .deliver_until_idle_filtered(|_, to, envelope| {
                !(to == "gamma" && matches!(envelope, GossipEnvelope::BurnBundle(_)))
            })
            .unwrap();
        let bundle = network
            .node("beta")
            .unwrap()
            .usable_burn_bundles()
            .into_iter()
            .find(|bundle| bundle.member == finalizer.address())
            .expect("beta should receive alpha's burn bundle");
        assert!(
            network
                .node("gamma")
                .unwrap()
                .usable_burn_bundles()
                .is_empty()
        );

        let conflicting_bundle = ledger.test_burn_bundle(&finalizer, vec![burn_b.clone()]);
        assert_ne!(bundle.canonical(), conflicting_bundle.canonical());
        network
            .node_mut("beta")
            .unwrap()
            .receive(GossipEnvelope::BurnBundle(conflicting_bundle))
            .unwrap();
        assert!(
            network
                .node("beta")
                .unwrap()
                .usable_burn_bundles()
                .into_iter()
                .all(|candidate| candidate.slot != bundle.slot || candidate.member != bundle.member)
        );

        let block = {
            let alpha = network.node_mut("alpha").unwrap();
            alpha.prepare_automatic_burn(2).unwrap();
            let work = alpha.prepare_next_block_with_local_anchor(2).unwrap();
            alpha
                .complete_prepared_block_at(
                    work.clone(),
                    run_vdf(work.vdf_seed(), work.vdf_rounds()),
                    2,
                )
                .unwrap()
        };
        assert!(
            block
                .burn_bundle_section
                .burns
                .iter()
                .any(|masked| masked.burn.signature() == burn_a.signature())
        );
        let stale_gamma_height = network.node("gamma").unwrap().chain_height();

        network
            .deliver_until_idle_filtered(|_, to, envelope| {
                !(to == "gamma"
                    && matches!(
                        envelope,
                        GossipEnvelope::Block(_)
                            | GossipEnvelope::Blocks { .. }
                            | GossipEnvelope::ChainBootstrap(_)
                    ))
            })
            .unwrap();
        assert_eq!(
            network.node("alpha").unwrap().chain_tip_hash(),
            network.node("beta").unwrap().chain_tip_hash()
        );
        assert_eq!(
            network.node("gamma").unwrap().chain_height(),
            stale_gamma_height
        );

        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("gamma.sqlite3")).unwrap();
        store
            .save(&network.node("gamma").unwrap().chain_snapshot())
            .unwrap();
        let restored_ledger =
            Ledger::from_persisted_snapshot(store.load().unwrap().unwrap()).unwrap();
        network.insert(
            "gamma",
            NodeCore::from_ledger(finalizer.clone(), restored_ledger, 0),
        );
        assert_eq!(
            network.node("gamma").unwrap().chain_height(),
            stale_gamma_height
        );

        assert!(network.sync_node_from_peer("alpha", "gamma", 128).unwrap());
        network.deliver_until_idle().unwrap();
        let tip = network.node("alpha").unwrap().chain_tip_hash();
        assert_eq!(network.node("beta").unwrap().chain_tip_hash(), tip);
        assert_eq!(network.node("gamma").unwrap().chain_tip_hash(), tip);
    }

    #[test]
    fn local_anchor_burn_is_included_when_publishing_bundle() {
        let wallet = Wallet::from_seed("local-anchor-bundle-wallet");
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(wallet.address(), MICRO_IUNA)],
            1,
        )
        .unwrap();
        assert!(
            ledger
                .finalizer_rank_for_next_block(wallet.address())
                .is_some()
        );
        let mut node =
            NodeCore::from_ledger_with_burn_fee_and_enabled(wallet.clone(), ledger, true, 0, 1);

        node.prepare_automatic_burn(1).unwrap();

        assert_eq!(node.burn_bundle_collection_wait_ms(1, true), None);

        node.publish_burn_bundle_for_next_block().unwrap();

        let bundles = node.usable_burn_bundles();
        let bundled_anchor = bundles
            .iter()
            .find(|bundle| bundle.member == wallet.address())
            .and_then(|bundle| bundle.burns.first())
            .expect("the local anchor should be exposed through the committee bundle");
        assert_eq!(
            bundled_anchor.fee(),
            bundled_anchor.economic_size_bytes() as u64
        );
    }

    #[test]
    fn local_anchor_burn_stores_total_fee_derived_from_configured_fee_rate() {
        let wallet = Wallet::from_seed("local-anchor-fee-rate-wallet");
        let ledger = funded_ledger(std::slice::from_ref(&wallet));
        let fee_per_byte = 100;
        let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
            wallet,
            ledger,
            true,
            3_000,
            fee_per_byte,
        );

        node.prepare_automatic_burn(1).unwrap();

        let burn = node
            .local_block_anchor_burn
            .as_ref()
            .map(|(_, burn)| burn)
            .expect("selected finalizer should prepare a local anchor");
        assert_eq!(burn.amount(), 3_000);
        assert_eq!(burn.fee(), fee_per_byte * burn.economic_size_bytes() as u64);
        assert_ne!(burn.fee(), fee_per_byte);
        let required = burn.amount().checked_add(burn.fee()).unwrap();
        let expected_outpoint = node
            .ledger()
            .available_utxos_for_address(node.wallet_address())
            .unwrap()
            .into_iter()
            .filter(|(_, output)| output.amount >= required)
            .min_by_key(|(_, output)| output.amount)
            .map(|(outpoint, _)| outpoint)
            .expect("fixture should contain a single UTXO large enough for the burn");
        let Transaction::Burn { inputs, .. } = burn else {
            panic!("local anchor should be a burn transaction");
        };
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].outpoint, expected_outpoint);
    }

    #[test]
    fn conflicting_local_anchor_falls_back_to_pending_wallet_burn() {
        let wallet = Wallet::from_seed("conflicting-local-anchor-wallet");
        let ledger = funded_ledger(std::slice::from_ref(&wallet));
        let pending_burn = ledger.build_burn(&wallet, 2, 1).unwrap();
        let mut node =
            NodeCore::from_ledger_with_burn_fee_and_enabled(wallet.clone(), ledger, true, 1, 1);
        node.receive_transaction(pending_burn.clone()).unwrap();

        node.prepare_automatic_burn(1).unwrap();
        let local_anchor = node
            .local_block_anchor_burn
            .as_ref()
            .map(|(_, burn)| burn)
            .expect("selected finalizer should prepare a local anchor");
        assert_ne!(local_anchor.signature(), pending_burn.signature());

        let (candidate, required_burn_signature) = node.ledger_with_local_block_anchor();
        assert_eq!(required_burn_signature, None);
        assert!(
            candidate
                .pending()
                .iter()
                .any(|transaction| transaction.signature() == pending_burn.signature())
        );

        let prepared = node.prepare_next_block_with_local_anchor(1).unwrap();
        let block = prepared.finish(&wallet, "test-vdf-output".to_string());
        assert!(
            block
                .transactions
                .iter()
                .any(|transaction| transaction.signature() == pending_burn.signature())
        );
    }

    #[test]
    fn local_anchor_preserves_pending_transfer_when_another_utxo_is_available() {
        let wallet = Wallet::from_seed("priority-local-anchor-wallet");
        let recipient = Wallet::from_seed("priority-local-anchor-recipient");
        let ledger = funded_ledger(std::slice::from_ref(&wallet));
        let pending_transfer = ledger
            .build_transfer(&wallet, recipient.address(), 2, 1)
            .unwrap();
        let mut node =
            NodeCore::from_ledger_with_burn_fee_and_enabled(wallet.clone(), ledger, true, 1, 1);
        node.receive_transaction(pending_transfer.clone()).unwrap();

        node.prepare_automatic_burn(1).unwrap();
        let local_anchor_signature = node
            .local_block_anchor_burn
            .as_ref()
            .map(|(_, burn)| burn.signature().to_string())
            .expect("selected finalizer should prepare a local anchor");
        let (candidate, required_burn_signature) = node.ledger_with_local_block_anchor();

        assert_eq!(
            required_burn_signature,
            Some(local_anchor_signature.clone())
        );
        assert!(
            candidate
                .pending()
                .iter()
                .any(|transaction| transaction.signature() == local_anchor_signature)
        );
        assert!(
            candidate
                .pending()
                .iter()
                .any(|transaction| transaction.signature() == pending_transfer.signature())
        );
        let prepared = node.prepare_next_block_with_local_anchor(1).unwrap();
        let block = prepared.finish(&wallet, "test-vdf-output".to_string());
        assert!(
            block
                .transactions
                .iter()
                .any(|transaction| transaction.signature() == pending_transfer.signature())
        );
    }

    #[test]
    fn local_anchor_uses_unreserved_utxo_instead_of_displacing_pending_transfer() {
        let wallet = Wallet::from_seed("non-conflicting-local-anchor-wallet");
        let recipient = Wallet::from_seed("non-conflicting-local-anchor-recipient");
        let ledger = funded_ledger(std::slice::from_ref(&wallet));
        let timestamp_ms = ledger.recovery_block_min_timestamp();
        let mut node =
            NodeCore::from_ledger_with_burn_fee_and_enabled(wallet.clone(), ledger, true, 1, 1);

        let first = node.automatic_mine_once(timestamp_ms);
        assert!(
            first.block.is_some(),
            "fixture should create multiple wallet UTXOs"
        );
        let available = node
            .ledger()
            .available_utxos_for_address(wallet.address())
            .unwrap();
        assert!(available.len() >= 2);
        let transfer_outpoint = available
            .iter()
            .filter(|(_, output)| output.amount >= 2)
            .min_by_key(|(_, output)| output.amount)
            .map(|(outpoint, _)| outpoint.clone())
            .expect("fixture should have a transfer input");
        let pending_transfer = node
            .ledger()
            .build_transfer_with_inputs(
                &wallet,
                recipient.address(),
                1,
                1,
                std::slice::from_ref(&transfer_outpoint),
            )
            .unwrap();
        node.receive_transaction(pending_transfer.clone()).unwrap();

        node.prepare_automatic_burn(timestamp_ms + 1).unwrap();
        let anchor = node
            .local_block_anchor_burn
            .as_ref()
            .map(|(_, burn)| burn)
            .expect("selected finalizer should prepare a local anchor");
        let Transaction::Burn { inputs, .. } = anchor else {
            panic!("local anchor should be a burn transaction");
        };
        assert!(
            inputs
                .iter()
                .all(|input| input.outpoint != transfer_outpoint)
        );
        let anchor_signature = anchor.signature().to_string();

        let prepared = node
            .prepare_next_block_with_local_anchor(timestamp_ms + 1)
            .unwrap();
        let block = prepared.finish(&wallet, "test-vdf-output".to_string());
        assert!(
            block
                .transactions
                .iter()
                .any(|transaction| { transaction.signature() == pending_transfer.signature() })
        );
        assert!(
            block
                .transactions
                .iter()
                .any(|transaction| transaction.signature() == anchor_signature)
        );
    }

    #[test]
    fn local_anchor_waits_when_all_confirmed_utxos_are_reserved_by_pending_transfer() {
        let wallet = Wallet::from_seed("reserved-local-anchor-wallet");
        let recipient = Wallet::from_seed("reserved-local-anchor-recipient");
        let ledger = funded_ledger(std::slice::from_ref(&wallet));
        let outpoints = ledger
            .available_utxos_for_address(wallet.address())
            .unwrap()
            .into_iter()
            .map(|(outpoint, _)| outpoint)
            .collect::<Vec<_>>();
        let pending_transfer = ledger
            .build_transfer_with_inputs(&wallet, recipient.address(), 1, 1, &outpoints)
            .unwrap();
        let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(wallet, ledger, true, 1, 1);
        node.receive_transaction(pending_transfer.clone()).unwrap();

        let error = node.prepare_automatic_burn(1).unwrap_err();

        assert!(format!("{error:#}").contains(
            "automatic finalizer anchor burn has insufficient confirmed funds outside pending transfers"
        ));
        assert!(node.local_block_anchor_burn.is_none());
        assert!(
            node.ledger()
                .pending()
                .iter()
                .any(|transaction| transaction.signature() == pending_transfer.signature())
        );
    }

    #[test]
    fn automatic_finalization_disabled_node_still_publishes_committee_bundle() {
        let wallet = Wallet::from_seed("disabled-finalizer-committee-wallet");
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(wallet.address(), MICRO_IUNA)],
            1,
        )
        .unwrap();
        let mut node =
            NodeCore::from_ledger_with_burn_fee_and_enabled(wallet.clone(), ledger, false, 0, 1);

        let plan = node.prepare_automatic_finalization(1);

        assert_eq!(
            plan.skipped_reason.as_deref(),
            Some("automatic mining is off")
        );
        assert!(node.drain_outbox().into_iter().any(|envelope| {
            matches!(
                envelope,
                GossipEnvelope::BurnBundle(bundle) if bundle.member == wallet.address()
            )
        }));
    }

    #[test]
    fn publish_burn_bundle_rebroadcasts_existing_local_bundle() {
        let wallet = Wallet::from_seed("rebroadcast-existing-bundle-wallet");
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(wallet.address(), MICRO_IUNA)],
            1,
        )
        .unwrap();
        assert!(
            ledger
                .burn_committee_memberships_for_next_block(wallet.address())
                .iter()
                .any(|member| member.owner == wallet.address())
        );
        let mut node =
            NodeCore::from_ledger_with_burn_fee_and_enabled(wallet.clone(), ledger, true, 0, 1);

        node.publish_burn_bundle_for_next_block().unwrap();
        let first = node
            .drain_outbox()
            .into_iter()
            .find_map(|envelope| match envelope {
                GossipEnvelope::BurnBundle(bundle) => Some(bundle),
                _ => None,
            })
            .expect("first publish should gossip the local burn bundle");

        node.publish_burn_bundle_for_next_block().unwrap();
        let rebroadcast = node
            .drain_outbox()
            .into_iter()
            .find_map(|envelope| match envelope {
                GossipEnvelope::BurnBundle(bundle) => Some(bundle),
                _ => None,
            })
            .expect("second publish should rebroadcast the existing burn bundle");

        assert_eq!(rebroadcast.canonical(), first.canonical());
    }

    #[test]
    fn finalizer_requests_missing_burn_bundles_while_collecting() {
        let wallet = Wallet::from_seed("missing-bundle-request-wallet");
        let ledger = funded_ledger(std::slice::from_ref(&wallet));
        let mut node = NodeCore::from_ledger(wallet, ledger, 0);
        let present_slots = std::collections::BTreeSet::from([1]);

        node.enqueue_missing_burn_bundle_request(
            42,
            "parent-hash".to_string(),
            2,
            vec![
                BurnCommitteeMember {
                    slot: 0,
                    root: "root-0".to_string(),
                    owner: "finalizer".to_string(),
                    weight: 1,
                },
                BurnCommitteeMember {
                    slot: 1,
                    root: "root-1".to_string(),
                    owner: "present".to_string(),
                    weight: 1,
                },
                BurnCommitteeMember {
                    slot: 2,
                    root: "root-2".to_string(),
                    owner: "missing".to_string(),
                    weight: 1,
                },
            ],
            &present_slots,
        );
        let request = node
            .drain_outbox()
            .into_iter()
            .find_map(|envelope| match envelope {
                GossipEnvelope::BurnBundleRequest {
                    height,
                    prev_hash,
                    slots,
                } => Some((height, prev_hash, slots)),
                _ => None,
            })
            .expect("collecting finalizer should request missing burn bundles");

        assert_eq!(request.0, 42);
        assert_eq!(request.1, "parent-hash");
        assert_eq!(request.2, vec![2]);
    }

    #[test]
    fn burn_bundle_request_response_only_returns_matching_slots() {
        let alice = Wallet::from_seed("bundle-request-response-alice");
        let bob = Wallet::from_seed("bundle-request-response-bob");
        let wallets = [alice.clone(), bob.clone()];
        let ledger = funded_ledger(&wallets);
        let signer = selected_finalizer(&ledger, &wallets);
        let mut node = NodeCore::from_ledger(signer.clone(), ledger.clone(), 0);

        node.publish_burn_bundle_for_next_block().unwrap();
        let bundle = node
            .usable_burn_bundles()
            .into_iter()
            .find(|bundle| bundle.member == signer.address())
            .expect("signer should have a local burn bundle");

        let matching =
            node.burn_bundles_for_request(bundle.height, &bundle.prev_hash, &[bundle.slot]);
        let wrong_parent = node.burn_bundles_for_request(bundle.height, "wrong-parent", &[]);
        let wrong_slot = node.burn_bundles_for_request(
            bundle.height,
            &bundle.prev_hash,
            &[bundle.slot.saturating_add(1)],
        );

        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].canonical(), bundle.canonical());
        assert!(wrong_parent.is_empty());
        assert!(wrong_slot.is_empty());
    }

    #[test]
    fn burn_bundle_gossip_reaches_peer_with_matching_mempool() {
        let alice = Wallet::from_seed("bundle-gossip-alice");
        let bob = Wallet::from_seed("bundle-gossip-bob");
        let wallets = [alice.clone(), bob.clone()];
        let ledger = funded_ledger(&wallets);
        let finalizer = selected_finalizer(&ledger, &wallets);
        let burner = non_finalizer_wallet(&wallets, &finalizer);
        let burn = ledger.build_burn(burner, 1, 1).unwrap();
        let mut sender = NodeCore::from_ledger(finalizer.clone(), ledger.clone(), 0);
        let mut receiver = NodeCore::from_ledger(finalizer.clone(), ledger, 0);
        sender.receive_transaction(burn.clone()).unwrap();
        receiver.receive_transaction(burn).unwrap();

        sender.publish_burn_bundle_for_next_block().unwrap();
        let mut network = InMemoryNetwork::default();
        network.insert("sender", sender);
        network.insert("receiver", receiver);

        network.deliver_until_idle().unwrap();

        assert!(
            network
                .node("receiver")
                .unwrap()
                .usable_burn_bundles()
                .iter()
                .any(|bundle| bundle.member == finalizer.address() && !bundle.burns.is_empty())
        );
    }

    #[test]
    fn block_with_burn_bundle_imports_on_independent_peer_ledger() {
        let alice = Wallet::from_seed("bundle-import-alice");
        let bob = Wallet::from_seed("bundle-import-bob");
        let wallets = [alice.clone(), bob.clone()];
        let ledger = funded_ledger(&wallets);
        let finalizer = selected_finalizer(&ledger, &wallets);
        let burner = non_finalizer_wallet(&wallets, &finalizer);
        let burn = ledger.build_burn(burner, 1, 1).unwrap();
        let mut producer = NodeCore::from_ledger_with_burn_fee_and_enabled(
            finalizer.clone(),
            ledger.clone(),
            true,
            0,
            1,
        );
        let mut peer = NodeCore::from_ledger(finalizer.clone(), ledger, 0);
        producer.receive_transaction(burn.clone()).unwrap();
        peer.receive_transaction(burn.clone()).unwrap();
        producer.prepare_automatic_burn(1).unwrap();
        producer.publish_burn_bundle_for_next_block().unwrap();

        let work = producer.prepare_next_block_with_local_anchor(1).unwrap();
        let block = producer
            .complete_prepared_block_at(
                work.clone(),
                run_vdf(work.vdf_seed(), work.vdf_rounds()),
                1,
            )
            .unwrap();
        assert!(!block.burn_bundle_section.burns.is_empty());

        peer.receive(super::super::GossipEnvelope::Block(block))
            .unwrap();

        assert_eq!(producer.chain_tip_hash(), peer.chain_tip_hash());
        assert!(
            !peer
                .pending_transactions()
                .iter()
                .any(|transaction| transaction.signature() == burn.signature())
        );
    }

    #[test]
    fn burn_bundle_contents_change_prepared_vdf_seed() {
        let alice = Wallet::from_seed("bundle-seed-alice");
        let bob = Wallet::from_seed("bundle-seed-bob");
        let wallets = [alice.clone(), bob.clone()];
        let ledger = funded_ledger(&wallets);
        let finalizer = selected_finalizer(&ledger, &wallets);
        let burner = non_finalizer_wallet(&wallets, &finalizer);
        let burn = ledger.build_burn(burner, 1, 1).unwrap();
        let mut node =
            NodeCore::from_ledger_with_burn_fee_and_enabled(finalizer, ledger, true, 0, 1);
        node.receive_transaction(burn).unwrap();
        node.prepare_automatic_burn(1).unwrap();

        let (ledger_with_anchor, required_burn_signature) = node.ledger_with_local_block_anchor();
        let without_bundle = ledger_with_anchor
            .prepare_next_block_with_required_burn_and_burn_bundles(
                node.wallet_address(),
                1,
                Vec::new(),
                required_burn_signature.as_deref(),
            )
            .unwrap();

        node.publish_burn_bundle_for_next_block().unwrap();
        let with_bundle = node.prepare_next_block_with_local_anchor(1).unwrap();

        assert_ne!(without_bundle.vdf_seed(), with_bundle.vdf_seed());
        assert_ne!(
            without_bundle
                .finish_at(
                    node.wallet.unlocked().unwrap(),
                    "precheck-vdf-output".to_string(),
                    1,
                )
                .burn_bundle_hashes(),
            with_bundle
                .finish_at(
                    node.wallet.unlocked().unwrap(),
                    "precheck-vdf-output".to_string(),
                    1,
                )
                .burn_bundle_hashes()
        );
    }
}
