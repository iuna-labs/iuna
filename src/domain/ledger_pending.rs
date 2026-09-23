use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};

use super::ledger_mempool::pending_pool_item_bytes;
use super::ledger_ops::{
    apply_spendable_pending_transaction, apply_transaction, best_selectable_burn_from_index,
    best_selectable_transaction_index, compact_block_context, ensure_transaction_fits_empty_block,
    estimated_block_selection_size_bytes, transaction_has_missing_inputs,
    validate_transaction_inputs, validate_transaction_outputs,
};
use super::ledger_v2::apply_prevalidated_transaction_v2_to_utxos;
use super::mine_policy::{
    MINE_MAX_ANCHOR_AGE_BLOCKS, mine_anchor, mine_anchor_count_before_height,
};
use super::selection::{BlockSelection, TransactionKind, fee_rate_key};
use super::transaction::{
    UnsignedTxInput, transaction_inputs_available, transaction_inputs_spent_by,
};
use super::validation::{
    validate_address, validate_hash, validate_signature, validate_stratum_header,
};
use super::{
    AddressNetwork, Amount, BurnBundleSection, FinalizerMode, Ledger, MAX_PENDING_POOL_BYTES,
    MAX_PENDING_TRANSACTIONS, MINE_ACTIONS_PER_ANCHOR_LIMIT, OutPoint, Transaction, TxOutput,
    hex_encode, transaction_v2_is_active,
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
                    && self.validate_transaction_anchor_for_pending(tx).is_ok()
                    && apply_transaction(
                        tx,
                        &mut utxos,
                        &self.transaction_signing_domain_for_pending(tx),
                    )
                    .is_ok()
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
            FinalizerMode::Ticket,
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
            FinalizerMode::Recovery,
            burn_bundle_section,
        )
    }

    pub(super) fn select_block_transactions_with_required_burn_owner(
        &self,
        required_burn_owner: Option<&str>,
        required_burn_signature: Option<&str>,
        finalizer_mode: FinalizerMode,
        burn_bundle_section: &BurnBundleSection,
    ) -> Result<BlockSelection> {
        let block_context = compact_block_context(self);
        let signing_domain = self.transaction_signing_domain();
        let mut utxos = self.utxos.clone();
        let mut remaining = self
            .valid_pending_transactions()
            .into_iter()
            .filter(|transaction| self.transaction_is_eligible_for_next_block(transaction))
            .collect::<Vec<_>>();
        let mut selected = Vec::new();
        let height = self.height().saturating_add(1);
        let domain = self.transaction_v2_domain()?;
        let network = AddressNetwork::from_profile_id(&self.launch_profile.profile_id);
        let mut remaining_v2 = if transaction_v2_is_active(height) {
            self.pending_v2
                .iter()
                .filter(|transaction| self.transaction_v2_is_eligible_for_next_block(transaction))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let mut selected_v2 = Vec::new();

        let required_burn_signatures = burn_bundle_section
            .required_burns()
            .into_iter()
            .map(|burn| burn.signature().to_string())
            .collect::<BTreeSet<_>>();
        let required_burns_v2 = burn_bundle_section
            .required_burns_v2()
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        let mut selected_required_burn_signatures = BTreeSet::new();
        let mut selected_required_burns_v2 = BTreeSet::new();

        let mut anchor_v2_index = None;
        let anchor_index = if let Some(signature) = required_burn_signature {
            let legacy = remaining
                .iter()
                .position(|transaction| transaction.signature() == signature);
            if legacy.is_none() {
                anchor_v2_index = remaining_v2.iter().position(|transaction| {
                    transaction
                        .transaction_id(&domain)
                        .ok()
                        .is_some_and(|id| hex_encode(id) == signature)
                });
            }
            if legacy.is_none() && anchor_v2_index.is_none() {
                bail!("required burn {signature} is not pending");
            }
            legacy
        } else if let Some(owner) = required_burn_owner {
            let legacy =
                best_selectable_burn_from_index(&remaining, &utxos, owner, &signing_domain);
            if legacy.is_none() {
                anchor_v2_index = remaining_v2.iter().position(|transaction| {
                    transaction.is_burn()
                        && transaction
                            .burn_legacy_owner()
                            .ok()
                            .flatten()
                            .is_some_and(|candidate| candidate == owner)
                });
            }
            legacy
        } else {
            let legacy = best_selectable_transaction_index(
                &remaining,
                &utxos,
                Some(TransactionKind::Burn),
                &signing_domain,
            );
            if legacy.is_none() {
                anchor_v2_index = remaining_v2
                    .iter()
                    .position(|transaction| transaction.is_burn());
            }
            legacy
        };
        if let Some(index) = anchor_index {
            let tx = remaining.remove(index);
            let signature = tx.signature().to_string();
            self.select_required_anchor_burn(tx, required_burn_owner, &mut utxos, &mut selected)?;
            if required_burn_signatures.contains(&signature) {
                selected_required_burn_signatures.insert(signature);
            }
        }
        if let Some(index) = anchor_v2_index {
            let transaction = remaining_v2.remove(index);
            if !transaction.is_burn() {
                bail!("required block anchor must be a burn transaction");
            }
            if let Some(owner) = required_burn_owner {
                if transaction.burn_legacy_owner()?.as_deref() != Some(owner) {
                    bail!("required block anchor burn must be from the recovery finalizer");
                }
            }
            apply_prevalidated_transaction_v2_to_utxos(transaction, &domain, network, &mut utxos)
                .context("required transaction v2 anchor burn is not spendable")?;
            let envelope = hex_encode(transaction.encode(&domain)?);
            if required_burns_v2.contains(&envelope) {
                selected_required_burns_v2.insert(envelope.clone());
            }
            selected_v2.push(envelope);
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
        let mut index = 0;
        while index < remaining_v2.len()
            && selected_required_burns_v2.len() < required_burns_v2.len()
        {
            let envelope = hex_encode(remaining_v2[index].encode(&domain)?);
            if !required_burns_v2.contains(&envelope) {
                index += 1;
                continue;
            }
            let transaction = remaining_v2.remove(index);
            if !transaction.is_burn() {
                bail!("attested transaction v2 must be a burn");
            }
            if selected.len().saturating_add(selected_v2.len())
                >= self.launch_profile.max_block_transactions
            {
                bail!(
                    "attested transaction v2 burns do not fit within the block transaction count limit"
                );
            }
            apply_prevalidated_transaction_v2_to_utxos(transaction, &domain, network, &mut utxos)
                .context("attested transaction v2 burn is not spendable")?;
            selected_required_burns_v2.insert(envelope.clone());
            selected_v2.push(envelope);
        }
        if selected_required_burns_v2.len() != required_burns_v2.len() {
            bail!("attested transaction v2 burn is not pending");
        }

        let required_selection = BlockSelection {
            transactions: selected.clone(),
            transactions_v2: selected_v2.clone(),
        };
        if estimated_block_selection_size_bytes(
            block_context,
            &required_selection,
            finalizer_mode,
            burn_bundle_section,
        )? > self.launch_profile.max_block_bytes
        {
            bail!("required block content does not fit in the block");
        }

        let mut selection = BlockSelection {
            transactions: selected,
            transactions_v2: selected_v2,
        };

        loop {
            if selection
                .transactions
                .len()
                .saturating_add(selection.transactions_v2.len())
                >= self.launch_profile.max_block_transactions
            {
                break;
            }

            let legacy =
                best_selectable_transaction_index(&remaining, &utxos, None, &signing_domain)
                    .map(|index| (index, fee_rate_key(&remaining[index])));
            let v2 = remaining_v2
                .iter()
                .enumerate()
                .filter_map(|(index, transaction)| {
                    let mut candidate_utxos = utxos.clone();
                    apply_prevalidated_transaction_v2_to_utxos(
                        transaction,
                        &domain,
                        network,
                        &mut candidate_utxos,
                    )
                    .ok()?;
                    let bytes = transaction.encoded_size_bytes(&domain).ok()?;
                    let rate = if bytes == 0 {
                        0
                    } else {
                        u128::from(transaction.fee()) * 1_000_000 / bytes as u128
                    };
                    Some((index, rate, candidate_utxos))
                })
                .max_by_key(|(index, rate, _)| (*rate, std::cmp::Reverse(*index)));

            if legacy.is_none() && v2.is_none() {
                break;
            }
            if v2
                .as_ref()
                .is_some_and(|(_, v2_rate, _)| legacy.is_none_or(|(_, rate)| *v2_rate > rate))
            {
                let (index, _, candidate_utxos) = v2.expect("v2 candidate was selected");
                let transaction = remaining_v2.remove(index);
                let mut candidate = selection.clone();
                candidate
                    .transactions_v2
                    .push(hex_encode(transaction.encode(&domain)?));
                if estimated_block_selection_size_bytes(
                    block_context,
                    &candidate,
                    finalizer_mode,
                    burn_bundle_section,
                )? <= self.launch_profile.max_block_bytes
                {
                    utxos = candidate_utxos;
                    selection = candidate;
                }
            } else {
                let (index, _) = legacy.expect("legacy candidate was selected");
                let transaction = remaining.remove(index);
                let mut candidate = selection.clone();
                candidate.transactions.push(transaction.clone());
                if estimated_block_selection_size_bytes(
                    block_context,
                    &candidate,
                    finalizer_mode,
                    burn_bundle_section,
                )? <= self.launch_profile.max_block_bytes
                {
                    apply_transaction(&transaction, &mut utxos, &signing_domain)?;
                    selection = candidate;
                }
            }
        }
        Ok(selection)
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
        self.validate_transaction_anchor_for_pending(transaction)?;
        ensure_transaction_fits_empty_block(
            compact_block_context(self),
            transaction,
            self.launch_profile.max_block_bytes,
        )?;
        self.validate_mine_anchor_available(transaction)?;
        let mut utxos = self.utxos_after_spendable_pending()?;
        apply_transaction(
            transaction,
            &mut utxos,
            &self.transaction_signing_domain_for_pending(transaction),
        )
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
                return Err(super::ValidationError::MineAnchorLimitReached.into());
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
                    && apply_transaction(
                        transaction,
                        &mut utxos,
                        &self.transaction_signing_domain_for_pending(transaction),
                    )
                    .is_ok()
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
                let next_height = self.height().saturating_add(1);
                if next_height >= super::HYBRID_REWARD_ACTIVATION_HEIGHT {
                    let address = super::decode_versioned_address(
                        recipient,
                        super::AddressNetwork::from_profile_id(&self.launch_profile.profile_id),
                    )?;
                    if address.version != super::AddressVersion::HybridKeyCommitment {
                        bail!("mine reward must use a hybrid address at height {next_height}");
                    }
                } else {
                    validate_address(recipient, "mine recipient")?;
                }
                validate_hash(anchor, "mine transaction anchor")?;
                validate_hash(signature, "mine transaction proof hash")?;
                if let Some(proof_header) = proof_header {
                    validate_stratum_header(proof_header)?;
                }
                let anchor_block = self
                    .chain
                    .iter()
                    .find(|block| block.hash == *anchor)
                    .ok_or(super::ValidationError::MineAnchorNotOnChain)?;
                let anchor_age = self.tip().height.saturating_sub(anchor_block.height);
                if anchor_age > MINE_MAX_ANCHOR_AGE_BLOCKS {
                    bail!("mine transaction anchor is too old");
                }
                let required_difficulty =
                    self.mine_difficulty_bits_for_anchor_height(anchor_block.height);
                if *difficulty_bits != required_difficulty {
                    bail!(
                        "mine transaction difficulty is invalid: got {}, required {} at anchor height {}",
                        difficulty_bits,
                        required_difficulty,
                        anchor_block.height
                    );
                }
            }
        }
        Ok(())
    }

    pub(super) fn validate_transaction_anchor_for_block(
        &self,
        transaction: &Transaction,
        height: u64,
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
            .context("burn transaction is missing its chain anchor")?;
        if anchor != self.tip().prev_hash {
            bail!("burn transaction anchor does not match the block grandparent");
        }
        Ok(())
    }

    pub(super) fn validate_transaction_anchor_for_pending(
        &self,
        transaction: &Transaction,
    ) -> Result<()> {
        if !transaction.is_burn() {
            return Ok(());
        }

        let next_height = self.height().saturating_add(1);
        let following_height = self.height().saturating_add(2);
        let anchor = transaction.burn_anchor();

        // Keep both pipeline stages: burns anchored to the tip's parent are
        // eligible now, while burns anchored to the tip wait one more block.
        if next_height < super::TIP_BOUND_BURN_ACTIVATION_HEIGHT && anchor.is_none() {
            return Ok(());
        }
        if next_height >= super::TIP_BOUND_BURN_ACTIVATION_HEIGHT
            && anchor == Some(self.tip().prev_hash.as_str())
        {
            return Ok(());
        }
        if following_height >= super::TIP_BOUND_BURN_ACTIVATION_HEIGHT
            && anchor == Some(self.tip_hash())
        {
            return Ok(());
        }

        Err(super::ValidationError::BurnAnchorOutsidePendingWindow.into())
    }

    pub(crate) fn transaction_is_eligible_for_next_block(&self, transaction: &Transaction) -> bool {
        self.validate_transaction_anchor_for_block(transaction, self.height().saturating_add(1))
            .is_ok()
    }

    pub(super) fn transaction_signing_domain_for_pending(
        &self,
        transaction: &Transaction,
    ) -> super::TransactionSigningDomain {
        let height = if transaction.is_burn()
            && transaction.burn_anchor() == Some(self.tip_hash())
            && self.height().saturating_add(2) >= super::TIP_BOUND_BURN_ACTIVATION_HEIGHT
        {
            self.height().saturating_add(2)
        } else {
            self.height().saturating_add(1)
        };
        self.transaction_signing_domain_at(height)
    }

    pub(super) fn utxos_after_valid_pending(&self) -> Result<BTreeMap<OutPoint, TxOutput>> {
        let mut utxos = self.utxos.clone();
        for pending in self.valid_pending_transactions() {
            apply_transaction(
                &pending,
                &mut utxos,
                &self.transaction_signing_domain_for_pending(&pending),
            )?;
        }
        Ok(utxos)
    }

    pub(super) fn utxos_after_spendable_pending(&self) -> Result<BTreeMap<OutPoint, TxOutput>> {
        let mut utxos = self.utxos.clone();
        for pending in self.valid_pending_transactions() {
            if matches!(pending, Transaction::Mine { .. }) {
                continue;
            }
            if apply_spendable_pending_transaction(
                &pending,
                &mut utxos,
                &self.transaction_signing_domain_for_pending(&pending),
            )
            .is_err()
            {
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
    fn burns_queue_for_one_block_then_expire_after_their_inclusion_height() {
        let wallet = Wallet::from_seed("tip-bound-burn-wallet");
        let mut ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 10)]), 1);

        extend_synthetic_chain_to(
            &mut ledger,
            super::super::TIP_BOUND_BURN_ACTIVATION_HEIGHT - 3,
        );
        let legacy_burn = ledger.build_burn(&wallet, 1, 1).unwrap();
        assert_eq!(legacy_burn.burn_anchor(), None);

        extend_synthetic_chain_to(
            &mut ledger,
            super::super::TIP_BOUND_BURN_ACTIVATION_HEIGHT - 2,
        );
        let anchored_burn = ledger.build_burn(&wallet, 1, 1).unwrap();
        assert_eq!(anchored_burn.burn_anchor(), Some(ledger.tip_hash()));
        assert!(ledger.submit_transaction(anchored_burn.clone()).unwrap());
        assert_eq!(
            ledger.valid_pending_transactions(),
            vec![anchored_burn.clone()]
        );
        assert!(
            ledger
                .select_block_transactions_with_required_burn_owner(
                    None,
                    None,
                    FinalizerMode::Ticket,
                    &BurnBundleSection::default(),
                )
                .unwrap()
                .transactions
                .is_empty()
        );

        extend_synthetic_chain_to(
            &mut ledger,
            super::super::TIP_BOUND_BURN_ACTIVATION_HEIGHT - 1,
        );
        assert_eq!(
            ledger.valid_pending_transactions(),
            vec![anchored_burn.clone()]
        );
        assert_eq!(
            ledger
                .select_block_transactions_with_required_burn_owner(
                    None,
                    None,
                    FinalizerMode::Ticket,
                    &BurnBundleSection::default(),
                )
                .unwrap()
                .transactions,
            vec![anchored_burn]
        );

        extend_synthetic_chain_to(&mut ledger, super::super::TIP_BOUND_BURN_ACTIVATION_HEIGHT);
        assert!(ledger.valid_pending_transactions().is_empty());
    }

    #[test]
    fn finalizer_burn_for_next_block_anchors_to_the_current_tip_parent() {
        let wallet = Wallet::from_seed("next-block-burn-wallet");
        let mut ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 10)]), 1);
        extend_synthetic_chain_to(
            &mut ledger,
            super::super::TIP_BOUND_BURN_ACTIVATION_HEIGHT - 1,
        );

        let future_burn = ledger.build_burn(&wallet, 1, 1).unwrap();
        let next_block_burn = ledger.build_burn_for_next_block(&wallet, 1, 1).unwrap();

        assert_eq!(future_burn.burn_anchor(), Some(ledger.tip_hash()));
        assert_eq!(
            next_block_burn.burn_anchor(),
            Some(ledger.tip().prev_hash.as_str())
        );
        assert!(!ledger.transaction_is_eligible_for_next_block(&future_burn));
        assert!(ledger.transaction_is_eligible_for_next_block(&next_block_burn));
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
                FinalizerMode::Ticket,
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
    fn ticket_block_selection_reserves_space_for_leader_proof() {
        let finalizer = Wallet::from_seed("ticket-selection-leader-proof-finalizer");
        let mut ledger = Ledger::new(BTreeMap::from([(finalizer.address().to_string(), 10)]), 1);
        let anchor = ledger.build_burn(&finalizer, 1, 1).unwrap();
        ledger.submit_transaction(anchor.clone()).unwrap();

        let required_selection = BlockSelection {
            transactions: vec![anchor.clone()],
            transactions_v2: Vec::new(),
        };
        let ticket_bytes = estimated_block_selection_size_bytes(
            compact_block_context(&ledger),
            &required_selection,
            FinalizerMode::Ticket,
            &BurnBundleSection::default(),
        )
        .unwrap();
        let recovery_bytes = estimated_block_selection_size_bytes(
            compact_block_context(&ledger),
            &required_selection,
            FinalizerMode::Recovery,
            &BurnBundleSection::default(),
        )
        .unwrap();
        assert!(ticket_bytes > recovery_bytes);

        ledger.launch_profile.max_block_bytes = ticket_bytes - 1;
        assert!(recovery_bytes <= ledger.launch_profile.max_block_bytes);

        let error = ledger
            .select_block_transactions_with_burn_section(
                finalizer.address(),
                Some(anchor.signature()),
                &BurnBundleSection::default(),
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("required block content does not fit in the block")
        );

        let recovery_selection = ledger
            .select_recovery_block_transactions_with_burn_section(
                finalizer.address(),
                Some(anchor.signature()),
                &BurnBundleSection::default(),
            )
            .unwrap();
        assert_eq!(recovery_selection.transactions, vec![anchor]);
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
            burns_v2: Vec::new(),
        };

        assert!(
            ledger
                .select_block_transactions_with_required_burn_owner(
                    Some(finalizer.address()),
                    Some(anchor.signature()),
                    FinalizerMode::Ticket,
                    &section,
                )
                .unwrap_err()
                .to_string()
                .contains("transaction count limit")
        );
    }
}
