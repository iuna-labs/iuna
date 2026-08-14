use anyhow::{Result, bail};

use super::ledger_ops::{
    apply_transaction, ensure_blinded_transaction_fits_empty_block,
    ensure_transaction_fits_empty_block, spend_blinded_inputs, spend_inputs,
    transaction_has_missing_inputs,
};
use super::transaction::{
    BlindedReveal, BlindedTransaction, Transaction, blinded_transaction_inputs_spent_by,
    transaction_inputs_spent_by, transaction_inputs_spent_by_inputs,
};
use super::{Ledger, MAX_ORPHAN_TRANSACTIONS, MAX_PENDING_TRANSACTIONS, TransactionSubmitOutcome};

impl Ledger {
    pub fn submit_transaction(&mut self, transaction: Transaction) -> Result<bool> {
        Ok(self.submit_transaction_with_outcome(transaction)?.added())
    }

    pub(crate) fn reserve_transaction_inputs(&mut self, transaction: &Transaction) -> Result<()> {
        self.validate_new_transaction(transaction)?;
        let mut utxos = self.utxos.clone();
        spend_inputs(transaction, &mut utxos)?;
        self.utxos = utxos;
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
        self.pending_blinded.push(transaction);
        Ok(true)
    }

    pub fn submit_blinded_reveal(&mut self, reveal: BlindedReveal) -> Result<bool> {
        if self.has_blinded_reveal(&reveal.commitment) {
            return Ok(false);
        }
        self.validate_blinded_reveal_terms(&reveal)?;
        if self.pending_reveals.len() >= MAX_PENDING_TRANSACTIONS
            && (!self.has_active_blinded_transaction(&reveal.commitment)
                || !self.drop_one_invalid_pending_blinded_reveal())
        {
            bail!("blinded reveal pool is full");
        }
        self.pending_reveals.push(reveal);
        Ok(true)
    }

    fn drop_one_invalid_pending_blinded_reveal(&mut self) -> bool {
        let Some(index) = self
            .pending_reveals
            .iter()
            .position(|reveal| self.pending_reveal_transaction(reveal).is_err())
        else {
            return false;
        };
        self.pending_reveals.remove(index);
        true
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
            self.orphans.push(transaction);
            return Ok(TransactionSubmitOutcome::Added);
        }
        apply_transaction(&transaction, &mut utxos)?;
        self.pending.push(transaction);
        self.promote_orphan_transactions()?;
        Ok(TransactionSubmitOutcome::Added)
    }
}
