use anyhow::{Context, Result};

use super::helpers::{allowed_recovery_vdf_rank_count, recovery_vdf_sample_percent};
use super::{
    AUTO_BLOCK_ANCHOR_BURN_AMOUNT, AUTO_BLOCK_ANCHOR_BURN_FEE,
    AUTO_PLAINTEXT_BURN_BEFORE_RECOVERY_MS, AutoMineOutcome, AutoMinePlan, BuiltBlindedTransaction,
    Ledger, NodeCore, PreparedBlock, REVEAL_BUNDLE_COLLECTION_MS, Transaction, run_vdf,
};
use crate::domain::Amount;

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

        let pow_error = match self.prepare_automatic_pow_mine() {
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
            wallet_rank.is_none() && self.should_prepare_recovery_vdf(timestamp_ms);
        if let Some(wait_ms) = self.reveal_bundle_collection_wait_ms(
            timestamp_ms,
            will_run_ticket_vdf || will_run_recovery_vdf,
        ) {
            plan.skipped_reason = Some(format!(
                "collecting blinded reveals for next block ({:.1}s remaining)",
                wait_ms as f64 / 1000.0
            ));
            return plan;
        }
        if let Err(error) = self.publish_reveal_bundle_for_next_block() {
            plan.skipped_reason = Some(format!("{error:#}"));
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
            if self.should_prepare_recovery_vdf(timestamp_ms) {
                match self.prepare_recovery_block_with_local_anchor(timestamp_ms) {
                    Ok(work) => {
                        plan.work = Some(work);
                    }
                    Err(error) => {
                        plan.skipped_reason = Some(format!("{error:#}"));
                    }
                }
            } else {
                let selected_leader = self.ledger.expected_leader_for_next_block();
                plan.skipped_reason = selected_leader.map(|leader| {
                    format!("wallet is waiting for selected finalizer {leader} to finish the VDF")
                });
            }
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
            return plan;
        }

        if !self.automatic_mining_enabled {
            plan.skipped_reason = Some("automatic mining is off".to_string());
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
            wallet_rank.is_none() && self.should_prepare_recovery_vdf(timestamp_ms);
        if let Some(wait_ms) = self.reveal_bundle_collection_wait_ms(
            timestamp_ms,
            will_run_ticket_vdf || will_run_recovery_vdf,
        ) {
            plan.skipped_reason = Some(format!(
                "collecting blinded reveals for next block ({:.1}s remaining)",
                wait_ms as f64 / 1000.0
            ));
            return plan;
        }
        if let Err(error) = self.publish_reveal_bundle_for_next_block() {
            plan.skipped_reason = Some(format!("{error:#}"));
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
            if self.should_prepare_recovery_vdf(timestamp_ms) {
                match self.prepare_recovery_block_with_local_anchor(timestamp_ms) {
                    Ok(work) => {
                        plan.work = Some(work);
                    }
                    Err(error) => {
                        plan.skipped_reason = Some(format!("{error:#}"));
                    }
                }
            } else {
                let selected_leader = self.ledger.expected_leader_for_next_block();
                plan.skipped_reason = selected_leader.map(|leader| {
                    format!("wallet is waiting for selected finalizer {leader} to finish the VDF")
                });
            }
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
        let burn = tx.payload.clone();
        self.submit_owned_blinded_transaction(tx)?;
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

        let ledger = self.wallet_anchor_build_ledger()?;
        let wallet = self.wallet.unlocked()?;
        let required = AUTO_BLOCK_ANCHOR_BURN_AMOUNT
            .checked_add(AUTO_BLOCK_ANCHOR_BURN_FEE)
            .context("automatic finalizer anchor burn amount plus fee overflows")?;
        let outpoint = ledger
            .available_utxos_for_address(wallet.address())?
            .into_iter()
            .filter(|(_, output)| output.amount >= required)
            .min_by_key(|(_, output)| output.amount)
            .map(|(outpoint, _)| outpoint);
        let burn = match outpoint {
            Some(outpoint) => ledger.build_burn_with_inputs(
                wallet,
                AUTO_BLOCK_ANCHOR_BURN_AMOUNT,
                AUTO_BLOCK_ANCHOR_BURN_FEE,
                &[outpoint],
            ),
            None => ledger.build_burn(
                wallet,
                AUTO_BLOCK_ANCHOR_BURN_AMOUNT,
                AUTO_BLOCK_ANCHOR_BURN_FEE,
            ),
        };
        let burn = match burn {
            Ok(burn) => burn,
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
    ) -> Option<BuiltBlindedTransaction> {
        let target = self.burn_per_block.min(balance);
        if target == 0 {
            return None;
        }
        let exact_at_fee_rate =
            self.build_blinded_burn_with_fee_rate_on_ledger(ledger, target, fee_per_byte);
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
                self.build_blinded_burn_with_fee_on_ledger(ledger, target, affordable_fee)
            {
                return Some(built);
            }
        }

        let mut low = 1;
        let mut high = target;
        let mut best = None;
        while low <= high {
            let amount = low + (high - low) / 2;
            match self.build_blinded_burn_with_fee_rate_on_ledger(ledger, amount, fee_per_byte) {
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
        if self.ledger.finalizer_rank_count_for_next_block() > 0 {
            return false;
        }
        recovery_vdf_sample_percent(self.wallet.address(), self.ledger.tip_hash())
            < self.recovery_vdf_top_rank_percent
    }

    fn reveal_bundle_collection_wait_ms(
        &mut self,
        timestamp_ms: u64,
        will_run_vdf: bool,
    ) -> Option<u64> {
        let next_height = self.ledger.height().saturating_add(1);
        let has_pending_reveals = !self.ledger.pending_blinded_reveals().is_empty();
        let wallet_is_committee_member = self
            .ledger
            .reveal_committee_for_next_block()
            .iter()
            .any(|member| member.owner == self.wallet.address());
        if !has_pending_reveals || (!wallet_is_committee_member && !will_run_vdf) {
            if !has_pending_reveals {
                self.reveal_bundle_collection_started = None;
            }
            return None;
        }

        let started_at = match self.reveal_bundle_collection_started {
            Some((height, started_at)) if height == next_height => started_at,
            _ => {
                self.reveal_bundle_collection_started = Some((next_height, timestamp_ms));
                timestamp_ms
            }
        };
        let elapsed = timestamp_ms.saturating_sub(started_at);
        (elapsed < REVEAL_BUNDLE_COLLECTION_MS)
            .then(|| REVEAL_BUNDLE_COLLECTION_MS.saturating_sub(elapsed))
    }

    pub(super) fn prepare_next_block_with_local_anchor(
        &self,
        timestamp_ms: u64,
    ) -> Result<PreparedBlock> {
        let (ledger, required_burn_signature) = self.ledger_with_local_block_anchor();
        ledger.prepare_next_block_with_required_burn_and_reveal_bundles(
            self.wallet.address(),
            timestamp_ms,
            self.usable_reveal_bundles(),
            required_burn_signature.as_deref(),
        )
    }

    fn prepare_recovery_block_with_local_anchor(&self, timestamp_ms: u64) -> Result<PreparedBlock> {
        let (ledger, required_burn_signature) = self.ledger_with_local_block_anchor();
        ledger.prepare_recovery_block_with_required_burn_and_reveal_bundles(
            self.wallet.address(),
            timestamp_ms,
            self.usable_reveal_bundles(),
            required_burn_signature.as_deref(),
        )
    }

    fn ledger_with_local_block_anchor(&self) -> (Ledger, Option<String>) {
        let mut ledger = self.ledger.clone();
        let Some((height, burn)) = &self.local_block_anchor_burn else {
            return (ledger, None);
        };
        if *height == ledger.height() && !ledger.has_transaction(burn.signature()) {
            ledger.drop_pending_blinded_conflicting_with_transaction(burn);
            if ledger.submit_transaction(burn.clone()).is_ok() {
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

    pub(super) fn clear_stale_reveal_bundle_collection(&mut self) {
        let current_next_height = self.ledger.height().saturating_add(1);
        if self
            .reveal_bundle_collection_started
            .is_some_and(|(height, _)| height != current_next_height)
        {
            self.reveal_bundle_collection_started = None;
        }
        if self.ledger.pending_blinded_reveals().is_empty() {
            self.reveal_bundle_collection_started = None;
        }
    }
}

#[cfg(test)]
mod tests;
