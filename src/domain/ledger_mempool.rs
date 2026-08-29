use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::ledger_ops::{
    apply_transaction, compact_block_context, ensure_transaction_fits_empty_block,
    transaction_has_missing_inputs,
};
use super::transaction::{Transaction, transaction_inputs_spent_by};
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

    pub fn submit_transaction_with_outcome(
        &mut self,
        transaction: Transaction,
    ) -> Result<TransactionSubmitOutcome> {
        if self.has_transaction(transaction.signature()) {
            return Ok(TransactionSubmitOutcome::AlreadyKnown);
        }

        let signing_domain = self.transaction_signing_domain();
        transaction.verify_signature(&signing_domain)?;
        self.validate_transaction_terms(&transaction)?;
        ensure_transaction_fits_empty_block(
            compact_block_context(self),
            &transaction,
            self.launch_profile.max_block_bytes,
        )?;
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

        let mut utxos = self.utxos_after_valid_pending()?;
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
        apply_transaction(&transaction, &mut utxos, &signing_domain)?;
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::domain::transaction::{UnsignedTxInput, UnsignedUtxoTransaction};
    use crate::domain::{OutPoint, TxOutput, Wallet};

    fn dummy_mine(signature_digit: char) -> Transaction {
        Transaction::Mine {
            recipient: "recipient".to_string(),
            anchor: "a".repeat(64),
            salt: 1,
            nonce: 1,
            difficulty_bits: 10,
            proof_header: None,
            signature: signature_digit.to_string().repeat(64),
        }
    }

    #[test]
    fn pending_pool_byte_limit_accepts_the_boundary_and_rejects_one_byte_over() {
        let item = "bounded-item";
        let item_bytes = serialized_len(&item).unwrap();

        assert_eq!(
            ensure_pending_pool_bytes(
                "test pool",
                MAX_PENDING_POOL_BYTES - item_bytes,
                &item,
                MAX_PENDING_POOL_BYTES,
            )
            .unwrap(),
            item_bytes
        );
        assert!(
            ensure_pending_pool_bytes(
                "test pool",
                MAX_PENDING_POOL_BYTES - item_bytes + 1,
                &item,
                MAX_PENDING_POOL_BYTES,
            )
            .unwrap_err()
            .to_string()
            .contains("byte limit exceeded")
        );
    }

    #[test]
    fn mempool_rejects_transaction_after_ten_thousand_items() {
        let alice = Wallet::from_seed("pending-limit-alice");
        let bob = Wallet::from_seed("pending-limit-bob");
        let mut ledger = Ledger::new(
            BTreeMap::from([
                (alice.address().to_string(), 10),
                (bob.address().to_string(), 10),
            ]),
            1,
        );
        let candidate = ledger.build_transfer(&alice, bob.address(), 1, 1).unwrap();
        ledger.pending = vec![dummy_mine('f'); MAX_PENDING_TRANSACTIONS];

        assert_eq!(ledger.pending.len(), 10_000);
        assert!(
            ledger
                .submit_transaction(candidate)
                .unwrap_err()
                .to_string()
                .contains("mempool is full")
        );
    }

    #[test]
    fn orphan_pool_rejects_transaction_after_1024_items() {
        let wallet = Wallet::from_seed("orphan-limit-wallet");
        let mut ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 10)]), 1);
        let orphan = UnsignedUtxoTransaction::Transfer {
            inputs: vec![UnsignedTxInput {
                outpoint: OutPoint {
                    txid: "b".repeat(64),
                    index: 0,
                },
                owner: wallet.address().to_string(),
            }],
            outputs: vec![TxOutput {
                address: wallet.address().to_string(),
                amount: 1,
            }],
            fee: 1,
        }
        .sign(&wallet, &ledger.transaction_signing_domain())
        .unwrap();
        ledger.orphans = vec![dummy_mine('e'); MAX_ORPHAN_TRANSACTIONS];

        assert_eq!(ledger.orphans.len(), 1_024);
        assert!(
            ledger
                .submit_transaction(orphan)
                .unwrap_err()
                .to_string()
                .contains("orphan transaction pool is full")
        );
    }
}
