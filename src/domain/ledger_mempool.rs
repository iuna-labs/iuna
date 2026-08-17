use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::ledger_ops::{
    apply_transaction, ensure_blinded_transaction_fits_empty_block,
    ensure_transaction_fits_empty_block, spend_blinded_inputs, transaction_has_missing_inputs,
};
use super::transaction::{
    BlindedReveal, BlindedTransaction, Transaction, blinded_transaction_inputs_spent_by,
    transaction_inputs_spent_by, transaction_inputs_spent_by_inputs,
};
use super::{
    Ledger, MAX_ORPHAN_TRANSACTIONS, MAX_PENDING_POOL_BYTES, MAX_PENDING_TRANSACTIONS,
    TransactionSubmitOutcome, spend_inputs_with_lineage,
};

impl Ledger {
    pub fn submit_transaction(&mut self, transaction: Transaction) -> Result<bool> {
        Ok(self.submit_transaction_with_outcome(transaction)?.added())
    }

    pub(crate) fn reserve_transaction_inputs(&mut self, transaction: &Transaction) -> Result<()> {
        self.validate_new_transaction(transaction)?;
        let mut utxos = self.utxos.clone();
        let mut utxo_lineage = self.utxo_lineage.clone();
        let mut lineage_values = self.lineage_values.clone();
        let mut lineage_owners = self.lineage_owners.clone();
        spend_inputs_with_lineage(
            transaction,
            &mut utxos,
            &mut utxo_lineage,
            &mut lineage_values,
            &mut lineage_owners,
        )?;
        self.utxos = utxos;
        self.utxo_lineage = utxo_lineage;
        self.lineage_values = lineage_values;
        self.lineage_owners = lineage_owners;
        Ok(())
    }

    pub fn submit_blinded_transaction(&mut self, transaction: BlindedTransaction) -> Result<bool> {
        if self.has_blinded_transaction(&transaction.commitment) {
            return Ok(false);
        }
        self.validate_blinded_transaction(&transaction)?;
        ensure_blinded_transaction_fits_empty_block(
            &transaction,
            self.launch_profile.max_block_bytes,
        )?;
        if blinded_transaction_inputs_spent_by(&transaction, &self.pending_blinded)
            || transaction_inputs_spent_by_inputs(&transaction.inputs, &self.pending)
            || transaction_inputs_spent_by_inputs(&transaction.inputs, &self.orphans)
        {
            bail!("blinded transaction conflicts with pending inputs");
        }
        let mut utxos = self.utxos_after_valid_pending_and_blinded()?;
        spend_blinded_inputs(&transaction, &mut utxos)?;
        if self.pending_blinded.len() >= MAX_PENDING_TRANSACTIONS {
            bail!("blinded mempool is full");
        }
        let candidate_bytes = ensure_pending_pool_bytes(
            "blinded mempool",
            self.pending_blinded_bytes,
            &transaction,
            MAX_PENDING_POOL_BYTES,
        )?;
        self.pending_blinded.push(transaction);
        self.pending_blinded_bytes = self.pending_blinded_bytes.saturating_add(candidate_bytes);
        Ok(true)
    }

    pub fn submit_blinded_reveal(&mut self, reveal: BlindedReveal) -> Result<bool> {
        if self.has_blinded_reveal(&reveal.commitment) {
            return Ok(false);
        }
        self.validate_blinded_reveal_terms(&reveal)?;
        self.pending_reveal_transaction(&reveal)?;
        if self.pending_reveals.len() >= MAX_PENDING_TRANSACTIONS
            && !self.drop_one_invalid_pending_blinded_reveal()?
        {
            bail!("blinded reveal pool is full");
        }
        let candidate_bytes = ensure_pending_pool_bytes(
            "blinded reveal pool",
            self.pending_reveal_bytes,
            &reveal,
            MAX_PENDING_POOL_BYTES,
        )?;
        self.pending_reveals.push(reveal);
        self.pending_reveal_bytes = self.pending_reveal_bytes.saturating_add(candidate_bytes);
        Ok(true)
    }

    fn drop_one_invalid_pending_blinded_reveal(&mut self) -> Result<bool> {
        let Some(index) = self
            .pending_reveals
            .iter()
            .position(|reveal| self.pending_reveal_transaction(reveal).is_err())
        else {
            return Ok(false);
        };
        let removed = self.pending_reveals.remove(index);
        self.pending_reveal_bytes = self
            .pending_reveal_bytes
            .saturating_sub(serialized_len(&removed)?);
        Ok(true)
    }

    pub fn submit_transaction_with_outcome(
        &mut self,
        transaction: Transaction,
    ) -> Result<TransactionSubmitOutcome> {
        if self.has_transaction(transaction.signature()) {
            return Ok(TransactionSubmitOutcome::AlreadyKnown);
        }

        transaction.verify_signature()?;
        self.validate_transaction_terms(&transaction)?;
        ensure_transaction_fits_empty_block(&transaction, self.launch_profile.max_block_bytes)?;
        self.validate_mine_anchor_available(&transaction)?;

        if transaction_inputs_spent_by(&transaction, &self.pending) {
            return Ok(TransactionSubmitOutcome::ConflictsWithPending);
        }
        if transaction_inputs_spent_by(&transaction, &self.orphans) {
            return Ok(TransactionSubmitOutcome::ConflictsWithPending);
        }

        if self.pending.len() >= MAX_PENDING_TRANSACTIONS {
            bail!("mempool is full");
        }

        let mut utxos = self.utxos_after_valid_pending_and_blinded()?;
        if transaction_has_missing_inputs(&transaction, &utxos) {
            if self.orphans.len() >= MAX_ORPHAN_TRANSACTIONS {
                bail!("orphan transaction pool is full");
            }
            let candidate_bytes = ensure_pending_pool_bytes(
                "orphan transaction pool",
                self.orphan_bytes,
                &transaction,
                MAX_PENDING_POOL_BYTES,
            )?;
            self.orphans.push(transaction);
            self.orphan_bytes = self.orphan_bytes.saturating_add(candidate_bytes);
            return Ok(TransactionSubmitOutcome::Added);
        }
        apply_transaction(&transaction, &mut utxos)?;
        let candidate_bytes = ensure_pending_pool_bytes(
            "mempool",
            self.pending_bytes,
            &transaction,
            MAX_PENDING_POOL_BYTES,
        )?;
        self.pending.push(transaction);
        self.pending_bytes = self.pending_bytes.saturating_add(candidate_bytes);
        self.promote_orphan_transactions()?;
        Ok(TransactionSubmitOutcome::Added)
    }

    pub(super) fn refresh_pending_pool_byte_counters(&mut self) -> Result<()> {
        self.pending_bytes = serialized_pool_len(&self.pending)?;
        self.orphan_bytes = serialized_pool_len(&self.orphans)?;
        self.pending_blinded_bytes = serialized_pool_len(&self.pending_blinded)?;
        self.pending_reveal_bytes = serialized_pool_len(&self.pending_reveals)?;
        Ok(())
    }
}

fn ensure_pending_pool_bytes<T: Serialize>(
    label: &str,
    existing_bytes: usize,
    candidate: &T,
    max_bytes: usize,
) -> Result<usize> {
    let candidate_bytes = serialized_len(candidate)?;
    let total_bytes = existing_bytes
        .checked_add(candidate_bytes)
        .context("pending pool byte size overflow")?;
    if total_bytes > max_bytes {
        bail!("{label} byte limit exceeded");
    }
    Ok(candidate_bytes)
}

fn serialized_pool_len<T: Serialize>(items: &[T]) -> Result<usize> {
    items.iter().try_fold(0usize, |total, item| {
        total
            .checked_add(serialized_len(item)?)
            .context("pending pool byte size overflow")
    })
}

pub(super) fn pending_pool_item_bytes<T: Serialize>(item: &T) -> Result<usize> {
    serialized_len(item)
}

fn serialized_len<T: Serialize>(item: &T) -> Result<usize> {
    serde_json::to_vec(item)
        .context("failed to serialize pending item for size check")
        .map(|bytes| bytes.len())
}
