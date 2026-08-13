use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};

use super::blinded::{
    ActiveBlindedTransaction, blinded_envelope_fee_for_transaction, blinded_locked_output_total,
    blinded_reveal_inputs_match, blinded_transaction_commitment, decrypt_blinded_transaction,
    verify_blinded_input_signatures,
};
use super::ledger_ops::{
    apply_spendable_pending_transaction, apply_transaction, best_selectable_blinded_index,
    best_selectable_burn_from_index, best_selectable_transaction_index,
    ensure_blinded_transaction_fits_empty_block, ensure_outputs_do_not_overflow,
    ensure_single_input_owner, ensure_transaction_fits_empty_block,
    estimated_block_selection_size_bytes, spend_blinded_inputs, spend_spendable_blinded_inputs,
    transaction_has_missing_inputs, validate_transaction_inputs, validate_transaction_outputs,
};
use super::mine_policy::{
    MINE_MAX_ANCHOR_AGE_BLOCKS, mine_anchor, mine_anchor_count_before_height,
};
use super::selection::{
    BlockSelection, SelectableItem, TransactionKind, best_selectable_item, blinded_fee_rate_key,
    fee_rate_key,
};
use super::transaction::{
    UnsignedTxInput, transaction_inputs_available, transaction_inputs_spent_by,
};
use super::validation::{
    validate_address, validate_hash, validate_signature, validate_stratum_header,
};
use super::{
    Amount, BLINDED_KEY_BYTES, BLINDED_NONCE_BYTES, BlindedReveal, BlindedTransaction, Ledger,
    MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS, MAX_PENDING_TRANSACTIONS,
    MINE_ACTIONS_PER_ANCHOR_LIMIT, OutPoint, Transaction, TxOutput, decode_hex, decode_hex_array,
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

    pub(super) fn select_block_transactions(
        &self,
        required_burn_signature: Option<&str>,
    ) -> Result<BlockSelection> {
        self.select_block_transactions_with_required_burn_owner(None, required_burn_signature)
    }

    pub(super) fn select_recovery_block_transactions(
        &self,
        miner: &str,
        required_burn_signature: Option<&str>,
    ) -> Result<BlockSelection> {
        self.select_block_transactions_with_required_burn_owner(
            Some(miner),
            required_burn_signature,
        )
    }

    pub(super) fn select_block_transactions_with_required_burn_owner(
        &self,
        required_burn_owner: Option<&str>,
        required_burn_signature: Option<&str>,
    ) -> Result<BlockSelection> {
        let mut utxos = self.utxos.clone();
        let mut remaining = self.valid_pending_transactions();
        let mut remaining_blinded = self.valid_pending_blinded_transactions();
        let mut selected = Vec::new();
        let mut selected_blinded = Vec::new();

        if let Some(signature) = required_burn_signature {
            let index = remaining
                .iter()
                .position(|transaction| transaction.signature() == signature)
                .with_context(|| format!("required burn {signature} is not pending"))?;
            let tx = remaining.remove(index);
            if !tx.is_burn() {
                bail!("required block anchor must be a burn transaction");
            }
            if let Some(owner) = required_burn_owner {
                if tx.sender() != owner {
                    bail!("required block anchor burn must be from the recovery finalizer");
                }
            }
            let candidate = BlockSelection {
                transactions: vec![tx.clone()],
                blinded_transactions: selected_blinded.clone(),
            };
            if estimated_block_selection_size_bytes(&candidate, required_burn_owner.is_some())?
                > self.launch_profile.max_block_bytes
            {
                bail!("required block anchor burn does not fit in the block");
            }
            apply_transaction(&tx, &mut utxos)
                .context("required block anchor burn is not spendable")?;
            selected.push(tx);
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
                let mut candidate = BlockSelection {
                    transactions: selected.clone(),
                    blinded_transactions: selected_blinded.clone(),
                };
                candidate.transactions.push(tx.clone());
                if estimated_block_selection_size_bytes(&candidate, required_burn_owner.is_some())?
                    <= self.launch_profile.max_block_bytes
                {
                    apply_transaction(&tx, &mut utxos)?;
                    selected.push(tx);
                }
            }
        }

        while selected.len() < self.launch_profile.max_block_transactions {
            let selected_count = selected.len() + selected_blinded.len();
            if selected_count >= self.launch_profile.max_block_transactions {
                break;
            }

            let best_plain = best_selectable_transaction_index(&remaining, &utxos, None)
                .map(|index| SelectableItem::Plain(index, fee_rate_key(&remaining[index])));
            let best_blinded =
                best_selectable_blinded_index(&remaining_blinded, &utxos).map(|index| {
                    SelectableItem::Blinded(index, blinded_fee_rate_key(&remaining_blinded[index]))
                });
            let Some(item) = best_selectable_item(best_plain, best_blinded) else {
                break;
            };

            match item {
                SelectableItem::Plain(index, _) => {
                    let tx = remaining.remove(index);
                    let mut candidate = BlockSelection {
                        transactions: selected.clone(),
                        blinded_transactions: selected_blinded.clone(),
                    };
                    candidate.transactions.push(tx.clone());
                    if estimated_block_selection_size_bytes(
                        &candidate,
                        required_burn_owner.is_some(),
                    )? <= self.launch_profile.max_block_bytes
                    {
                        apply_transaction(&tx, &mut utxos)?;
                        selected.push(tx);
                    }
                }
                SelectableItem::Blinded(index, _) => {
                    let transaction = remaining_blinded.remove(index);
                    let mut candidate = BlockSelection {
                        transactions: selected.clone(),
                        blinded_transactions: selected_blinded.clone(),
                    };
                    candidate.blinded_transactions.push(transaction.clone());
                    if estimated_block_selection_size_bytes(
                        &candidate,
                        required_burn_owner.is_some(),
                    )? <= self.launch_profile.max_block_bytes
                    {
                        spend_blinded_inputs(&transaction, &mut utxos)?;
                        selected_blinded.push(transaction);
                    }
                }
            }
        }
        Ok(BlockSelection {
            transactions: selected,
            blinded_transactions: selected_blinded,
        })
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
            let mut promoted_index = None;
            let mut utxos = self.utxos_after_valid_pending_and_blinded()?;
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
                    promoted_index = Some(index);
                    break;
                }
            }

            let Some(index) = promoted_index else {
                return Ok(());
            };
            self.pending.push(self.orphans.remove(index));
        }
    }

    pub(super) fn validate_transaction_terms(&self, transaction: &Transaction) -> Result<()> {
        match transaction {
            Transaction::Transfer {
                inputs,
                outputs,
                signature,
                ..
            } => {
                validate_transaction_inputs(inputs)?;
                validate_transaction_outputs(outputs)?;
                validate_signature(signature, "transaction signature")?;
            }
            Transaction::Burn {
                inputs,
                change,
                signature,
                ..
            } => {
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

    pub(super) fn validate_blinded_transaction(
        &self,
        transaction: &BlindedTransaction,
    ) -> Result<()> {
        validate_hash(&transaction.commitment, "blinded transaction commitment")?;
        validate_hash(
            &transaction.payload_hash,
            "blinded transaction payload hash",
        )?;
        decode_hex_array::<BLINDED_NONCE_BYTES>(&transaction.nonce)
            .context("invalid blinded transaction nonce")?;
        let ciphertext = decode_hex(&transaction.ciphertext)
            .context("invalid blinded transaction ciphertext")?;
        if ciphertext.is_empty() {
            bail!("blinded transaction ciphertext is empty");
        }
        if ciphertext.len() != transaction.encrypted_size as usize {
            bail!("blinded transaction encrypted size is invalid");
        }
        if transaction.expires_at_height <= self.height() {
            bail!("blinded transaction is expired");
        }
        if transaction.expires_at_height
            > self
                .height()
                .saturating_add(MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS)
        {
            bail!("blinded transaction expiry is too far in the future");
        }
        validate_transaction_inputs(&transaction.inputs)?;
        if transaction.inputs.is_empty() && transaction.fee > 0 {
            bail!("blinded transaction with a fee must lock visible inputs");
        }
        if !transaction.inputs.is_empty() {
            verify_blinded_input_signatures(transaction)?;
        }
        let expected = blinded_transaction_commitment(transaction)?;
        if transaction.commitment != expected {
            bail!("blinded transaction commitment is invalid");
        }
        ensure_blinded_transaction_fits_empty_block(
            transaction,
            self.launch_profile.max_block_bytes,
        )?;
        Ok(())
    }

    pub(super) fn validate_blinded_reveal_terms(&self, reveal: &BlindedReveal) -> Result<()> {
        validate_hash(&reveal.commitment, "blinded reveal commitment")?;
        decode_hex_array::<BLINDED_KEY_BYTES>(&reveal.key).context("invalid blinded reveal key")?;
        Ok(())
    }

    pub(super) fn valid_pending_blinded_transactions(&self) -> Vec<BlindedTransaction> {
        let next_height = self.height().saturating_add(1);
        self.pending_blinded
            .iter()
            .filter(|transaction| {
                transaction.expires_at_height > next_height
                    && self.validate_blinded_transaction(transaction).is_ok()
            })
            .cloned()
            .collect()
    }

    pub(super) fn valid_pending_blinded_reveals(&self) -> Vec<BlindedReveal> {
        self.pending_reveals
            .iter()
            .filter(|reveal| self.pending_reveal_transaction(reveal).is_ok())
            .cloned()
            .collect()
    }

    pub(super) fn reveal_fee_order_key(&self, reveal: &BlindedReveal) -> (u128, Amount) {
        let Some(active) = self.active_blinded.get(&reveal.commitment) else {
            return (0, 0);
        };
        let size = active.transaction.fee_rate_size_bytes();
        let rate = if size == 0 {
            0
        } else {
            u128::from(active.transaction.fee) * 1_000_000 / size as u128
        };
        (rate, active.transaction.fee)
    }

    pub(super) fn pending_reveal_transaction(&self, reveal: &BlindedReveal) -> Result<Transaction> {
        self.validate_blinded_reveal_terms(reveal)?;
        let active = self
            .active_blinded
            .get(&reveal.commitment)
            .context("blinded reveal does not reference an active blinded transaction")?;
        self.decrypt_active_blinded(active, reveal)
    }

    pub(super) fn decrypt_active_blinded(
        &self,
        active: &ActiveBlindedTransaction,
        reveal: &BlindedReveal,
    ) -> Result<Transaction> {
        if self.height() >= active.transaction.expires_at_height {
            bail!("blinded transaction reveal is expired");
        }
        let transaction = decrypt_blinded_transaction(&active.transaction, reveal)?;
        if matches!(transaction, Transaction::Mine { .. }) {
            bail!("mine actions are public and cannot be blinded");
        }
        if blinded_envelope_fee_for_transaction(&transaction) != active.transaction.fee {
            bail!("blinded transaction reveal fee does not match envelope");
        }
        if !blinded_reveal_inputs_match(active, &transaction) {
            bail!("blinded transaction reveal inputs do not match envelope");
        }
        self.validate_transaction_terms(&transaction)?;
        Ok(transaction)
    }

    pub(super) fn apply_revealed_blinded_transaction(
        &self,
        active: &ActiveBlindedTransaction,
        transaction: &Transaction,
        utxos: &mut BTreeMap<OutPoint, TxOutput>,
    ) -> Result<()> {
        if matches!(transaction, Transaction::Mine { .. }) {
            bail!("mine actions are public and cannot be blinded");
        }
        transaction.verify_signature()?;
        ensure_single_input_owner(transaction)?;
        let input_total = blinded_locked_output_total(active)?;
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
            bail!("blinded transaction inputs do not balance outputs, burn, and fee");
        }
        ensure_outputs_do_not_overflow(utxos, &outputs)?;
        for (index, output) in outputs.iter().enumerate() {
            utxos.insert(
                OutPoint {
                    txid: transaction.signature().to_string(),
                    index: index as u32,
                },
                output.clone(),
            );
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

    pub(super) fn utxos_after_valid_pending_and_blinded(
        &self,
    ) -> Result<BTreeMap<OutPoint, TxOutput>> {
        let mut utxos = self.utxos_after_valid_pending()?;
        for pending in self.valid_pending_blinded_transactions() {
            spend_blinded_inputs(&pending, &mut utxos)?;
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
        for pending in self.valid_pending_blinded_transactions() {
            if spend_spendable_blinded_inputs(&pending, &mut utxos).is_err() {
                continue;
            }
        }
        Ok(utxos)
    }
}
