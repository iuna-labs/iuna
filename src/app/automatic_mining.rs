use anyhow::{Context, Result};

use super::helpers::{allowed_recovery_vdf_rank_count, recovery_vdf_sample_percent};
use super::{
    AUTO_BLOCK_ANCHOR_BURN_AMOUNT, AUTO_BLOCK_ANCHOR_BURN_FEE,
    AUTO_PLAINTEXT_BURN_BEFORE_RECOVERY_MS, AutoMineOutcome, AutoMinePlan,
    BURN_BUNDLE_COLLECTION_MS, Ledger, NodeCore, PreparedBlock, Transaction, run_vdf,
};
use crate::domain::{Amount, FinalizerMode};

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
        let will_run_recovery_vdf = self.should_prepare_recovery_vdf(timestamp_ms);
        let will_run_ticket_vdf = !will_run_recovery_vdf
            && wallet_rank.is_some_and(|rank| self.wallet_rank_runs_vdf(rank));
        if let Some(wait_ms) = self.burn_bundle_collection_wait_ms(
            timestamp_ms,
            will_run_ticket_vdf || will_run_recovery_vdf,
        ) {
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
        let will_run_recovery_vdf = self.should_prepare_recovery_vdf(timestamp_ms);
        let will_run_ticket_vdf = !will_run_recovery_vdf
            && wallet_rank.is_some_and(|rank| self.wallet_rank_runs_vdf(rank));
        if let Some(wait_ms) = self.burn_bundle_collection_wait_ms(
            timestamp_ms,
            will_run_ticket_vdf || will_run_recovery_vdf,
        ) {
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
            let finalizer_mode = if self.should_prepare_recovery_vdf(timestamp_ms) {
                FinalizerMode::Recovery
            } else {
                FinalizerMode::Ticket
            };
            let finalizer_rank = if matches!(finalizer_mode, FinalizerMode::Ticket) {
                attestation_ledger
                    .finalizer_rank_for_next_block(self.wallet.address())
                    .unwrap_or(0)
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
        let wallet_is_committee_member = self
            .ledger
            .burn_committee_for_next_block()
            .iter()
            .any(|member| member.owner == self.wallet.address());
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
        let elapsed = timestamp_ms.saturating_sub(started_at);
        (elapsed < BURN_BUNDLE_COLLECTION_MS)
            .then(|| BURN_BUNDLE_COLLECTION_MS.saturating_sub(elapsed))
    }

    pub(super) fn prepare_next_block_with_local_anchor(
        &self,
        timestamp_ms: u64,
    ) -> Result<PreparedBlock> {
        let (ledger, required_burn_signature) = self.ledger_with_local_block_anchor();
        ledger.prepare_next_block_with_required_burn_and_burn_bundles(
            self.wallet.address(),
            timestamp_ms,
            self.usable_burn_bundles(),
            required_burn_signature.as_deref(),
        )
    }

    fn prepare_recovery_block_with_local_anchor(&self, timestamp_ms: u64) -> Result<PreparedBlock> {
        let (ledger, required_burn_signature) = self.ledger_with_local_block_anchor();
        ledger.prepare_recovery_block_with_required_burn_and_burn_bundles(
            self.wallet.address(),
            timestamp_ms,
            self.usable_burn_bundles(),
            required_burn_signature.as_deref(),
        )
    }

    pub(super) fn ledger_with_local_block_anchor(&self) -> (Ledger, Option<String>) {
        let mut ledger = self.ledger.clone();
        let Some((height, burn)) = &self.local_block_anchor_burn else {
            return (ledger, None);
        };
        if *height == ledger.height()
            && !ledger.has_transaction(burn.signature())
            && ledger.submit_transaction(burn.clone()).is_ok()
        {
            return (ledger, Some(burn.signature().to_string()));
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        adapters::chain_store::SqliteChainStore,
        app::{GossipEnvelope, InMemoryNetwork},
        domain::{GenesisBurn, Ledger, MICRO_IUNA, Wallet, run_vdf},
    };
    use tempfile::tempdir;

    use super::NodeCore;

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
                .all(|candidate| candidate.slot != bundle.slot)
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
                            | GossipEnvelope::ChainSnapshot(_)
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

        assert!(
            node.usable_burn_bundles()
                .iter()
                .any(|bundle| bundle.member == wallet.address() && !bundle.burns.is_empty())
        );
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
