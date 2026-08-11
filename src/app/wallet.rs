use anyhow::{Context, Result, bail};

use crate::domain::{
    Amount, BlindedTransaction, Block, BuiltBlindedTransaction, DEFAULT_TRANSACTION_FEE, Ledger,
    OutPoint, PreparedBlock, RevealBundle, StratumMineShare, StratumMineTemplate, Transaction,
    Wallet, run_vdf,
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
        self.burn_with_fee(amount, 0)
    }

    pub fn burn_with_fee(&mut self, amount: Amount, fee: Amount) -> Result<Transaction> {
        let tx = self
            .wallet_build_ledger()?
            .build_burn(self.wallet.unlocked()?, amount, fee)?;
        self.submit_transaction_as_owned_blinded(tx)
    }

    pub fn burn_with_fee_rate(
        &mut self,
        amount: Amount,
        fee_per_byte: Amount,
    ) -> Result<(Transaction, FeeEstimate)> {
        let (built, estimate) = self.build_blinded_burn_with_fee_rate(amount, fee_per_byte)?;
        let tx = built.payload.clone();
        self.submit_owned_blinded_transaction(built)?;
        Ok((tx, estimate))
    }

    pub fn blinded_burn_with_fee(
        &mut self,
        amount: Amount,
        fee: Amount,
        expires_at_height: u64,
    ) -> Result<BlindedTransaction> {
        let built = self.wallet_build_ledger()?.build_blinded_burn(
            self.wallet.unlocked()?,
            amount,
            fee,
            expires_at_height,
        )?;
        self.submit_owned_blinded_transaction(built)
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
        self.submit_transaction_as_owned_blinded(tx)
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
        self.submit_transaction_as_owned_blinded(tx)
    }

    pub fn blinded_transfer_with_fee(
        &mut self,
        to: impl Into<String>,
        amount: Amount,
        fee: Amount,
        expires_at_height: u64,
    ) -> Result<BlindedTransaction> {
        let built = self.wallet_build_ledger()?.build_blinded_transfer(
            self.wallet.unlocked()?,
            to,
            amount,
            fee,
            expires_at_height,
        )?;
        self.submit_owned_blinded_transaction(built)
    }

    pub fn transfer_with_fee_rate(
        &mut self,
        to: impl Into<String>,
        amount: Amount,
        fee_per_byte: Amount,
        outpoints: &[OutPoint],
    ) -> Result<(Transaction, FeeEstimate)> {
        let (built, estimate) =
            self.build_blinded_transfer_with_fee_rate(to, amount, fee_per_byte, outpoints)?;
        let tx = built.payload.clone();
        self.submit_owned_blinded_transaction(built)?;
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

    pub(super) fn usable_reveal_bundles(&self) -> Vec<RevealBundle> {
        let next_height = self.ledger.height().saturating_add(1);
        let mut bundles = self
            .reveal_bundles
            .iter()
            .filter(|((height, slot), _)| {
                *height == next_height
                    && !self
                        .equivocated_reveal_bundle_slots
                        .contains(&(*height, *slot))
            })
            .map(|(_, bundle)| bundle.clone())
            .collect::<Vec<_>>();
        bundles.sort_by_key(|bundle| bundle.slot);
        bundles
    }

    pub(super) fn prune_reveal_bundles(&mut self) {
        let height = self.ledger.height();
        self.reveal_bundles
            .retain(|(bundle_height, _), _| *bundle_height > height);
        self.equivocated_reveal_bundle_slots
            .retain(|(bundle_height, _)| *bundle_height > height);
    }

    pub(super) fn publish_reveal_bundle_for_next_block(&mut self) -> Result<()> {
        let wallet = match &self.wallet {
            NodeWallet::Unlocked(wallet) => wallet,
            NodeWallet::Locked { .. } => return Ok(()),
        };
        let Some(bundle) = self.ledger.build_reveal_bundle(wallet)? else {
            return Ok(());
        };
        let key = (bundle.height, bundle.slot);
        if self.equivocated_reveal_bundle_slots.contains(&key)
            || self.reveal_bundles.contains_key(&key)
        {
            return Ok(());
        }
        self.ledger
            .validate_next_block_reveal_bundles(vec![bundle.clone()])?;
        self.reveal_bundles.insert(key, bundle.clone());
        self.outbox.push(GossipEnvelope::RevealBundle(bundle));
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
            self.outbox.push(GossipEnvelope::MineAction(tx.clone()));
        }
        Ok(tx)
    }

    pub(super) fn build_burn_with_fee_rate(
        &self,
        amount: Amount,
        fee_per_byte: Amount,
    ) -> Result<(Transaction, FeeEstimate)> {
        let (built, estimate) = self.build_blinded_burn_with_fee_rate(amount, fee_per_byte)?;
        Ok((built.payload, estimate))
    }

    pub(super) fn build_blinded_burn_with_fee_rate(
        &self,
        amount: Amount,
        fee_per_byte: Amount,
    ) -> Result<(BuiltBlindedTransaction, FeeEstimate)> {
        let ledger = self.wallet_build_ledger()?;
        self.build_blinded_burn_with_fee_rate_on_ledger(&ledger, amount, fee_per_byte)
    }

    pub(super) fn build_blinded_burn_with_fee_rate_on_ledger(
        &self,
        ledger: &Ledger,
        amount: Amount,
        fee_per_byte: Amount,
    ) -> Result<(BuiltBlindedTransaction, FeeEstimate)> {
        let expires_at_height = self.default_blinded_transaction_expiry_height();
        converge_fee_by_byte(fee_per_byte, |fee| {
            let tx = ledger.build_burn(self.wallet.unlocked()?, amount, fee)?;
            ledger.build_blinded_transaction(self.wallet.unlocked()?, tx, expires_at_height)
        })
    }

    pub(super) fn build_blinded_burn_with_fee_on_ledger(
        &self,
        ledger: &Ledger,
        amount: Amount,
        fee: Amount,
    ) -> Result<BuiltBlindedTransaction> {
        let expires_at_height = self.default_blinded_transaction_expiry_height();
        let tx = ledger.build_burn(self.wallet.unlocked()?, amount, fee)?;
        ledger.build_blinded_transaction(self.wallet.unlocked()?, tx, expires_at_height)
    }

    pub(super) fn build_transfer_with_fee_rate(
        &self,
        to: impl Into<String>,
        amount: Amount,
        fee_per_byte: Amount,
        outpoints: &[OutPoint],
    ) -> Result<(Transaction, FeeEstimate)> {
        let (built, estimate) =
            self.build_blinded_transfer_with_fee_rate(to, amount, fee_per_byte, outpoints)?;
        Ok((built.payload, estimate))
    }

    pub(super) fn build_blinded_transfer_with_fee_rate(
        &self,
        to: impl Into<String>,
        amount: Amount,
        fee_per_byte: Amount,
        outpoints: &[OutPoint],
    ) -> Result<(BuiltBlindedTransaction, FeeEstimate)> {
        let to = to.into();
        let ledger = self.wallet_build_ledger()?;
        let expires_at_height = self.default_blinded_transaction_expiry_height();
        converge_fee_by_byte(fee_per_byte, |fee| {
            let tx = if outpoints.is_empty() {
                ledger.build_transfer(self.wallet.unlocked()?, to.clone(), amount, fee)
            } else {
                ledger.build_transfer_with_inputs(
                    self.wallet.unlocked()?,
                    to.clone(),
                    amount,
                    fee,
                    outpoints,
                )
            }?;
            ledger.build_blinded_transaction(self.wallet.unlocked()?, tx, expires_at_height)
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
        self.queue_owned_blinded_payloads(&mut ledger)?;
        Ok(ledger)
    }

    pub(super) fn wallet_anchor_build_ledger(&self) -> Result<Ledger> {
        let mut ledger = self.ledger.clone();
        self.reserve_local_block_anchor_inputs(&mut ledger)?;
        ledger.clear_pending_transactions();
        ledger.clear_pending_blinded_transactions();
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
        self.publish_reveal_bundle_for_next_block()?;
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
        self.prune_reveal_bundles();
        self.prune_owned_blinded_payloads_for_block(&block);
        self.outbox.push(GossipEnvelope::Block(block.clone()));
        self.publish_owned_reveals_for_block(&block)?;
        Ok(block)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        app::{FeeEstimate, NodeCore},
        domain::{
            GenesisBurn, Ledger, MICRO_IUNA, MINE_FINALIZER_FEE, Transaction, VDF_TARGET_BLOCK_MS,
            Wallet, run_vdf,
        },
    };

    #[test]
    fn fee_rate_transfer_and_burn_pay_at_least_bytes_times_rate() {
        let transfer_sender = Wallet::from_seed("fee-rate-transfer-sender");
        let transfer_recipient = Wallet::from_seed("fee-rate-transfer-recipient");
        let mut transfer_genesis = BTreeMap::new();
        transfer_genesis.insert(transfer_sender.address().to_string(), 10 * MICRO_IUNA);
        let transfer_ledger = Ledger::new_with_genesis_burns(
            transfer_genesis,
            vec![GenesisBurn::new(transfer_sender.address(), MICRO_IUNA)],
            1,
        )
        .unwrap();
        let mut transfer_node = NodeCore::from_ledger(transfer_sender, transfer_ledger, 0);

        let (transfer, transfer_estimate) = transfer_node
            .transfer_with_fee_rate(transfer_recipient.address(), MICRO_IUNA, 2, &[])
            .unwrap();
        let transfer_blinded_bytes =
            transfer_node.ledger().pending_blinded_transactions()[0].fee_rate_size_bytes();
        assert_eq!(transfer_estimate.bytes, transfer_blinded_bytes);
        let minimum_transfer_fee = transfer_blinded_bytes as u64 * 2;
        assert!(transfer.fee() >= minimum_transfer_fee);
        assert!(transfer_node.ledger().pending().is_empty());
        assert_eq!(
            transfer_node.ledger().pending_blinded_transactions().len(),
            1
        );

        let burn_wallet = Wallet::from_seed("fee-rate-burn-wallet");
        let mut burn_genesis = BTreeMap::new();
        burn_genesis.insert(burn_wallet.address().to_string(), 10 * MICRO_IUNA);
        let burn_ledger = Ledger::new_with_genesis_burns(
            burn_genesis,
            vec![GenesisBurn::new(burn_wallet.address(), MICRO_IUNA)],
            1,
        )
        .unwrap();
        let mut burn_node = NodeCore::from_ledger(burn_wallet, burn_ledger, 0);
        let (burn, burn_estimate) = burn_node.burn_with_fee_rate(MICRO_IUNA, 3).unwrap();
        let burn_blinded_bytes =
            burn_node.ledger().pending_blinded_transactions()[0].fee_rate_size_bytes();
        assert_eq!(burn_estimate.bytes, burn_blinded_bytes);
        let minimum_burn_fee = burn_blinded_bytes as u64 * 3;
        assert!(burn.fee() >= minimum_burn_fee);
        assert!(burn_node.ledger().pending().is_empty());
        assert_eq!(burn_node.ledger().pending_blinded_transactions().len(), 1);
    }

    #[test]
    fn mine_fee_estimate_uses_template_without_searching_pow() {
        let wallet = Wallet::from_seed("mine-fee-template-wallet");
        let mut genesis = BTreeMap::new();
        genesis.insert(wallet.address().to_string(), MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            genesis,
            vec![GenesisBurn::new(wallet.address(), MICRO_IUNA)],
            1,
        )
        .unwrap();
        let node = NodeCore::from_ledger(wallet, ledger, 0);

        let estimate = node.estimate_mine_fee(MINE_FINALIZER_FEE).unwrap();

        assert_eq!(
            estimate,
            FeeEstimate {
                bytes: Transaction::Mine {
                    recipient: node.wallet_address().to_string(),
                    anchor: node.ledger().tip_hash().to_string(),
                    salt: 0,
                    nonce: 0,
                    difficulty_bits: node.ledger().current_mine_difficulty_bits(),
                    proof_header: None,
                    signature: "0".repeat(64),
                }
                .economic_size_bytes(),
                fee: MINE_FINALIZER_FEE,
            }
        );
    }

    #[test]
    fn wallet_building_reserves_local_anchor_burn_inputs() {
        let alice = Wallet::from_seed("local-anchor-reserve-alice");
        let finalizers = [alice.clone()];
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        let mut ledger = Ledger::new_with_genesis_burns(
            allocations,
            finalizers
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
                .collect(),
            1,
        )
        .unwrap();
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let leader_wallet = finalizers
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap()
            .clone();
        let split = ledger
            .build_transfer(&leader_wallet, leader_wallet.address(), MICRO_IUNA, 0)
            .unwrap();
        ledger.submit_transaction(split).unwrap();
        let anchor = ledger.build_burn(&leader_wallet, 1, 0).unwrap();
        ledger.submit_transaction(anchor).unwrap();
        let split_block = ledger.mine_next_block(&leader_wallet, 1).unwrap();
        ledger.apply_block(split_block).unwrap();
        let mut node =
            NodeCore::from_ledger_with_burn_fee_and_enabled(leader_wallet, ledger, true, 0, 1);

        let plan = node.prepare_automatic_finalization(1);
        assert!(plan.burned.is_some());
        assert!(node.status().wallet_balance < node.ledger().balance_of(node.wallet_address()));
        let (_, anchor_burn) = node
            .local_block_anchor_burn
            .clone()
            .expect("leader burn should be held as a local block anchor");
        let Transaction::Burn { inputs, .. } = anchor_burn else {
            panic!("local block anchor must be a burn");
        };
        let anchor_inputs = inputs
            .iter()
            .map(|input| input.outpoint.clone())
            .collect::<Vec<_>>();

        let blinded = node
            .blinded_burn_with_fee(MICRO_IUNA / 20, 1, node.chain_height() + 4)
            .unwrap();

        assert!(
            blinded
                .inputs
                .iter()
                .all(|input| !anchor_inputs.contains(&input.outpoint)),
            "blinded wallet transactions must not spend inputs reserved by the local anchor burn"
        );

        let work = node.prepare_next_block_with_local_anchor(2).unwrap();
        let vdf_output = run_vdf(work.vdf_seed(), work.vdf_rounds());
        let mut peer_ledger = node.clone_ledger();
        let block = node
            .complete_prepared_block_at(work, vdf_output, VDF_TARGET_BLOCK_MS * 2)
            .unwrap();
        assert!(block.transactions.iter().any(Transaction::is_burn));
        assert!(
            block
                .blinded_transactions
                .iter()
                .any(|transaction| transaction.commitment == blinded.commitment)
        );
        peer_ledger.apply_block_at(block, u64::MAX).unwrap();
        assert_eq!(
            node.ledger().status().tip_hash,
            peer_ledger.status().tip_hash
        );
    }
}
