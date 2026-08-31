use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};

use super::ledger_mempool::pending_pool_item_bytes;
use super::ledger_ops::{
    apply_spendable_pending_transaction, apply_transaction, best_selectable_burn_from_index,
    best_selectable_transaction_index, compact_block_context, ensure_transaction_fits_empty_block,
    estimated_block_selection_size_bytes, transaction_has_missing_inputs,
    validate_transaction_inputs, validate_transaction_outputs,
};
use super::mine_policy::{
    MINE_MAX_ANCHOR_AGE_BLOCKS, mine_anchor, mine_anchor_count_before_height,
};
use super::selection::{BlockSelection, TransactionKind};
use super::transaction::{
    UnsignedTxInput, transaction_inputs_available, transaction_inputs_spent_by,
};
use super::validation::{
    validate_address, validate_hash, validate_signature, validate_stratum_header,
};
use super::{
    Amount, BurnBundleSection, Ledger, MAX_PENDING_POOL_BYTES, MAX_PENDING_TRANSACTIONS,
    MINE_ACTIONS_PER_ANCHOR_LIMIT, OutPoint, Transaction, TxOutput,
};

pub(crate) const MINE_ANCHOR_LIMIT_REACHED: &str = "mine transaction anchor limit reached";

impl Ledger {
    pub(super) fn valid_pending_transactions(&self) -> Vec<Transaction> {
        let mut utxos = self.utxos.clone();
        let signing_domain = self.transaction_signing_domain();
        let mut valid = Vec::new();
        let mut remaining = self.pending.iter().collect::<Vec<_>>();
        let mut selected_mine_anchor_counts = BTreeMap::new();

        while !remaining.is_empty() {
            let mut progressed = false;
            let mut still_pending = Vec::new();

            for tx in remaining {
                if let Some(anchor) = mine_anchor(tx) {
                    let selected = selected_mine_anchor_counts
                        .get(anchor)
                        .copied()
                        .unwrap_or_default();
                    if mine_anchor_count_before_height(&self.chain, anchor, self.height())
                        .saturating_add(selected)
                        >= MINE_ACTIONS_PER_ANCHOR_LIMIT
                    {
                        continue;
                    }
                }
                if transaction_inputs_available(tx, &utxos)
                    && self.validate_transaction_terms(tx).is_ok()
                    && self
                        .validate_transaction_anchor_for_height(
                            tx,
                            self.height().saturating_add(1),
                            self.tip_hash(),
                        )
                        .is_ok()
                    && apply_transaction(tx, &mut utxos, &signing_domain).is_ok()
                {
                    if let Some(anchor) = mine_anchor(tx) {
                        selected_mine_anchor_counts
                            .entry(anchor)
                            .and_modify(|count| *count += 1)
                            .or_insert(1);
                    }
                    valid.push(tx.clone());
                    progressed = true;
                } else {
                    still_pending.push(tx);
                }
            }

            if !progressed {
                break;
            }

            remaining = still_pending;
        }

        valid
    }

    pub(super) fn select_block_transactions_with_burn_section(
        &self,
        miner: &str,
        required_burn_signature: Option<&str>,
        burn_bundle_section: &BurnBundleSection,
    ) -> Result<BlockSelection> {
        self.select_block_transactions_with_required_burn_owner(
            Some(miner),
            required_burn_signature,
            burn_bundle_section,
        )
    }

    pub(super) fn select_recovery_block_transactions_with_burn_section(
        &self,
        miner: &str,
        required_burn_signature: Option<&str>,
        burn_bundle_section: &BurnBundleSection,
    ) -> Result<BlockSelection> {
        self.select_block_transactions_with_required_burn_owner(
            Some(miner),
            required_burn_signature,
            burn_bundle_section,
        )
    }

    pub(super) fn select_block_transactions_with_required_burn_owner(
        &self,
        required_burn_owner: Option<&str>,
        required_burn_signature: Option<&str>,
        burn_bundle_section: &BurnBundleSection,
    ) -> Result<BlockSelection> {
        let block_context = compact_block_context(self);
        let signing_domain = self.transaction_signing_domain();
        let mut utxos = self.utxos.clone();
        let mut remaining = self.valid_pending_transactions();
        let mut selected = Vec::new();

        let required_burn_signatures = burn_bundle_section
            .required_burns()
            .into_iter()
            .map(|burn| burn.signature().to_string())
            .collect::<BTreeSet<_>>();
        let mut selected_required_burn_signatures = BTreeSet::new();

        let anchor_index = if let Some(signature) = required_burn_signature {
            Some(
                remaining
                    .iter()
                    .position(|transaction| transaction.signature() == signature)
                    .with_context(|| format!("required burn {signature} is not pending"))?,
            )
        } else if let Some(owner) = required_burn_owner {
            best_selectable_burn_from_index(&remaining, &utxos, owner, &signing_domain)
        } else {
            best_selectable_transaction_index(
                &remaining,
                &utxos,
                Some(TransactionKind::Burn),
                &signing_domain,
            )
        };
        if let Some(index) = anchor_index {
            let tx = remaining.remove(index);
            let signature = tx.signature().to_string();
            self.select_required_anchor_burn(tx, required_burn_owner, &mut utxos, &mut selected)?;
            if required_burn_signatures.contains(&signature) {
                selected_required_burn_signatures.insert(signature);
            }
        }

        let mut index = 0;
        while index < remaining.len()
            && selected_required_burn_signatures.len() < required_burn_signatures.len()
        {
            if !required_burn_signatures.contains(remaining[index].signature()) {
                index += 1;
                continue;
            }
            let tx = remaining.remove(index);
            if !tx.is_burn() {
                bail!("attested transaction must be a burn");
            }
            if selected.len() >= self.launch_profile.max_block_transactions {
                bail!("attested burns do not fit within the block transaction count limit");
            }
            let signature = tx.signature().to_string();
            apply_transaction(&tx, &mut utxos, &signing_domain)
                .context("attested burn is not spendable")?;
            selected.push(tx);
            selected_required_burn_signatures.insert(signature);
        }
        if selected_required_burn_signatures.len() != required_burn_signatures.len() {
            let missing = required_burn_signatures
                .difference(&selected_required_burn_signatures)
                .next()
                .expect("required burn set differs");
            bail!("attested burn {missing} is not pending");
        }

        let required_selection = BlockSelection {
            transactions: selected.clone(),
        };
        if estimated_block_selection_size_bytes(
            block_context,
            &required_selection,
            required_burn_owner.is_some(),
            burn_bundle_section,
        )? > self.launch_profile.max_block_bytes
        {
            bail!("required block content does not fit in the block");
        }

        while selected.len() < self.launch_profile.max_block_transactions {
            let Some(index) =
                best_selectable_transaction_index(&remaining, &utxos, None, &signing_domain)
            else {
                break;
            };
            let tx = remaining.remove(index);
            let mut candidate = BlockSelection {
                transactions: selected.clone(),
            };
            candidate.transactions.push(tx.clone());
            if estimated_block_selection_size_bytes(
                block_context,
                &candidate,
                required_burn_owner.is_some(),
                burn_bundle_section,
            )? <= self.launch_profile.max_block_bytes
            {
                apply_transaction(&tx, &mut utxos, &signing_domain)?;
                selected.push(tx);
            }
        }
        Ok(BlockSelection {
            transactions: selected,
        })
    }

    fn select_required_anchor_burn(
        &self,
        tx: Transaction,
        required_burn_owner: Option<&str>,
        utxos: &mut BTreeMap<OutPoint, TxOutput>,
        selected: &mut Vec<Transaction>,
    ) -> Result<()> {
        if !tx.is_burn() {
            bail!("required block anchor must be a burn transaction");
        }
        if let Some(owner) = required_burn_owner {
            if tx.sender() != owner {
                bail!("required block anchor burn must be from the recovery finalizer");
            }
        }
        if selected.len() >= self.launch_profile.max_block_transactions {
            bail!("required block anchor burn does not fit within the transaction count limit");
        }
        apply_transaction(&tx, utxos, &self.transaction_signing_domain())
            .context("required block anchor burn is not spendable")?;
        selected.push(tx);
        Ok(())
    }

    pub(super) fn select_inputs(
        &self,
        address: &str,
        amount: Amount,
    ) -> Result<(Vec<UnsignedTxInput>, Amount)> {
        let utxos = self.utxos_after_spendable_pending()?;
        let mut selected = Vec::new();
        let mut total = 0_u64;
        for (outpoint, output) in &utxos {
            if output.address != address {
                continue;
            }
            selected.push(UnsignedTxInput {
                outpoint: outpoint.clone(),
                owner: address.to_string(),
            });
            total = total
                .checked_add(output.amount)
                .context("selected input total overflows")?;
            if total >= amount {
                return Ok((selected, total));
            }
        }
        bail!("insufficient funds for {address}")
    }

    pub(super) fn select_inputs_by_outpoint(
        &self,
        address: &str,
        amount: Amount,
        outpoints: &[OutPoint],
    ) -> Result<(Vec<UnsignedTxInput>, Amount)> {
        if outpoints.is_empty() {
            bail!("at least one UTXO must be selected");
        }
        let utxos = self.utxos_after_spendable_pending()?;
        let mut seen = BTreeSet::new();
        let mut selected = Vec::new();
        let mut total = 0_u64;
        for outpoint in outpoints {
            if !seen.insert(outpoint.clone()) {
                bail!("selected UTXO {} is duplicated", outpoint.id());
            }
            let output = utxos
                .get(outpoint)
                .with_context(|| format!("selected UTXO {} is not spendable", outpoint.id()))?;
            if output.address != address {
                bail!("selected UTXO {} is not owned by {address}", outpoint.id());
            }
            selected.push(UnsignedTxInput {
                outpoint: outpoint.clone(),
                owner: address.to_string(),
            });
            total = total
                .checked_add(output.amount)
                .context("selected input total overflows")?;
        }
        if total < amount {
            bail!("selected UTXOs do not cover amount plus fee");
        }
        Ok((selected, total))
    }

    pub(super) fn validate_new_transaction(&self, transaction: &Transaction) -> Result<()> {
        self.validate_transaction_terms(transaction)?;
        self.validate_transaction_anchor_for_height(
            transaction,
            self.height().saturating_add(1),
            self.tip_hash(),
        )?;
        ensure_transaction_fits_empty_block(
            compact_block_context(self),
            transaction,
            self.launch_profile.max_block_bytes,
        )?;
        self.validate_mine_anchor_available(transaction)?;
        let mut utxos = self.utxos_after_spendable_pending()?;
        apply_transaction(transaction, &mut utxos, &self.transaction_signing_domain())
    }

    pub(super) fn validate_mine_anchor_available(&self, transaction: &Transaction) -> Result<()> {
        if let Some(anchor) = mine_anchor(transaction) {
            let known_count = mine_anchor_count_before_height(&self.chain, anchor, self.height())
                .saturating_add(
                    self.pending
                        .iter()
                        .filter(|tx| mine_anchor(tx) == Some(anchor))
                        .count(),
                )
                .saturating_add(
                    self.orphans
                        .iter()
                        .filter(|tx| {
                            mine_anchor(tx) == Some(anchor)
                                && tx.signature() != transaction.signature()
                        })
                        .count(),
                );
            if known_count >= MINE_ACTIONS_PER_ANCHOR_LIMIT {
                bail!(MINE_ANCHOR_LIMIT_REACHED);
            }
        }
        Ok(())
    }

    pub(super) fn promote_orphan_transactions(&mut self) -> Result<()> {
        let signing_domain = self.transaction_signing_domain();
        loop {
            if self.pending.len() >= MAX_PENDING_TRANSACTIONS {
                return Ok(());
            }
            let mut promoted = None;
            let mut utxos = self.utxos_after_valid_pending()?;
            for (index, transaction) in self.orphans.iter().enumerate() {
                if transaction_inputs_spent_by(transaction, &self.pending) {
                    continue;
                }
                if transaction_has_missing_inputs(transaction, &utxos) {
                    continue;
                }
                if self.validate_new_transaction(transaction).is_ok()
                    && apply_transaction(transaction, &mut utxos, &signing_domain).is_ok()
                {
                    let transaction_bytes = pending_pool_item_bytes(transaction)?;
                    let promoted_bytes = self
                        .pending_bytes
                        .checked_add(transaction_bytes)
                        .context("pending pool byte size overflow")?;
                    if promoted_bytes > MAX_PENDING_POOL_BYTES {
                        continue;
                    }
                    promoted = Some((index, transaction_bytes));
                    break;
                }
            }

            let Some((index, transaction_bytes)) = promoted else {
                return Ok(());
            };
            let transaction = self.orphans.remove(index);
            self.orphan_bytes = self.orphan_bytes.saturating_sub(transaction_bytes);
            self.pending.push(transaction);
            self.pending_bytes = self.pending_bytes.saturating_add(transaction_bytes);
        }
    }

    pub(super) fn validate_transaction_terms(&self, transaction: &Transaction) -> Result<()> {
        match transaction {
            Transaction::Transfer {
                inputs,
                outputs,
                fee,
                signature,
                ..
            } => {
                if *fee == 0 {
                    bail!("transfer transaction fee must be greater than zero");
                }
                validate_transaction_inputs(inputs)?;
                validate_transaction_outputs(outputs)?;
                validate_signature(signature, "transaction signature")?;
            }
            Transaction::Burn {
                inputs,
                change,
                fee,
                anchor,
                signature,
                ..
            } => {
                if *fee == 0 {
                    bail!("burn transaction fee must be greater than zero");
                }
                validate_transaction_inputs(inputs)?;
                validate_transaction_outputs(change)?;
                if let Some(anchor) = anchor {
                    validate_hash(anchor, "burn transaction anchor")?;
                }
                validate_signature(signature, "transaction signature")?;
            }
            Transaction::Mine {
                recipient,
                anchor,
                difficulty_bits,
                proof_header,
                signature,
                ..
            } => {
                validate_address(recipient, "mine recipient")?;
                validate_hash(anchor, "mine transaction anchor")?;
                validate_hash(signature, "mine transaction proof hash")?;
                if let Some(proof_header) = proof_header {
                    validate_stratum_header(proof_header)?;
                }
                let anchor_block = self
                    .chain
                    .iter()
                    .find(|block| block.hash == *anchor)
                    .context("mine transaction anchor is not on this chain")?;
                let anchor_age = self.tip().height.saturating_sub(anchor_block.height);
                if anchor_age > MINE_MAX_ANCHOR_AGE_BLOCKS {
                    bail!("mine transaction anchor is too old");
                }
                let required_difficulty =
                    self.mine_difficulty_bits_for_anchor_height(anchor_block.height);
                if *difficulty_bits != required_difficulty {
                    bail!("mine transaction difficulty is invalid");
                }
            }
        }
        Ok(())
    }

    pub(super) fn validate_transaction_anchor_for_height(
        &self,
        transaction: &Transaction,
        height: u64,
        parent_hash: &str,
    ) -> Result<()> {
        if !transaction.is_burn() {
            return Ok(());
        }
        if height < super::TIP_BOUND_BURN_ACTIVATION_HEIGHT {
            if transaction.burn_anchor().is_some() {
                bail!("burn transaction anchor is not active yet");
            }
            return Ok(());
        }
        let anchor = transaction
            .burn_anchor()
            .context("burn transaction is missing its parent anchor")?;
        if anchor != parent_hash {
            bail!("burn transaction anchor does not match the block parent");
        }
        Ok(())
    }

    pub(super) fn utxos_after_valid_pending(&self) -> Result<BTreeMap<OutPoint, TxOutput>> {
        let mut utxos = self.utxos.clone();
        let signing_domain = self.transaction_signing_domain();
        for pending in self.valid_pending_transactions() {
            apply_transaction(&pending, &mut utxos, &signing_domain)?;
        }
        Ok(utxos)
    }

    pub(super) fn utxos_after_spendable_pending(&self) -> Result<BTreeMap<OutPoint, TxOutput>> {
        let mut utxos = self.utxos.clone();
        let signing_domain = self.transaction_signing_domain();
        for pending in self.valid_pending_transactions() {
            if matches!(pending, Transaction::Mine { .. }) {
                continue;
            }
            if apply_spendable_pending_transaction(&pending, &mut utxos, &signing_domain).is_err() {
                continue;
            }
        }
        Ok(utxos)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::domain::{BurnBundleSection, FinalizerMode, MaskedBurn, Wallet};

    fn extend_synthetic_chain_to(ledger: &mut Ledger, target_height: u64) {
        while ledger.height() < target_height {
            let parent = ledger.tip().clone();
            let height = parent.height + 1;
            let mut block = parent;
            block.height = height;
            block.prev_hash = block.hash.clone();
            block.hash = format!("{height:064x}");
            block.timestamp_ms = height;
            block.finalizer_mode = FinalizerMode::Ticket;
            block.finalizer_rank = 0;
            block.burn_bundle_section = BurnBundleSection::default();
            block.transactions.clear();
            ledger.chain.push(block);
        }
    }

    #[test]
    fn mine_actions_expire_after_ten_blocks() {
        let wallet = Wallet::from_seed("mine-anchor-expiry-wallet");
        let mut ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 10)]), 1);
        let anchor = ledger.genesis_hash().to_string();
        let mine = Transaction::Mine {
            recipient: wallet.address().to_string(),
            anchor,
            salt: 1,
            nonce: 1,
            difficulty_bits: ledger.mine_difficulty_bits_for_anchor_height(0),
            proof_header: None,
            signature: "a".repeat(64),
        };

        extend_synthetic_chain_to(&mut ledger, MINE_MAX_ANCHOR_AGE_BLOCKS);
        assert!(ledger.validate_transaction_terms(&mine).is_ok());

        extend_synthetic_chain_to(&mut ledger, MINE_MAX_ANCHOR_AGE_BLOCKS + 1);
        assert!(
            ledger
                .validate_transaction_terms(&mine)
                .unwrap_err()
                .to_string()
                .contains("anchor is too old")
        );
    }

    #[test]
    fn burns_become_tip_bound_at_activation_and_expire_after_tip_change() {
        let wallet = Wallet::from_seed("tip-bound-burn-wallet");
        let mut ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 10)]), 1);

        extend_synthetic_chain_to(
            &mut ledger,
            super::super::TIP_BOUND_BURN_ACTIVATION_HEIGHT - 2,
        );
        let legacy_burn = ledger.build_burn(&wallet, 1, 1).unwrap();
        assert_eq!(legacy_burn.burn_anchor(), None);

        extend_synthetic_chain_to(
            &mut ledger,
            super::super::TIP_BOUND_BURN_ACTIVATION_HEIGHT - 1,
        );
        let anchored_burn = ledger.build_burn(&wallet, 1, 1).unwrap();
        assert_eq!(anchored_burn.burn_anchor(), Some(ledger.tip_hash()));
        assert!(ledger.submit_transaction(anchored_burn.clone()).unwrap());
        assert_eq!(ledger.valid_pending_transactions(), vec![anchored_burn]);

        extend_synthetic_chain_to(&mut ledger, super::super::TIP_BOUND_BURN_ACTIVATION_HEIGHT);
        assert!(ledger.valid_pending_transactions().is_empty());
    }

    #[test]
    fn burn_signature_commits_to_parent_anchor() {
        let wallet = Wallet::from_seed("tip-bound-burn-signature-wallet");
        let mut ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 10)]), 1);
        extend_synthetic_chain_to(
            &mut ledger,
            super::super::TIP_BOUND_BURN_ACTIVATION_HEIGHT - 1,
        );
        let mut burn = ledger.build_burn(&wallet, 1, 1).unwrap();
        let Transaction::Burn { anchor, .. } = &mut burn else {
            unreachable!();
        };
        *anchor = Some("f".repeat(64));

        assert!(
            burn.verify_signature(&ledger.transaction_signing_domain())
                .unwrap_err()
                .to_string()
                .contains("signature is invalid")
        );
    }

    #[test]
    fn block_selection_reserves_anchor_first_then_orders_remaining_by_fee_rate() {
        let finalizer = Wallet::from_seed("selection-finalizer");
        let high_fee_sender = Wallet::from_seed("selection-high-fee");
        let medium_fee_sender = Wallet::from_seed("selection-medium-fee");
        let recipient = Wallet::from_seed("selection-recipient");
        let mut ledger = Ledger::new(
            BTreeMap::from([
                (finalizer.address().to_string(), 1_000),
                (high_fee_sender.address().to_string(), 1_000),
                (medium_fee_sender.address().to_string(), 1_000),
            ]),
            1,
        );
        let anchor = ledger.build_burn(&finalizer, 1, 1).unwrap();
        let high_fee = ledger
            .build_transfer(&high_fee_sender, recipient.address(), 1, 20)
            .unwrap();
        let medium_fee = ledger.build_burn(&medium_fee_sender, 1, 5).unwrap();
        for transaction in [&medium_fee, &anchor, &high_fee] {
            ledger.submit_transaction(transaction.clone()).unwrap();
        }

        let selection = ledger
            .select_block_transactions_with_required_burn_owner(
                Some(finalizer.address()),
                Some(anchor.signature()),
                &BurnBundleSection::default(),
            )
            .unwrap();

        assert_eq!(selection.transactions[0].signature(), anchor.signature());
        assert!(selection.transactions[0].is_burn());
        assert!(selection.transactions[1..].windows(2).all(|pair| {
            super::super::selection::fee_rate_key(&pair[0])
                >= super::super::selection::fee_rate_key(&pair[1])
        }));
        assert!(
            selection
                .transactions
                .iter()
                .any(|transaction| transaction.signature() == high_fee.signature())
        );
    }

    #[test]
    fn public_transfers_and_burns_require_nonzero_fees() {
        let alice = Wallet::from_seed("zero-fee-alice");
        let bob = Wallet::from_seed("zero-fee-bob");
        let ledger = Ledger::new(BTreeMap::from([(alice.address().to_string(), 10)]), 1);

        assert!(
            ledger
                .build_transfer(&alice, bob.address(), 1, 0)
                .unwrap_err()
                .to_string()
                .contains("fee must be greater than zero")
        );
        assert!(
            ledger
                .build_burn(&alice, 1, 0)
                .unwrap_err()
                .to_string()
                .contains("fee must be greater than zero")
        );
    }

    #[test]
    fn required_anchor_and_attested_burns_must_fit_transaction_count_limit() {
        let finalizer = Wallet::from_seed("count-limit-finalizer");
        let committee_sender = Wallet::from_seed("count-limit-committee");
        let mut ledger = Ledger::new(
            BTreeMap::from([
                (finalizer.address().to_string(), 10),
                (committee_sender.address().to_string(), 10),
            ]),
            1,
        );
        let anchor = ledger.build_burn(&finalizer, 1, 1).unwrap();
        let attested = ledger.build_burn(&committee_sender, 1, 1).unwrap();
        ledger.submit_transaction(anchor.clone()).unwrap();
        ledger.submit_transaction(attested.clone()).unwrap();
        ledger.launch_profile.max_block_transactions = 1;
        let section = BurnBundleSection {
            signatures: Vec::new(),
            burns: vec![MaskedBurn {
                burn: attested,
                bundle_mask: 0,
            }],
        };

        assert!(
            ledger
                .select_block_transactions_with_required_burn_owner(
                    Some(finalizer.address()),
                    Some(anchor.signature()),
                    &section,
                )
                .unwrap_err()
                .to_string()
                .contains("transaction count limit")
        );
    }
}
