use anyhow::{Result, bail};

use super::{
    BurnBundle, FinalizerMode, Ledger, PreparedBlock, RECOVERY_BLOCK_DELAY_MS, Wallet,
    ensure_block_has_burn, ensure_block_has_burn_from, recovery_vdf_seed_for_child, run_vdf,
    ticket_block_min_timestamp, vdf_content_commitment, vdf_seed_for_child,
};

impl Ledger {
    pub fn mine_next_block(&self, wallet: &Wallet, timestamp_ms: u64) -> Result<super::Block> {
        let reward_address = (self.height().saturating_add(1)
            >= super::HYBRID_REWARD_ACTIVATION_HEIGHT)
            .then(|| {
                wallet.hybrid_address(super::AddressNetwork::from_profile_id(
                    &self.launch_profile.profile_id,
                ))
            });
        let prepared = self.prepare_next_block_with_required_burn_and_burn_bundles(
            wallet.address(),
            reward_address.as_deref(),
            timestamp_ms,
            Vec::new(),
            None,
        )?;
        let vdf_output = run_vdf(prepared.vdf_seed(), prepared.vdf_rounds());
        Ok(prepared.finish(wallet, vdf_output))
    }

    pub fn mine_recovery_block(&self, wallet: &Wallet, timestamp_ms: u64) -> Result<super::Block> {
        let reward_address = (self.height().saturating_add(1)
            >= super::HYBRID_REWARD_ACTIVATION_HEIGHT)
            .then(|| {
                wallet.hybrid_address(super::AddressNetwork::from_profile_id(
                    &self.launch_profile.profile_id,
                ))
            });
        let prepared = self.prepare_recovery_block_with_required_burn_and_burn_bundles(
            wallet.address(),
            reward_address.as_deref(),
            timestamp_ms,
            Vec::new(),
            None,
        )?;
        let vdf_output = run_vdf(prepared.vdf_seed(), prepared.vdf_rounds());
        Ok(prepared.finish(wallet, vdf_output))
    }

    pub fn prepare_next_block(&self, miner: &str, timestamp_ms: u64) -> Result<PreparedBlock> {
        self.prepare_next_block_with_burn_bundles(miner, timestamp_ms, Vec::new())
    }

    pub fn prepare_next_block_with_burn_bundles(
        &self,
        miner: &str,
        timestamp_ms: u64,
        burn_bundles: Vec<BurnBundle>,
    ) -> Result<PreparedBlock> {
        self.prepare_next_block_with_required_burn_and_burn_bundles(
            miner,
            None,
            timestamp_ms,
            burn_bundles,
            None,
        )
    }

    pub(crate) fn prepare_next_block_with_required_burn_and_burn_bundles(
        &self,
        miner: &str,
        reward_address: Option<&str>,
        timestamp_ms: u64,
        burn_bundles: Vec<BurnBundle>,
        required_burn_signature: Option<&str>,
    ) -> Result<PreparedBlock> {
        let height = self.tip().height + 1;
        self.validate_reward_address(height, reward_address, "finalizer")?;
        let Some((finalizer_rank, leader_ticket)) = self.finalizer_ticket_for_miner(height, miner)
        else {
            bail!("cannot mine block without a mature burn ticket");
        };
        if self.expected_leader_for_next_block().is_none() {
            bail!("no selected leader for block {height}");
        }

        let burn_bundles =
            self.validate_next_block_burn_bundles_for_finalizer_rank(finalizer_rank, burn_bundles)?;
        let burn_bundle_section = self.burn_bundle_section_from_bundles(burn_bundles)?;
        let selection = self.select_block_transactions_with_burn_section(
            miner,
            required_burn_signature,
            &burn_bundle_section,
        )?;
        ensure_block_has_burn(&selection.transactions, &selection.transactions_v2)?;

        let tip = self.tip();
        let prev_hash = tip.hash.clone();
        let timestamp_ms = timestamp_ms.max(ticket_block_min_timestamp(tip, finalizer_rank)?);
        let bundle_hashes = burn_bundle_section.burn_bundle_hashes(height, &prev_hash, miner);
        let reward = self.expected_reward_for_next_block(
            &selection.transactions,
            &selection.transactions_v2,
            &burn_bundle_section,
        )?;
        let vdf_rounds = self.vdf_rounds_for_finalizer_rank(finalizer_rank)?;
        let content_commitment = vdf_content_commitment(
            height,
            &prev_hash,
            miner,
            reward_address,
            FinalizerMode::Ticket,
            finalizer_rank,
            reward,
            vdf_rounds,
            Some(&leader_ticket.id),
            &burn_bundle_section,
            &selection.transactions,
            &selection.transactions_v2,
        );
        let vdf_seed = vdf_seed_for_child(&prev_hash, height, &bundle_hashes, &content_commitment);
        Ok(PreparedBlock {
            height,
            prev_hash,
            timestamp_ms,
            miner: miner.to_string(),
            reward_address: reward_address.map(str::to_string),
            finalizer_mode: FinalizerMode::Ticket,
            reward,
            vdf_rounds,
            vdf_seed,
            finalizer_rank,
            leader_ticket: Some(leader_ticket),
            burn_bundle_section,
            transactions: selection.transactions,
            transactions_v2: selection.transactions_v2,
        })
    }

    pub fn recovery_block_available_at(&self, timestamp_ms: u64) -> bool {
        timestamp_ms >= self.recovery_block_min_timestamp()
    }

    pub fn recovery_block_min_timestamp(&self) -> u64 {
        self.tip()
            .timestamp_ms
            .saturating_add(RECOVERY_BLOCK_DELAY_MS)
    }

    pub fn prepare_recovery_block(&self, miner: &str, timestamp_ms: u64) -> Result<PreparedBlock> {
        self.prepare_recovery_block_with_burn_bundles(miner, timestamp_ms, Vec::new())
    }

    pub fn prepare_recovery_block_with_burn_bundles(
        &self,
        miner: &str,
        timestamp_ms: u64,
        burn_bundles: Vec<BurnBundle>,
    ) -> Result<PreparedBlock> {
        self.prepare_recovery_block_with_required_burn_and_burn_bundles(
            miner,
            None,
            timestamp_ms,
            burn_bundles,
            None,
        )
    }

    pub(crate) fn prepare_recovery_block_with_required_burn_and_burn_bundles(
        &self,
        miner: &str,
        reward_address: Option<&str>,
        timestamp_ms: u64,
        burn_bundles: Vec<BurnBundle>,
        required_burn_signature: Option<&str>,
    ) -> Result<PreparedBlock> {
        let height = self.tip().height + 1;
        self.validate_reward_address(height, reward_address, "finalizer")?;
        let min_timestamp = self.recovery_block_min_timestamp();
        if timestamp_ms < min_timestamp {
            bail!("recovery block is not available before timestamp {min_timestamp}");
        }

        let burn_bundles = self.validate_next_block_burn_bundles(burn_bundles)?;
        let burn_bundle_section = self.burn_bundle_section_from_bundles(burn_bundles)?;
        let selection = self.select_recovery_block_transactions_with_burn_section(
            miner,
            required_burn_signature,
            &burn_bundle_section,
        )?;
        ensure_block_has_burn(&selection.transactions, &selection.transactions_v2)?;
        ensure_block_has_burn_from(&selection.transactions, &selection.transactions_v2, miner)?;

        let tip = self.tip();
        let prev_hash = tip.hash.clone();
        let timestamp_ms = timestamp_ms.max(tip.timestamp_ms + 1);
        let bundle_hashes = burn_bundle_section.burn_bundle_hashes(height, &prev_hash, miner);
        let reward = self.expected_reward_for_next_block(
            &selection.transactions,
            &selection.transactions_v2,
            &burn_bundle_section,
        )?;
        let vdf_rounds = self.recovery_vdf_rounds()?;
        let content_commitment = vdf_content_commitment(
            height,
            &prev_hash,
            miner,
            reward_address,
            FinalizerMode::Recovery,
            0,
            reward,
            vdf_rounds,
            None,
            &burn_bundle_section,
            &selection.transactions,
            &selection.transactions_v2,
        );
        let vdf_seed = recovery_vdf_seed_for_child(
            &prev_hash,
            height,
            timestamp_ms,
            &bundle_hashes,
            &content_commitment,
        );
        Ok(PreparedBlock {
            height,
            prev_hash,
            timestamp_ms,
            miner: miner.to_string(),
            reward_address: reward_address.map(str::to_string),
            finalizer_mode: FinalizerMode::Recovery,
            finalizer_rank: 0,
            reward,
            vdf_rounds,
            vdf_seed,
            leader_ticket: None,
            burn_bundle_section,
            transactions: selection.transactions,
            transactions_v2: selection.transactions_v2,
        })
    }
}
