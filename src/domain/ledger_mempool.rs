use std::{cmp::Ordering, collections::BTreeSet};

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

    /// Inserts a locally required block transaction ahead of the current
    /// mempool, then rebuilds the pool around it. This is only used on a cloned
    /// ledger while assembling a block; the node's public mempool is unchanged.
    pub(crate) fn prioritize_transaction_for_block_building(
        &mut self,
        transaction: Transaction,
    ) -> Result<bool> {
        let mut displaced = std::mem::take(&mut self.pending);
        displaced.append(&mut self.orphans);
        self.pending_bytes = 0;
        self.orphan_bytes = 0;

        let added = self.submit_transaction(transaction)?;
        if !added {
            return Ok(false);
        }
        for candidate in displaced {
            let _ = self.submit_transaction(candidate);
        }
        Ok(true)
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

        self.validate_transaction_terms(&transaction)?;
        self.validate_transaction_anchor_for_pending(&transaction)?;
        let signing_domain = self.transaction_signing_domain_for_pending(&transaction);
        transaction.verify_signature(&signing_domain)?;
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
        let candidate_bytes = pending_pool_item_bytes(&transaction)?;
        let replacement_backup = self.pending_room_required(candidate_bytes).then(|| {
            (
                self.pending.clone(),
                self.orphans.clone(),
                self.pending_bytes,
                self.orphan_bytes,
            )
        });
        if let Err(error) = self.make_pending_room(&transaction, candidate_bytes) {
            self.restore_pending_after_failed_replacement(replacement_backup);
            return Err(error);
        }

        // An eviction may remove state the candidate depended on. Components
        // containing candidate ancestors are protected, but revalidation keeps
        // this admission step fail-closed if the graph is ever malformed.
        let mut utxos = match self.utxos_after_valid_pending() {
            Ok(utxos) => utxos,
            Err(error) => {
                self.restore_pending_after_failed_replacement(replacement_backup);
                return Err(error);
            }
        };
        if transaction_has_missing_inputs(&transaction, &utxos) {
            self.restore_pending_after_failed_replacement(replacement_backup);
            bail!("mempool replacement removed a candidate dependency");
        }
        if let Err(error) = apply_transaction(&transaction, &mut utxos, &signing_domain) {
            self.restore_pending_after_failed_replacement(replacement_backup);
            return Err(error);
        }
        self.pending.push(transaction);
        self.pending_bytes = self.pending_bytes.saturating_add(candidate_bytes);
        if let Err(error) = self.promote_orphan_transactions() {
            if let Some((pending, orphans, pending_bytes, orphan_bytes)) = replacement_backup {
                self.pending = pending;
                self.orphans = orphans;
                self.pending_bytes = pending_bytes;
                self.orphan_bytes = orphan_bytes;
            }
            return Err(error);
        }
        Ok(TransactionSubmitOutcome::Added)
    }

    fn pending_room_required(&self, candidate_bytes: usize) -> bool {
        self.pending.len() >= MAX_PENDING_TRANSACTIONS
            || self
                .pending_bytes
                .checked_add(candidate_bytes)
                .is_none_or(|bytes| bytes > MAX_PENDING_POOL_BYTES)
    }

    fn restore_pending_after_failed_replacement(
        &mut self,
        backup: Option<(Vec<Transaction>, Vec<Transaction>, usize, usize)>,
    ) {
        if let Some((pending, orphans, pending_bytes, orphan_bytes)) = backup {
            self.pending = pending;
            self.orphans = orphans;
            self.pending_bytes = pending_bytes;
            self.orphan_bytes = orphan_bytes;
        }
    }

    fn make_pending_room(&mut self, candidate: &Transaction, candidate_bytes: usize) -> Result<()> {
        if !self.pending_room_required(candidate_bytes) {
            return Ok(());
        }

        let evicted =
            pending_eviction_plan(&self.pending, &self.orphans, candidate, candidate_bytes)?;
        self.pending
            .retain(|transaction| !evicted.contains(transaction.signature()));
        self.orphans
            .retain(|transaction| !evicted.contains(transaction.signature()));
        self.refresh_pending_pool_byte_counters()
    }

    pub(super) fn refresh_pending_pool_byte_counters(&mut self) -> Result<()> {
        self.pending_bytes = serialized_pool_len(&self.pending)?;
        self.orphan_bytes = serialized_pool_len(&self.orphans)?;
        Ok(())
    }
}

#[derive(Debug)]
struct EvictionPackage {
    signatures: BTreeSet<String>,
    fee: u128,
    economic_bytes: u128,
    pending_bytes: usize,
    pending_count: usize,
    tie_break: String,
}

fn pending_eviction_plan(
    pending: &[Transaction],
    orphans: &[Transaction],
    candidate: &Transaction,
    candidate_bytes: usize,
) -> Result<BTreeSet<String>> {
    if candidate_bytes > MAX_PENDING_POOL_BYTES {
        bail!("mempool byte limit exceeded");
    }

    let by_signature = pending
        .iter()
        .enumerate()
        .map(|(index, transaction)| (transaction.signature().to_string(), index))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut edges = vec![Vec::new(); pending.len()];
    for (index, transaction) in pending.iter().enumerate() {
        for input in transaction.inputs() {
            if let Some(parent) = by_signature.get(&input.outpoint.txid).copied() {
                edges[index].push(parent);
                edges[parent].push(index);
            }
        }
    }

    let protected = candidate
        .inputs()
        .iter()
        .filter_map(|input| by_signature.get(&input.outpoint.txid).copied())
        .collect::<BTreeSet<_>>();
    let mut visited = vec![false; pending.len()];
    let mut packages = Vec::new();
    for start in 0..pending.len() {
        if visited[start] {
            continue;
        }
        let mut stack = vec![start];
        let mut indices = Vec::new();
        let mut protects_candidate = false;
        visited[start] = true;
        while let Some(index) = stack.pop() {
            indices.push(index);
            protects_candidate |= protected.contains(&index);
            for adjacent in &edges[index] {
                if !visited[*adjacent] {
                    visited[*adjacent] = true;
                    stack.push(*adjacent);
                }
            }
        }
        if protects_candidate {
            continue;
        }

        let signatures = indices
            .iter()
            .map(|index| pending[*index].signature().to_string())
            .collect::<BTreeSet<_>>();
        let fee = indices
            .iter()
            .map(|index| u128::from(pending[*index].fee()))
            .sum();
        let economic_bytes = indices
            .iter()
            .map(|index| pending[*index].economic_size_bytes() as u128)
            .sum();
        let pending_bytes = indices.iter().try_fold(0usize, |total, index| {
            total
                .checked_add(pending_pool_item_bytes(&pending[*index])?)
                .context("pending eviction package byte size overflow")
        })?;
        let tie_break = signatures.iter().next().cloned().unwrap_or_default();
        packages.push(EvictionPackage {
            signatures,
            fee,
            economic_bytes,
            pending_bytes,
            pending_count: indices.len(),
            tie_break,
        });
    }

    packages.sort_by(compare_package_fee_rate);
    let candidate_fee = u128::from(candidate.fee());
    let candidate_economic_bytes = candidate.economic_size_bytes() as u128;
    let mut remaining_count = pending.len();
    let mut remaining_bytes = serialized_pool_len(pending)?;
    let mut evicted = BTreeSet::new();
    for package in packages {
        let count_fits = remaining_count < MAX_PENDING_TRANSACTIONS;
        let bytes_fit = remaining_bytes
            .checked_add(candidate_bytes)
            .is_some_and(|bytes| bytes <= MAX_PENDING_POOL_BYTES);
        if count_fits && bytes_fit {
            extend_with_orphan_descendants(&mut evicted, orphans);
            return Ok(evicted);
        }
        if candidate_fee.saturating_mul(package.economic_bytes)
            <= package.fee.saturating_mul(candidate_economic_bytes)
        {
            break;
        }
        remaining_count = remaining_count.saturating_sub(package.pending_count);
        remaining_bytes = remaining_bytes.saturating_sub(package.pending_bytes);
        evicted.extend(package.signatures);
    }

    let count_fits = remaining_count < MAX_PENDING_TRANSACTIONS;
    let bytes_fit = remaining_bytes
        .checked_add(candidate_bytes)
        .is_some_and(|bytes| bytes <= MAX_PENDING_POOL_BYTES);
    if count_fits && bytes_fit {
        extend_with_orphan_descendants(&mut evicted, orphans);
        Ok(evicted)
    } else {
        bail!("mempool is full and candidate fee rate does not exceed an evictable package")
    }
}

fn extend_with_orphan_descendants(evicted: &mut BTreeSet<String>, orphans: &[Transaction]) {
    loop {
        let mut changed = false;
        for orphan in orphans {
            if !evicted.contains(orphan.signature())
                && orphan
                    .inputs()
                    .iter()
                    .any(|input| evicted.contains(&input.outpoint.txid))
            {
                changed |= evicted.insert(orphan.signature().to_string());
            }
        }
        if !changed {
            return;
        }
    }
}

fn compare_package_fee_rate(left: &EvictionPackage, right: &EvictionPackage) -> Ordering {
    left.fee
        .saturating_mul(right.economic_bytes)
        .cmp(&right.fee.saturating_mul(left.economic_bytes))
        .then_with(|| left.fee.cmp(&right.fee))
        .then_with(|| left.tie_break.cmp(&right.tie_break))
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

    fn dummy_mine(recipient: &str, signature_digit: char) -> Transaction {
        Transaction::Mine {
            recipient: recipient.to_string(),
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
    fn full_mempool_accepts_a_higher_fee_independent_transaction() {
        let alice = Wallet::from_seed("pending-limit-alice");
        let bob = Wallet::from_seed("pending-limit-bob");
        let mut ledger = Ledger::new(
            BTreeMap::from([
                (alice.address().to_string(), 10_000_000),
                (bob.address().to_string(), 10),
            ]),
            1,
        );
        let candidate = ledger
            .build_transfer(&alice, bob.address(), 1, 5_000_000)
            .unwrap();
        ledger.pending = (0..MAX_PENDING_TRANSACTIONS)
            .map(|index| {
                let digit = char::from_digit((index % 15 + 1) as u32, 16).unwrap();
                let mut transaction = dummy_mine(bob.address(), digit);
                if let Transaction::Mine { signature, .. } = &mut transaction {
                    *signature = format!("{index:064x}");
                }
                transaction
            })
            .collect();
        ledger.refresh_pending_pool_byte_counters().unwrap();

        assert_eq!(ledger.pending.len(), 10_000);
        assert!(ledger.submit_transaction(candidate.clone()).unwrap());
        assert_eq!(ledger.pending.len(), 10_000);
        assert!(ledger.has_transaction(candidate.signature()));
        assert_eq!(
            ledger.pending_bytes,
            serialized_pool_len(&ledger.pending).unwrap()
        );
        assert_eq!(ledger.orphan_bytes, 0);
    }

    #[test]
    fn full_mempool_rejects_a_lower_fee_candidate_without_mutation() {
        let alice = Wallet::from_seed("lower-fee-candidate-alice");
        let bob = Wallet::from_seed("lower-fee-candidate-bob");
        let mut ledger = Ledger::new(
            BTreeMap::from([
                (alice.address().to_string(), 10),
                (bob.address().to_string(), 10),
            ]),
            1,
        );
        let candidate = ledger.build_transfer(&alice, bob.address(), 1, 1).unwrap();
        ledger.pending = (0..MAX_PENDING_TRANSACTIONS)
            .map(|index| {
                let mut transaction = dummy_mine(bob.address(), 'd');
                if let Transaction::Mine { signature, .. } = &mut transaction {
                    *signature = format!("{index:064x}");
                }
                transaction
            })
            .collect();
        ledger.refresh_pending_pool_byte_counters().unwrap();
        let before = ledger.pending.clone();
        let before_bytes = ledger.pending_bytes;

        let error = ledger.submit_transaction(candidate).unwrap_err();

        assert!(error.to_string().contains("candidate fee rate"));
        assert_eq!(ledger.pending, before);
        assert_eq!(ledger.pending_bytes, before_bytes);
        assert!(ledger.orphans.is_empty());
        assert_eq!(ledger.orphan_bytes, 0);
    }

    #[test]
    fn eviction_package_includes_pending_and_orphan_descendants() {
        let wallet = Wallet::from_seed("eviction-package-wallet");
        let parent_signature = "1".repeat(128);
        let child_signature = "2".repeat(128);
        let orphan_signature = "3".repeat(128);
        let parent = synthetic_dependency_transaction(
            wallet.address(),
            "a".repeat(64),
            parent_signature.clone(),
            1,
        );
        let child = synthetic_dependency_transaction(
            wallet.address(),
            parent_signature,
            child_signature.clone(),
            1,
        );
        let orphan = synthetic_dependency_transaction(
            wallet.address(),
            child_signature,
            orphan_signature.clone(),
            u64::MAX,
        );
        let candidate = synthetic_dependency_transaction(
            wallet.address(),
            "b".repeat(64),
            "f".repeat(128),
            10_000,
        );
        let pending = vec![parent, child];
        let candidate_bytes = pending_pool_item_bytes(&candidate).unwrap();
        let mut padded = pending.clone();
        padded.extend(
            (pending.len()..MAX_PENDING_TRANSACTIONS).map(|_| dummy_mine(wallet.address(), 'e')),
        );

        let mut ledger = Ledger::new(BTreeMap::new(), 1);
        ledger.pending = padded;
        ledger.orphans = vec![orphan];
        ledger.refresh_pending_pool_byte_counters().unwrap();

        ledger
            .make_pending_room(&candidate, candidate_bytes)
            .unwrap();

        assert!(!ledger.has_transaction(&"1".repeat(128)));
        assert!(!ledger.has_transaction(&"2".repeat(128)));
        assert!(!ledger.has_transaction(&orphan_signature));
        assert_eq!(
            ledger.pending_bytes,
            serialized_pool_len(&ledger.pending).unwrap()
        );
        assert_eq!(
            ledger.orphan_bytes,
            serialized_pool_len(&ledger.orphans).unwrap()
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
        ledger.orphans = vec![dummy_mine(wallet.address(), 'e'); MAX_ORPHAN_TRANSACTIONS];

        assert_eq!(ledger.orphans.len(), 1_024);
        assert!(
            ledger
                .submit_transaction(orphan)
                .unwrap_err()
                .to_string()
                .contains("orphan transaction pool is full")
        );
    }

    fn synthetic_dependency_transaction(
        owner: &str,
        parent: String,
        signature: String,
        fee: u64,
    ) -> Transaction {
        Transaction::Transfer {
            inputs: vec![super::super::TxInput {
                outpoint: OutPoint {
                    txid: parent,
                    index: 0,
                },
                owner: owner.to_string(),
                signature: signature.clone(),
            }],
            outputs: vec![TxOutput {
                address: owner.to_string(),
                amount: 1,
            }],
            fee,
            signature,
        }
    }
}
