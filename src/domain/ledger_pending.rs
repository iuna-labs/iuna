use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};

use super::ledger_mempool::pending_pool_item_bytes;
use super::ledger_ops::{
    apply_spendable_pending_transaction, apply_transaction, best_selectable_burn_from_index,
    best_selectable_transaction_index, ensure_transaction_fits_empty_block,
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

impl Ledger {
    pub(super) fn valid_pending_transactions(&self) -> Vec<Transaction> {
        let mut utxos = self.utxos.clone();
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
                    && apply_transaction(tx, &mut utxos).is_ok()
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
        let mut utxos = self.utxos.clone();
        let mut remaining = self.valid_pending_transactions();
        let mut selected = Vec::new();

        let required_burn_signatures = burn_bundle_section
            .required_burns()
            .into_iter()
            .map(|burn| burn.signature().to_string())
            .collect::<BTreeSet<_>>();
        let mut selected_required_burn_signatures = BTreeSet::new();
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
            let signature = tx.signature().to_string();
            let mut candidate = BlockSelection {
                transactions: selected.clone(),
            };
            candidate.transactions.push(tx.clone());
            if estimated_block_selection_size_bytes(
                &candidate,
                required_burn_owner.is_some(),
                burn_bundle_section,
            )? > self.launch_profile.max_block_bytes
            {
                bail!("attested burns do not fit in the block");
            }
            apply_transaction(&tx, &mut utxos).context("attested burn is not spendable")?;
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

        if let Some(signature) = required_burn_signature {
            if !selected
                .iter()
                .any(|transaction| transaction.signature() == signature)
            {
                let index = remaining
                    .iter()
                    .position(|transaction| transaction.signature() == signature)
                    .with_context(|| format!("required burn {signature} is not pending"))?;
                let tx = remaining.remove(index);
                self.select_required_anchor_burn(
                    tx,
                    required_burn_owner,
                    burn_bundle_section,
                    &mut utxos,
                    &mut selected,
                )?;
            }
        }

        let needs_first_burn = !selected.iter().any(Transaction::is_burn);
        let needs_owner_burn = required_burn_owner.is_some_and(|owner| {
            !selected
                .iter()
                .any(|transaction| transaction.is_burn() && transaction.sender() == owner)
        });
        if needs_first_burn || needs_owner_burn {
            let first_burn_index = if let Some(owner) = required_burn_owner {
                best_selectable_burn_from_index(&remaining, &utxos, owner)
            } else {
                best_selectable_transaction_index(&remaining, &utxos, Some(TransactionKind::Burn))
            };
            if let Some(index) = first_burn_index {
                let tx = remaining.remove(index);
                self.select_required_anchor_burn(
                    tx,
                    required_burn_owner,
                    burn_bundle_section,
                    &mut utxos,
                    &mut selected,
                )?;
            }
        }

        while selected.len() < self.launch_profile.max_block_transactions {
            let Some(index) = best_selectable_transaction_index(&remaining, &utxos, None) else {
                break;
            };
            let tx = remaining.remove(index);
            let mut candidate = BlockSelection {
                transactions: selected.clone(),
            };
            candidate.transactions.push(tx.clone());
            if estimated_block_selection_size_bytes(
                &candidate,
                required_burn_owner.is_some(),
                burn_bundle_section,
            )? <= self.launch_profile.max_block_bytes
            {
                apply_transaction(&tx, &mut utxos)?;
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
        burn_bundle_section: &BurnBundleSection,
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
        let mut candidate = BlockSelection {
            transactions: selected.clone(),
        };
        candidate.transactions.push(tx.clone());
        if estimated_block_selection_size_bytes(
            &candidate,
            required_burn_owner.is_some(),
            burn_bundle_section,
        )? > self.launch_profile.max_block_bytes
        {
            bail!("required block anchor burn does not fit in the block");
        }
        apply_transaction(&tx, utxos).context("required block anchor burn is not spendable")?;
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
        ensure_transaction_fits_empty_block(transaction, self.launch_profile.max_block_bytes)?;
        self.validate_mine_anchor_available(transaction)?;
        let mut utxos = self.utxos_after_spendable_pending()?;
        apply_transaction(transaction, &mut utxos)
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
                bail!("mine transaction anchor limit reached");
            }
        }
        Ok(())
    }

    pub(super) fn promote_orphan_transactions(&mut self) -> Result<()> {
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
                    && apply_transaction(transaction, &mut utxos).is_ok()
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
                signature,
                ..
            } => {
                if *fee == 0 {
                    bail!("burn transaction fee must be greater than zero");
                }
                validate_transaction_inputs(inputs)?;
                validate_transaction_outputs(change)?;
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

    pub(super) fn utxos_after_valid_pending(&self) -> Result<BTreeMap<OutPoint, TxOutput>> {
        let mut utxos = self.utxos.clone();
        for pending in self.valid_pending_transactions() {
            apply_transaction(&pending, &mut utxos)?;
        }
        Ok(utxos)
    }

    pub(super) fn utxos_after_spendable_pending(&self) -> Result<BTreeMap<OutPoint, TxOutput>> {
        let mut utxos = self.utxos.clone();
        for pending in self.valid_pending_transactions() {
            if matches!(pending, Transaction::Mine { .. }) {
                continue;
            }
            if apply_spendable_pending_transaction(&pending, &mut utxos).is_err() {
                continue;
            }
        }
        Ok(utxos)
    }
}
