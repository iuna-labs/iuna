use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use crate::domain::{
    AddressNetwork, Amount, Block, BurnBundle, DEFAULT_TRANSACTION_FEE, Ledger, OutPoint,
    PreparedBlock, StratumMineShare, StratumMineTemplate, Transaction, TransactionSubmitOutcome,
    TransactionV2, VersionedAddress, Wallet, hex_encode, run_vdf,
};

use super::{
    ExternalMineJob, FeeEstimate, GossipEnvelope, NodeCore, QuantumMigrationPreview,
    helpers::converge_fee_by_byte, now_ms,
};

pub(crate) const LEGACY_RECIPIENT_HYBRID_FUNDS_ERROR: &str = "the recipient uses a legacy address, \
which can only receive legacy funds; your balance is held at hybrid addresses, so ask the \
recipient for their address-v1 (iuna1p…) address";

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
    pub fn preview_quantum_migration(
        &self,
        fee_per_byte: Amount,
    ) -> Result<QuantumMigrationPreview> {
        if fee_per_byte == 0 {
            bail!("migration fee per byte must be greater than zero");
        }
        let ledger = self.wallet_build_ledger()?;
        let wallet = self.wallet.unlocked()?;
        let domain = ledger.transaction_v2_domain()?;
        let initial = ledger.build_v2_migration_batch(wallet, 1)?;
        let bytes = initial.encoded_size_bytes(&domain)?;
        let fee = fee_per_byte
            .checked_mul(bytes as u64)
            .context("migration fee overflows")?;
        let transaction = ledger.build_v2_migration_batch(wallet, fee)?;
        let bytes = transaction.encoded_size_bytes(&domain)?;
        let transaction_id = hex_encode(transaction.transaction_id(&domain)?);
        let (input_count, amount) = match &transaction {
            TransactionV2::Migration {
                inputs, outputs, ..
            } => (
                inputs.len(),
                outputs.iter().try_fold(0_u64, |total, output| {
                    total
                        .checked_add(output.amount)
                        .context("migration amount overflows")
                })?,
            ),
            _ => unreachable!("migration builder returned another transaction kind"),
        };
        Ok(QuantumMigrationPreview {
            address: wallet.hybrid_address(AddressNetwork::from_profile_id(
                &ledger.launch_profile().profile_id,
            )),
            transaction_id,
            input_count,
            remaining_legacy_utxos: ledger
                .available_utxos_for_address(wallet.address())?
                .len()
                .saturating_sub(input_count),
            bytes,
            fee,
            amount,
        })
    }

    pub fn submit_quantum_migration(
        &mut self,
        fee_per_byte: Amount,
        max_fee: Amount,
        expected_transaction_id: &str,
    ) -> Result<QuantumMigrationPreview> {
        let preview = self.preview_quantum_migration(fee_per_byte)?;
        if preview.fee > max_fee || preview.transaction_id != expected_transaction_id {
            bail!("wallet outputs or migration fee changed; request a new preview");
        }
        let ledger = self.wallet_build_ledger()?;
        let transaction = ledger.build_v2_migration_batch(self.wallet.unlocked()?, preview.fee)?;
        self.submit_public_transaction_v2(transaction)?;
        Ok(preview)
    }

    pub fn estimate_hybrid_transfer_fee(
        &self,
        recipient: VersionedAddress,
        amount: Amount,
        fee_per_byte: Amount,
    ) -> Result<FeeEstimate> {
        self.build_hybrid_transfer_with_fee_rate(recipient, amount, fee_per_byte)
            .map(|(_, estimate)| estimate)
    }

    pub fn transfer_hybrid_with_fee_rate(
        &mut self,
        recipient: VersionedAddress,
        amount: Amount,
        fee_per_byte: Amount,
    ) -> Result<String> {
        let (transaction, _) =
            self.build_hybrid_transfer_with_fee_rate(recipient, amount, fee_per_byte)?;
        let domain = self.ledger.transaction_v2_domain()?;
        let transaction_id = hex_encode(transaction.transaction_id(&domain)?);
        self.submit_public_transaction_v2(transaction)?;
        Ok(transaction_id)
    }

    fn build_hybrid_transfer_with_fee_rate(
        &self,
        recipient: VersionedAddress,
        amount: Amount,
        fee_per_byte: Amount,
    ) -> Result<(TransactionV2, FeeEstimate)> {
        if fee_per_byte == 0 {
            bail!("fee per byte must be greater than zero");
        }
        let ledger = self.wallet_build_ledger()?;
        let domain = ledger.transaction_v2_domain()?;
        let wallet = self.wallet.unlocked()?;
        let mut fee = 1;
        for _ in 0..64 {
            let transaction = ledger.build_v2_transfer(wallet, recipient, amount, fee)?;
            let bytes = transaction.encoded_size_bytes(&domain)?;
            let required_fee = fee_per_byte
                .checked_mul(bytes as Amount)
                .context("fee per byte times transaction bytes overflows")?
                .max(1);
            if fee >= required_fee {
                return Ok((transaction, FeeEstimate { bytes, fee }));
            }
            fee = required_fee;
        }
        bail!("hybrid transfer fee did not converge")
    }

    /// Accept a transaction signed by an external/lightweight wallet.
    /// Mining actions are deliberately excluded from the public wallet API.
    pub fn submit_external_wallet_transaction(
        &mut self,
        tx: Transaction,
    ) -> Result<TransactionSubmitOutcome> {
        if matches!(tx, Transaction::Mine { .. }) {
            bail!("the wallet endpoint accepts only transfer and burn transactions");
        }
        let outcome = self.ledger.submit_transaction_with_outcome(tx.clone())?;
        if outcome.added() {
            self.outbox.push(GossipEnvelope::Transaction(tx));
        }
        Ok(outcome)
    }

    /// Accept a canonical v2 envelope signed by an external/lightweight wallet.
    /// Mining and burn transactions remain node-managed operations.
    pub fn submit_external_wallet_transaction_v2(
        &mut self,
        envelope: &str,
    ) -> Result<(String, TransactionSubmitOutcome)> {
        let encoded = crate::domain::decode_hex(envelope)?;
        let transaction = self.ledger.decode_transaction_v2(&encoded)?;
        if !matches!(
            transaction,
            TransactionV2::Migration { .. } | TransactionV2::Transfer { .. }
        ) {
            bail!("the wallet endpoint accepts only migration and transfer v2 transactions");
        }
        let domain = self.ledger.transaction_v2_domain()?;
        let transaction_id = hex_encode(transaction.transaction_id(&domain)?);
        let outcome = self.ledger.submit_transaction_v2(transaction)?;
        if outcome.added() {
            self.outbox.push(GossipEnvelope::TransactionV2 {
                envelope: hex_encode(encoded),
            });
        }
        Ok((transaction_id, outcome))
    }

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
        match self.build_burn_with_fee_rate(amount, fee_per_byte) {
            Ok((_, estimate)) => Ok(estimate),
            Err(legacy_error) => self
                .build_v2_burn_with_fee_rate_on_ledger(
                    &self.wallet_build_ledger()?,
                    amount,
                    fee_per_byte,
                    false,
                )
                .map(|(_, estimate)| estimate)
                .with_context(|| format!("legacy burn unavailable: {legacy_error:#}")),
        }
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
            recipient: self.reward_address_for_next_block()?,
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

    pub(super) fn build_v2_burn_with_fee_rate_on_ledger(
        &self,
        ledger: &Ledger,
        amount: Amount,
        fee_per_byte: Amount,
        for_next_block: bool,
    ) -> Result<(TransactionV2, FeeEstimate)> {
        if fee_per_byte == 0 {
            bail!("fee per byte must be greater than zero");
        }
        let domain = ledger.transaction_v2_domain()?;
        let wallet = self.wallet.unlocked()?;
        let mut fee = 1;
        for _ in 0..64 {
            let transaction = if for_next_block {
                ledger.build_v2_burn_for_next_block(wallet, amount, fee)?
            } else {
                ledger.build_v2_burn(wallet, amount, fee)?
            };
            let bytes = transaction.encoded_size_bytes(&domain)?;
            let required_fee = fee_per_byte
                .checked_mul(bytes as Amount)
                .context("fee per byte times transaction bytes overflows")?
                .max(1);
            if fee >= required_fee {
                return Ok((transaction, FeeEstimate { bytes, fee }));
            }
            fee = required_fee;
        }
        bail!("hybrid burn fee did not converge")
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
        .map_err(|error| self.explain_legacy_transfer_error(error, amount))
    }

    /// Legacy recipients can only be paid from legacy outputs: transaction v2 outputs must use
    /// address v1. Point users at the real cause when their value already sits at hybrid
    /// addresses instead of surfacing a bare insufficient-funds or ownership error.
    fn explain_legacy_transfer_error(&self, error: anyhow::Error, amount: Amount) -> anyhow::Error {
        let legacy_balance = self.ledger.balance_of(self.wallet.address());
        let hybrid_balance = self.balance_of_addresses(&self.wallet_hybrid_addresses());
        if legacy_balance >= amount || hybrid_balance == 0 {
            return error;
        }
        error.context(LEGACY_RECIPIENT_HYBRID_FUNDS_ERROR)
    }

    pub(super) fn wallet_hybrid_addresses(&self) -> Vec<String> {
        self.wallet
            .unlocked()
            .ok()
            .and_then(|wallet| {
                self.ledger
                    .wallet_owned_hybrid_encoded_addresses(wallet)
                    .ok()
            })
            .unwrap_or_default()
    }

    pub(super) fn balance_of_addresses(&self, addresses: &[String]) -> Amount {
        addresses.iter().fold(0, |total, address| {
            total.saturating_add(self.ledger.balance_of(address))
        })
    }

    pub(super) fn build_mine_estimate(&self) -> Result<(Transaction, FeeEstimate)> {
        let tx = self
            .ledger
            .build_mine(self.reward_address_for_next_block()?)?;
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

    pub(super) fn reward_address_for_next_block(&self) -> Result<String> {
        if self.ledger.height().saturating_add(1) < crate::domain::HYBRID_REWARD_ACTIVATION_HEIGHT {
            return Ok(self.wallet.address().to_string());
        }
        Ok(self.ledger.wallet_reward_address(
            self.wallet.unlocked()?,
            self.ledger.height().saturating_add(1),
        ))
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

#[cfg(test)]
mod quantum_migration_tests {
    use std::collections::BTreeMap;

    use crate::{
        app::NodeCore,
        domain::{Ledger, Wallet},
    };

    #[test]
    fn migration_preview_reports_the_canonical_hybrid_transaction() {
        let wallet = Wallet::from_seed("migration-preview-wallet");
        let ledger = Ledger::new(
            BTreeMap::from([(wallet.address().to_string(), 1_000_000)]),
            1,
        );
        let node = NodeCore::from_ledger(wallet, ledger, 0);

        let preview = node.preview_quantum_migration(2).unwrap();

        assert_eq!(preview.input_count, 1);
        assert_eq!(preview.remaining_legacy_utxos, 0);
        assert_eq!(preview.fee, preview.bytes as u64 * 2);
        assert_eq!(preview.amount + preview.fee, 1_000_000);
        assert_eq!(preview.transaction_id.len(), 64);
        assert!(preview.address.starts_with("iuna1p"));
    }
}

#[cfg(test)]
mod legacy_recipient_tests {
    use std::collections::BTreeMap;

    use super::LEGACY_RECIPIENT_HYBRID_FUNDS_ERROR;
    use crate::{
        app::NodeCore,
        domain::{AddressNetwork, Ledger, OutPoint, TxOutput, Wallet},
    };

    #[test]
    fn legacy_transfer_from_hybrid_funds_explains_the_address_version_mismatch() {
        let wallet = Wallet::from_seed("legacy-recipient-hybrid-wallet");
        let recipient = Wallet::from_seed("legacy-recipient").address().to_string();
        let mut ledger = Ledger::new(BTreeMap::new(), 1);
        ledger.insert_utxo_for_test(
            OutPoint {
                txid: "42".repeat(32),
                index: 0,
            },
            TxOutput {
                address: wallet.hybrid_address(AddressNetwork::Mainnet),
                amount: 1_000_000,
            },
        );
        let node = NodeCore::from_ledger(wallet, ledger, 0);

        let error = node
            .estimate_transfer_fee(recipient, 1_000, 1, &[])
            .unwrap_err();

        assert!(
            format!("{error:#}").starts_with(LEGACY_RECIPIENT_HYBRID_FUNDS_ERROR),
            "{error:#}"
        );
    }

    #[test]
    fn legacy_transfer_without_any_funds_keeps_the_plain_error() {
        let wallet = Wallet::from_seed("legacy-recipient-empty-wallet");
        let recipient = Wallet::from_seed("legacy-recipient").address().to_string();
        let ledger = Ledger::new(BTreeMap::new(), 1);
        let node = NodeCore::from_ledger(wallet, ledger, 0);

        let error = node
            .estimate_transfer_fee(recipient, 1_000, 1, &[])
            .unwrap_err();

        assert!(!format!("{error:#}").contains(LEGACY_RECIPIENT_HYBRID_FUNDS_ERROR));
    }
}
