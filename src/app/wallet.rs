use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use crate::domain::{
    Amount, Block, BurnBundle, DEFAULT_TRANSACTION_FEE, Ledger, OutPoint, PreparedBlock,
    StratumMineShare, StratumMineTemplate, Transaction, Wallet, run_vdf,
};

use super::{
    ExternalMineJob, FeeEstimate, GossipEnvelope, NodeCore, helpers::converge_fee_by_byte, now_ms,
};

#[derive(Clone, Debug)]
pub(super) enum NodeWallet {
    Unlocked(Wallet),
    Locked { address: String },
}

impl NodeWallet {
    pub(super) fn address(&self) -> &str {
        match self {
            Self::Unlocked(wallet) => wallet.address(),
            Self::Locked { address } => address,
        }
    }

    pub(super) fn unlocked(&self) -> Result<&Wallet> {
        match self {
            Self::Unlocked(wallet) => Ok(wallet),
            Self::Locked { .. } => bail!("wallet is locked"),
        }
    }

    pub(super) fn is_locked(&self) -> bool {
        matches!(self, Self::Locked { .. })
    }
}

impl NodeCore {
    pub fn burn(&mut self, amount: Amount) -> Result<Transaction> {
        self.burn_with_fee(amount, DEFAULT_TRANSACTION_FEE)
    }

    pub fn burn_with_fee(&mut self, amount: Amount, fee: Amount) -> Result<Transaction> {
        let tx = self
            .wallet_build_ledger()?
            .build_burn(self.wallet.unlocked()?, amount, fee)?;
        self.submit_public_transaction(tx)
    }

    pub fn burn_with_fee_rate(
        &mut self,
        amount: Amount,
        fee_per_byte: Amount,
    ) -> Result<(Transaction, FeeEstimate)> {
        let (tx, estimate) = self.build_burn_with_fee_rate(amount, fee_per_byte)?;
        self.submit_public_transaction(tx.clone())?;
        Ok((tx, estimate))
    }

    pub fn estimate_burn_fee(&self, amount: Amount, fee_per_byte: Amount) -> Result<FeeEstimate> {
        self.build_burn_with_fee_rate(amount, fee_per_byte)
            .map(|(_, estimate)| estimate)
    }

    pub fn transfer(&mut self, to: impl Into<String>, amount: Amount) -> Result<Transaction> {
        self.transfer_with_fee(to, amount, DEFAULT_TRANSACTION_FEE)
    }

    pub fn transfer_with_fee(
        &mut self,
        to: impl Into<String>,
        amount: Amount,
        fee: Amount,
    ) -> Result<Transaction> {
        let tx =
            self.wallet_build_ledger()?
                .build_transfer(self.wallet.unlocked()?, to, amount, fee)?;
        self.submit_public_transaction(tx)
    }

    pub fn transfer_with_fee_spending(
        &mut self,
        to: impl Into<String>,
        amount: Amount,
        fee: Amount,
        outpoints: &[OutPoint],
    ) -> Result<Transaction> {
        let tx = self.wallet_build_ledger()?.build_transfer_with_inputs(
            self.wallet.unlocked()?,
            to,
            amount,
            fee,
            outpoints,
        )?;
        self.submit_public_transaction(tx)
    }

    pub fn transfer_with_fee_rate(
        &mut self,
        to: impl Into<String>,
        amount: Amount,
        fee_per_byte: Amount,
        outpoints: &[OutPoint],
    ) -> Result<(Transaction, FeeEstimate)> {
        let (tx, estimate) =
            self.build_transfer_with_fee_rate(to, amount, fee_per_byte, outpoints)?;
        self.submit_public_transaction(tx.clone())?;
        Ok((tx, estimate))
    }

    pub fn estimate_transfer_fee(
        &self,
        to: impl Into<String>,
        amount: Amount,
        fee_per_byte: Amount,
        outpoints: &[OutPoint],
    ) -> Result<FeeEstimate> {
        self.build_transfer_with_fee_rate(to, amount, fee_per_byte, outpoints)
            .map(|(_, estimate)| estimate)
    }

    pub fn mine_pow_reward(&mut self) -> Result<Transaction> {
        let (tx, _) = self.build_mine_estimate()?;
        self.submit_public_mine_action(tx)
    }

    pub fn estimate_mine_fee(&self, _fee_per_byte: Amount) -> Result<FeeEstimate> {
        let tx = Transaction::Mine {
            recipient: self.wallet.address().to_string(),
            anchor: self.ledger.tip_hash().to_string(),
            salt: 0,
            nonce: 0,
            difficulty_bits: self.ledger.current_mine_difficulty_bits(),
            proof_header: None,
            signature: "0".repeat(64),
        };
        Ok(FeeEstimate {
            bytes: tx.economic_size_bytes(),
            fee: tx.fee(),
        })
    }

    pub fn external_mine_job(
        &self,
        recipient: impl Into<String>,
        salt: u64,
    ) -> Result<ExternalMineJob> {
        let recipient = recipient.into();
        let tip = self
            .chain()
            .last()
            .context("cannot build mine job without a chain tip")?;
        let difficulty_bits = self.ledger.current_mine_difficulty_bits();
        Ok(ExternalMineJob {
            template: self.ledger.stratum_mine_template(
                recipient,
                &tip.hash,
                salt,
                difficulty_bits,
            )?,
        })
    }

    pub fn submit_external_mine(
        &mut self,
        recipient: impl Into<String>,
        template: StratumMineTemplate,
        share: StratumMineShare,
    ) -> Result<Transaction> {
        let tx = self.ledger.build_stratum_mine(template, share)?;
        let recipient = recipient.into();
        if tx.to() != Some(recipient.as_str()) {
            bail!("submitted mine recipient does not match worker");
        }
        self.submit_public_mine_action(tx)
    }

    pub(super) fn usable_burn_bundles(&self) -> Vec<BurnBundle> {
        let next_height = self.ledger.height().saturating_add(1);
        let mut bundles = self
            .burn_bundles
            .iter()
            .filter(|((height, slot, member), _)| {
                *height == next_height
                    && !self.equivocated_burn_bundle_slots.contains(&(
                        *height,
                        *slot,
                        member.clone(),
                    ))
            })
            .map(|(_, bundle)| bundle.clone())
            .collect::<Vec<_>>();
        bundles.sort_by_key(|bundle| bundle.slot);
        bundles
    }

    pub(super) fn usable_burn_bundles_for_finalizer_rank(
        &self,
        finalizer_rank: u32,
    ) -> Vec<BurnBundle> {
        let committee = self
            .ledger
            .burn_committee_for_next_ticket_block(finalizer_rank)
            .into_iter()
            .map(|member| (member.slot, member.owner))
            .collect::<BTreeMap<_, _>>();
        self.usable_burn_bundles()
            .into_iter()
            .filter(|bundle| {
                committee
                    .get(&bundle.slot)
                    .is_some_and(|owner| *owner == bundle.member)
            })
            .collect()
    }

    pub(crate) fn burn_bundles_for_request(
        &self,
        height: u64,
        prev_hash: &str,
        slots: &[u8],
    ) -> Vec<BurnBundle> {
        self.usable_burn_bundles()
            .into_iter()
            .filter(|bundle| bundle.height == height && bundle.prev_hash == prev_hash)
            .filter(|bundle| slots.is_empty() || slots.contains(&bundle.slot))
            .collect()
    }

    pub(super) fn prune_burn_bundles(&mut self) {
        let height = self.ledger.height();
        self.burn_bundles
            .retain(|(bundle_height, _, _), _| *bundle_height > height);
        self.equivocated_burn_bundle_slots
            .retain(|(bundle_height, _, _)| *bundle_height > height);
    }

    pub(super) fn publish_burn_bundle_for_next_block(&mut self) -> Result<()> {
        let wallet = match &self.wallet {
            NodeWallet::Unlocked(wallet) => wallet,
            NodeWallet::Locked { .. } => return Ok(()),
        };
        let (ledger, _) = self.ledger_with_local_block_anchor();
        for bundle in ledger.build_burn_bundles(wallet)? {
            let key = (bundle.height, bundle.slot, bundle.member.clone());
            if self.equivocated_burn_bundle_slots.contains(&key) {
                continue;
            }
            if let Some(existing) = self.burn_bundles.get(&key) {
                self.outbox
                    .push(GossipEnvelope::BurnBundle(existing.clone()));
                continue;
            }
            ledger.validate_next_block_burn_bundles(vec![bundle.clone()])?;
            self.burn_bundles.insert(key, bundle.clone());
            self.outbox.push(GossipEnvelope::BurnBundle(bundle));
        }
        Ok(())
    }

    pub(super) fn submit_public_mine_action(&mut self, tx: Transaction) -> Result<Transaction> {
        if !matches!(tx, Transaction::Mine { .. }) {
            bail!("only mine actions may be submitted as public mempool transactions");
        }
        if self
            .ledger
            .submit_transaction_with_outcome(tx.clone())?
            .added()
        {
            self.outbox.push(GossipEnvelope::Transaction(tx.clone()));
        }
        Ok(tx)
    }

    pub(super) fn submit_public_transaction(&mut self, tx: Transaction) -> Result<Transaction> {
        if matches!(tx, Transaction::Mine { .. }) {
            return self.submit_public_mine_action(tx);
        }
        if self
            .ledger
            .submit_transaction_with_outcome(tx.clone())?
            .added()
        {
            self.outbox.push(GossipEnvelope::Transaction(tx.clone()));
        }
        Ok(tx)
    }

    pub(super) fn build_burn_with_fee_rate(
        &self,
        amount: Amount,
        fee_per_byte: Amount,
    ) -> Result<(Transaction, FeeEstimate)> {
        let ledger = self.wallet_build_ledger()?;
        self.build_burn_with_fee_rate_on_ledger(&ledger, amount, fee_per_byte)
    }

    pub(super) fn build_burn_with_fee_rate_on_ledger(
        &self,
        ledger: &Ledger,
        amount: Amount,
        fee_per_byte: Amount,
    ) -> Result<(Transaction, FeeEstimate)> {
        converge_fee_by_byte(fee_per_byte, |fee| {
            ledger.build_burn(self.wallet.unlocked()?, amount, fee)
        })
    }

    pub(super) fn build_transfer_with_fee_rate(
        &self,
        to: impl Into<String>,
        amount: Amount,
        fee_per_byte: Amount,
        outpoints: &[OutPoint],
    ) -> Result<(Transaction, FeeEstimate)> {
        let to = to.into();
        let ledger = self.wallet_build_ledger()?;
        converge_fee_by_byte(fee_per_byte, |fee| {
            if outpoints.is_empty() {
                ledger.build_transfer(self.wallet.unlocked()?, to.clone(), amount, fee)
            } else {
                ledger.build_transfer_with_inputs(
                    self.wallet.unlocked()?,
                    to.clone(),
                    amount,
                    fee,
                    outpoints,
                )
            }
        })
    }

    pub(super) fn build_mine_estimate(&self) -> Result<(Transaction, FeeEstimate)> {
        let tx = self.ledger.build_mine(self.wallet.address())?;
        Ok((
            tx.clone(),
            FeeEstimate {
                bytes: tx.economic_size_bytes(),
                fee: tx.fee(),
            },
        ))
    }

    pub(super) fn wallet_build_ledger(&self) -> Result<Ledger> {
        let mut ledger = self.ledger.clone();
        self.reserve_local_block_anchor_inputs(&mut ledger)?;
        Ok(ledger)
    }

    pub(super) fn wallet_anchor_build_ledger(&self) -> Result<Ledger> {
        let mut ledger = self.ledger.clone();
        self.reserve_local_block_anchor_inputs(&mut ledger)?;
        ledger.clear_pending_transactions();
        Ok(ledger)
    }

    pub(super) fn queue_local_block_anchor(&self, ledger: &mut Ledger) -> Result<()> {
        let Some((height, burn)) = &self.local_block_anchor_burn else {
            return Ok(());
        };
        if *height == ledger.height() && !ledger.has_transaction(burn.signature()) {
            let _ = ledger.submit_transaction(burn.clone())?;
        }
        Ok(())
    }

    pub(super) fn reserve_local_block_anchor_inputs(&self, ledger: &mut Ledger) -> Result<()> {
        let Some((height, burn)) = &self.local_block_anchor_burn else {
            return Ok(());
        };
        if *height == ledger.height() && !ledger.has_transaction(burn.signature()) {
            let _ = ledger.reserve_transaction_inputs(burn);
        }
        Ok(())
    }

    pub fn mine_one(&mut self) -> Result<Block> {
        self.mine_one_at(now_ms())
    }

    pub fn mine_one_at(&mut self, timestamp_ms: u64) -> Result<Block> {
        self.publish_burn_bundle_for_next_block()?;
        let work = self.prepare_next_block_with_local_anchor(timestamp_ms)?;
        let vdf_output = run_vdf(work.vdf_seed(), work.vdf_rounds());
        self.complete_prepared_block_at(work, vdf_output, timestamp_ms)
    }

    pub fn complete_prepared_block(
        &mut self,
        work: PreparedBlock,
        vdf_output: String,
    ) -> Result<Block> {
        self.complete_prepared_block_at(work, vdf_output, now_ms())
    }

    pub fn complete_prepared_block_at(
        &mut self,
        work: PreparedBlock,
        vdf_output: String,
        timestamp_ms: u64,
    ) -> Result<Block> {
        let block = work.finish_at(self.wallet.unlocked()?, vdf_output, timestamp_ms);
        self.ledger.apply_locally_mined_block(block.clone())?;
        self.clear_stale_local_block_anchor();
        self.prune_burn_bundles();
        self.outbox.push(GossipEnvelope::Block(block.clone()));
        Ok(block)
    }

    pub fn precheck_prepared_block_without_vdf_at(
        &self,
        work: &PreparedBlock,
        timestamp_ms: u64,
    ) -> Result<()> {
        let block = work.clone().finish_at(
            self.wallet.unlocked()?,
            "precheck-vdf-output".to_string(),
            timestamp_ms,
        );
        self.ledger
            .block_requires_vdf_verification_at(&block, timestamp_ms)?;
        Ok(())
    }
}
