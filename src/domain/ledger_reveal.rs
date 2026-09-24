use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};

use super::ledger_ops::verify_address_signature;
use super::reveal::{burn_bundle_slot_mask, burn_committee_mask};
use super::{
    Amount, BURN_COMMITTEE_SIZE, Block, BurnBundle, BurnBundlePayload, BurnBundleSection,
    BurnBundleSignature, BurnCommitteeMember, FinalizerMode, Ledger, MAX_BURN_BUNDLE_BYTES,
    MaskedBurn, MaskedBurnV2, OBJECTIVE_FINALITY_ACTIVATION_HEIGHT, Transaction, Wallet,
    hex_encode,
};

impl Ledger {
    pub fn burn_bundle_attestations_required_for_next_block(&self) -> bool {
        self.pending.iter().any(|transaction| {
            transaction.is_burn() && self.transaction_is_eligible_for_next_block(transaction)
        }) || self.pending_v2.iter().any(|transaction| {
            transaction.is_burn() && self.transaction_v2_is_eligible_for_next_block(transaction)
        })
    }

    pub fn explicit_burn_bundle_signatures_required_for_next_block(
        &self,
        finalizer_mode: FinalizerMode,
        finalizer_rank: u32,
        _finalizer: &str,
    ) -> usize {
        let committee_size = match finalizer_mode {
            FinalizerMode::Ticket => self
                .burn_committee_for_next_ticket_block(finalizer_rank)
                .len(),
            FinalizerMode::Recovery => 1,
        };
        self.required_explicit_burn_signatures(
            self.height() + 1,
            finalizer_mode,
            finalizer_rank,
            committee_size,
        )
    }

    pub fn build_burn_bundle(&self, wallet: &Wallet) -> Result<Option<BurnBundle>> {
        Ok(self.build_burn_bundles(wallet)?.into_iter().next())
    }

    pub fn build_burn_bundles(&self, wallet: &Wallet) -> Result<Vec<BurnBundle>> {
        let height = self.tip().height + 1;
        let prev_hash = self.tip().hash.clone();
        let memberships = self.burn_committee_memberships_for_next_block(wallet.address());
        if memberships.is_empty() {
            return Ok(Vec::new());
        }
        let burns = self
            .valid_pending_transactions()
            .into_iter()
            .filter(|transaction| {
                transaction.is_burn() && self.transaction_is_eligible_for_next_block(transaction)
            })
            .collect::<Vec<_>>();
        let domain = self.transaction_v2_domain()?;
        let burns_v2 = self
            .pending_v2
            .iter()
            .filter(|transaction| {
                transaction.is_burn() && self.transaction_v2_is_eligible_for_next_block(transaction)
            })
            .map(|transaction| {
                Ok((
                    transaction.fee(),
                    hex_encode(transaction.transaction_id(&domain)?),
                    hex_encode(transaction.encode(&domain)?),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut candidates = burns
            .into_iter()
            .map(|burn| (burn.fee(), burn.signature().to_string(), Some(burn), None))
            .chain(
                burns_v2
                    .into_iter()
                    .map(|(fee, id, envelope)| (fee, id, None, Some(envelope))),
            )
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

        let mut bundles = Vec::new();
        for member in memberships {
            let mut selected = Vec::new();
            let mut selected_v2 = Vec::new();
            for (_, _, legacy_burn, v2_envelope) in &candidates {
                let mut candidate = selected.clone();
                let mut candidate_v2 = selected_v2.clone();
                if let Some(burn) = legacy_burn {
                    candidate.push(burn.clone());
                }
                if let Some(envelope) = v2_envelope {
                    candidate_v2.push(envelope.clone());
                }
                let bundle = wallet.burn_bundle(BurnBundlePayload {
                    height,
                    prev_hash: prev_hash.clone(),
                    slot: member.slot,
                    member: wallet.address().to_string(),
                    reward_address: (height >= super::HYBRID_REWARD_ACTIVATION_HEIGHT)
                        .then(|| self.wallet_reward_address(wallet, height)),
                    burns: candidate.clone(),
                    burns_v2: candidate_v2.clone(),
                });
                if bundle.serialized_size_bytes()? <= MAX_BURN_BUNDLE_BYTES {
                    selected = candidate;
                    selected_v2 = candidate_v2;
                }
            }
            bundles.push(
                wallet.burn_bundle(BurnBundlePayload {
                    height,
                    prev_hash: prev_hash.clone(),
                    slot: member.slot,
                    member: wallet.address().to_string(),
                    reward_address: (height >= super::HYBRID_REWARD_ACTIVATION_HEIGHT)
                        .then(|| self.wallet_reward_address(wallet, height)),
                    burns: selected,
                    burns_v2: selected_v2,
                }),
            );
        }
        Ok(bundles)
    }

    pub fn validate_next_block_burn_bundles(
        &self,
        bundles: Vec<BurnBundle>,
    ) -> Result<Vec<BurnBundle>> {
        let expected_height = self.tip().height + 1;
        let expected_prev_hash = self.tip().hash.clone();
        self.validate_burn_bundles_for_any_next_ticket_block(
            expected_height,
            &expected_prev_hash,
            bundles,
        )
    }

    pub(super) fn validate_next_block_burn_bundles_for_finalizer_rank(
        &self,
        finalizer_rank: u32,
        bundles: Vec<BurnBundle>,
    ) -> Result<Vec<BurnBundle>> {
        let expected_height = self.tip().height + 1;
        let expected_prev_hash = self.tip().hash.clone();
        let committee = self
            .burn_committee_for_next_ticket_block(finalizer_rank)
            .into_iter()
            .map(|member| (member.slot, member))
            .collect::<BTreeMap<_, _>>();
        let validated = self.validate_burn_bundles_for_committee(
            expected_height,
            &expected_prev_hash,
            &committee,
            bundles,
        )?;
        let required_signatures = self.required_explicit_burn_signatures(
            expected_height,
            FinalizerMode::Ticket,
            finalizer_rank,
            committee.len(),
        );
        let explicit_signatures = validated.iter().filter(|bundle| bundle.slot != 0).count();
        if explicit_signatures < required_signatures {
            bail!(
                "not enough burn bundle signatures collected: got {explicit_signatures}, need {required_signatures}"
            );
        }
        Ok(validated)
    }

    pub(crate) fn precheck_next_block_burn_bundle(&self, bundle: &BurnBundle) -> Result<()> {
        let expected_height = self.tip().height + 1;
        let expected_prev_hash = self.tip().hash.clone();
        let max_rank = self
            .finalizer_rank_count_for_next_block()
            .min(BURN_COMMITTEE_SIZE);
        let mut first_error = None;
        for rank in 0..max_rank {
            let committee = self
                .burn_committee_for_next_ticket_block(rank as u32)
                .into_iter()
                .map(|member| (member.slot, member))
                .collect::<BTreeMap<_, _>>();
            match self.precheck_burn_bundle_for_block(
                expected_height,
                &expected_prev_hash,
                &committee,
                bundle,
            ) {
                Ok(()) => return Ok(()),
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        Err(first_error.unwrap_or_else(|| anyhow::anyhow!("burn bundle slot is not assigned")))
    }

    #[cfg(test)]
    pub fn test_burn_bundle(&self, wallet: &Wallet, burns: Vec<Transaction>) -> BurnBundle {
        let height = self.tip().height + 1;
        let prev_hash = self.tip().hash.clone();
        let member = self
            .burn_committee_for_next_block()
            .into_iter()
            .find(|member| member.owner == wallet.address())
            .expect("test wallet must be a burn committee member");
        wallet.burn_bundle(BurnBundlePayload {
            height,
            prev_hash,
            slot: member.slot,
            member: wallet.address().to_string(),
            reward_address: (height >= super::HYBRID_REWARD_ACTIVATION_HEIGHT)
                .then(|| self.wallet_reward_address(wallet, height)),
            burns,
            burns_v2: Vec::new(),
        })
    }

    pub(super) fn burn_bundle_section_from_bundles(
        &self,
        bundles: Vec<BurnBundle>,
    ) -> Result<BurnBundleSection> {
        let signatures = bundles
            .iter()
            .filter(|bundle| bundle.slot != 0)
            .map(|bundle| BurnBundleSignature {
                slot: bundle.slot,
                member: bundle.member.clone(),
                reward_address: bundle.reward_address.clone(),
                signature: bundle.signature.clone(),
            })
            .collect::<Vec<_>>();
        let mut by_signature: BTreeMap<String, MaskedBurn> = BTreeMap::new();
        let mut by_v2_id: BTreeMap<String, MaskedBurnV2> = BTreeMap::new();
        for bundle in bundles {
            let slot_mask = if bundle.slot == 0 {
                0
            } else {
                burn_bundle_slot_mask(bundle.slot).unwrap_or(0)
            };
            for burn in bundle.burns {
                by_signature
                    .entry(burn.signature().to_string())
                    .and_modify(|masked| masked.bundle_mask |= slot_mask)
                    .or_insert(MaskedBurn {
                        burn,
                        bundle_mask: slot_mask,
                    });
            }
            for envelope in bundle.burns_v2 {
                by_v2_id
                    .entry(envelope.clone())
                    .and_modify(|masked| masked.bundle_mask |= slot_mask)
                    .or_insert(MaskedBurnV2 {
                        envelope,
                        bundle_mask: slot_mask,
                    });
            }
        }
        let mut burns = by_signature.into_values().collect::<Vec<_>>();
        burns.sort_by(|left, right| {
            right
                .burn
                .fee()
                .cmp(&left.burn.fee())
                .then_with(|| left.burn.signature().cmp(right.burn.signature()))
        });
        let domain = self.transaction_v2_domain()?;
        let mut burns_v2 = by_v2_id
            .into_values()
            .map(|masked| {
                let burn = super::ledger_v2::decode_canonical_transaction_v2_envelope(
                    &masked.envelope,
                    &domain,
                )?;
                Ok((
                    burn.fee(),
                    hex_encode(burn.transaction_id(&domain)?),
                    masked,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        burns_v2.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
        Ok(BurnBundleSection {
            signatures,
            burns,
            burns_v2: burns_v2.into_iter().map(|(_, _, masked)| masked).collect(),
        })
    }

    pub(super) fn validate_burn_bundle_section_for_block(&self, block: &Block) -> Result<()> {
        let section = &block.burn_bundle_section;
        if section.signatures.len() > BURN_COMMITTEE_SIZE.saturating_sub(1) {
            bail!("block has too many burn bundle signatures");
        }
        if section
            .signatures
            .windows(2)
            .any(|pair| pair[0].slot >= pair[1].slot)
        {
            bail!("burn bundle signatures are not in slot order");
        }
        let committee = self
            .burn_committee_for_block(block)
            .into_iter()
            .map(|member| (member.slot, member))
            .collect::<BTreeMap<_, _>>();
        let mut seen_slots = BTreeSet::new();
        let mut seen_members = BTreeSet::new();
        let mut included_mask = 0_u8;
        for signature in &section.signatures {
            if usize::from(signature.slot) >= BURN_COMMITTEE_SIZE {
                bail!("burn bundle slot is invalid");
            }
            if signature.slot == 0 {
                bail!("finalizer burn attestation is implicit");
            }
            if !seen_slots.insert(signature.slot) {
                bail!("duplicate burn bundle slot");
            }
            if !seen_members.insert(signature.member.clone()) {
                bail!("duplicate burn bundle member");
            }
            let member = committee
                .get(&signature.slot)
                .context("burn bundle slot is not assigned")?;
            if signature.member != member.owner {
                bail!("burn bundle member is not assigned to slot");
            }
            self.validate_reward_address(
                block.height,
                signature.reward_address.as_deref(),
                "committee member",
            )?;
            included_mask |= burn_bundle_slot_mask(signature.slot)?;
        }
        let required_signatures = self.required_explicit_burn_signatures(
            block.height,
            block.finalizer_mode,
            block.finalizer_rank,
            committee.len(),
        );
        if section.signatures.len() < required_signatures {
            bail!(
                "block has too few burn bundle signatures: got {}, need {required_signatures}",
                section.signatures.len()
            );
        }

        let mut seen_burns = BTreeSet::new();
        let mut previous_key: Option<(Amount, String)> = None;
        for masked in &section.burns {
            if masked.bundle_mask & !burn_committee_mask() != 0 {
                bail!("masked burn references an invalid burn bundle slot");
            }
            if masked.bundle_mask & !included_mask != 0 {
                bail!("masked burn references a missing burn bundle signature");
            }
            if !seen_burns.insert(masked.burn.signature().to_string()) {
                bail!("duplicate burn in burn bundle section");
            }
            if !masked.burn.is_burn() {
                bail!("burn bundle section contains a non-burn transaction");
            }
            if matching_burn_by_signature(&masked.burn, &block.transactions).is_none() {
                bail!("attested burn is not included in the block");
            }
            self.validate_transaction_terms(&masked.burn)?;
            let key = (masked.burn.fee(), masked.burn.signature().to_string());
            if let Some((previous_fee, previous_signature)) = &previous_key {
                if key.0 > *previous_fee || key.0 == *previous_fee && key.1 < *previous_signature {
                    bail!("burn bundle section is not fee ordered");
                }
            }
            previous_key = Some(key);
        }
        let domain = self.transaction_v2_domain()?;
        let mut seen_burns_v2 = BTreeSet::new();
        for masked in &section.burns_v2 {
            if masked.bundle_mask & !burn_committee_mask() != 0 {
                bail!("masked transaction v2 burn references an invalid burn bundle slot");
            }
            if masked.bundle_mask & !included_mask != 0 {
                bail!("masked transaction v2 burn references a missing burn bundle signature");
            }
            let burn = super::ledger_v2::decode_canonical_transaction_v2_envelope(
                &masked.envelope,
                &domain,
            )?;
            if !burn.is_burn() {
                bail!("burn bundle section contains a non-burn transaction v2");
            }
            let id = hex_encode(burn.transaction_id(&domain)?);
            if !seen_burns_v2.insert(id) {
                bail!("duplicate transaction v2 burn in burn bundle section");
            }
            if !block.transactions_v2.contains(&masked.envelope) {
                bail!("attested transaction v2 burn is not included in the block");
            }
            self.validate_transaction_v2_anchor_for_block(&burn, block.height)?;
        }

        for bundle in section.expand(block.height, &block.prev_hash) {
            if bundle.serialized_size_bytes()? > MAX_BURN_BUNDLE_BYTES {
                bail!("burn bundle exceeds max size");
            }
            verify_address_signature(
                &bundle.member,
                &bundle.canonical_payload(),
                &bundle.signature,
                "burn bundle",
            )?;
        }
        Ok(())
    }

    fn required_explicit_burn_signatures(
        &self,
        height: u64,
        finalizer_mode: FinalizerMode,
        finalizer_rank: u32,
        committee_size: usize,
    ) -> usize {
        if committee_size == 0 {
            return 0;
        }
        match finalizer_mode {
            FinalizerMode::Ticket
                if finalizer_rank == 0 && height >= OBJECTIVE_FINALITY_ACTIVATION_HEIGHT =>
            {
                objective_finality_quorum(committee_size).saturating_sub(1)
            }
            FinalizerMode::Ticket if finalizer_rank == 0 => committee_size.min(3).saturating_sub(1),
            FinalizerMode::Ticket if finalizer_rank == 1 => committee_size.min(2).saturating_sub(1),
            FinalizerMode::Ticket => 0,
            FinalizerMode::Recovery => 0,
        }
    }

    pub(super) fn block_certifies_parent(&self, block: &Block, committee_size: usize) -> bool {
        block.height > OBJECTIVE_FINALITY_ACTIVATION_HEIGHT
            && block.finalizer_mode == FinalizerMode::Ticket
            && block.finalizer_rank == 0
            && committee_size > 0
            && block.burn_bundle_section.signatures.len() + 1
                >= objective_finality_quorum(committee_size)
    }

    fn validate_burn_bundles_for_any_next_ticket_block(
        &self,
        expected_height: u64,
        expected_prev_hash: &str,
        bundles: Vec<BurnBundle>,
    ) -> Result<Vec<BurnBundle>> {
        let max_rank = self
            .finalizer_rank_count_for_next_block()
            .min(BURN_COMMITTEE_SIZE);
        let mut first_error = None;
        for rank in 0..max_rank {
            let committee = self
                .burn_committee_for_next_ticket_block(rank as u32)
                .into_iter()
                .map(|member| (member.slot, member))
                .collect::<BTreeMap<_, _>>();
            match self.validate_burn_bundles_for_committee(
                expected_height,
                expected_prev_hash,
                &committee,
                bundles.clone(),
            ) {
                Ok(validated) => return Ok(validated),
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        Err(first_error.unwrap_or_else(|| anyhow::anyhow!("no ticket committee is available")))
    }

    fn validate_burn_bundles_for_committee(
        &self,
        expected_height: u64,
        expected_prev_hash: &str,
        committee: &BTreeMap<u8, BurnCommitteeMember>,
        mut bundles: Vec<BurnBundle>,
    ) -> Result<Vec<BurnBundle>> {
        if bundles.len() > BURN_COMMITTEE_SIZE {
            bail!("block has too many burn bundles");
        }
        bundles.sort_by_key(|bundle| bundle.slot);
        if bundles.windows(2).any(|pair| pair[0].slot == pair[1].slot) {
            bail!("duplicate burn bundle slot");
        }
        let mut seen_members = BTreeSet::new();
        let domain = self.transaction_v2_domain()?;
        for bundle in &bundles {
            if !seen_members.insert(bundle.member.clone()) {
                bail!("duplicate burn bundle member");
            }
            self.precheck_burn_bundle_for_block(
                expected_height,
                expected_prev_hash,
                committee,
                bundle,
            )?;
            for burn in &bundle.burns {
                if matching_burn_by_signature(burn, &self.pending).is_none() {
                    bail!("burn bundle references a burn that is not in the mempool");
                }
            }
            for envelope in &bundle.burns_v2 {
                let burn =
                    super::ledger_v2::decode_canonical_transaction_v2_envelope(envelope, &domain)?;
                let id = burn.transaction_id(&domain)?;
                if !self
                    .pending_v2
                    .iter()
                    .any(|pending| pending.transaction_id(&domain).ok() == Some(id))
                {
                    bail!(
                        "burn bundle references a transaction v2 burn that is not in the mempool"
                    );
                }
            }
        }
        Ok(bundles)
    }

    fn precheck_burn_bundle_for_block(
        &self,
        expected_height: u64,
        expected_prev_hash: &str,
        committee: &BTreeMap<u8, BurnCommitteeMember>,
        bundle: &BurnBundle,
    ) -> Result<()> {
        if bundle.height != expected_height {
            bail!("burn bundle height is invalid");
        }
        if bundle.prev_hash != expected_prev_hash {
            return Err(super::ValidationError::BurnBundleParentMismatch.into());
        }
        if usize::from(bundle.slot) >= BURN_COMMITTEE_SIZE {
            bail!("burn bundle slot is invalid");
        }
        let member = committee
            .get(&bundle.slot)
            .context("burn bundle slot is not assigned")?;
        if bundle.member != member.owner {
            bail!("burn bundle member is not assigned to slot");
        }
        self.validate_reward_address(
            expected_height,
            bundle.reward_address.as_deref(),
            "committee member",
        )?;
        if bundle.serialized_size_bytes()? > MAX_BURN_BUNDLE_BYTES {
            bail!("burn bundle exceeds max size");
        }
        verify_address_signature(
            &bundle.member,
            &bundle.canonical_payload(),
            &bundle.signature,
            "burn bundle",
        )?;
        let mut seen_bundle_burns = BTreeSet::new();
        let mut previous_key: Option<(Amount, String)> = None;
        for burn in &bundle.burns {
            if !seen_bundle_burns.insert(burn.signature().to_string()) {
                bail!("duplicate burn in burn bundle");
            }
            if !burn.is_burn() {
                bail!("burn bundle contains a non-burn transaction");
            }
            self.validate_transaction_terms(burn)?;
            self.validate_transaction_anchor_for_block(burn, expected_height)?;
            let key = (burn.fee(), burn.signature().to_string());
            if let Some((previous_fee, previous_signature)) = &previous_key {
                if key.0 > *previous_fee || key.0 == *previous_fee && key.1 < *previous_signature {
                    bail!("burn bundle is not fee ordered");
                }
            }
            previous_key = Some(key);
        }
        let domain = self.transaction_v2_domain()?;
        let mut seen_bundle_burns_v2 = BTreeSet::new();
        let mut previous_v2_key: Option<(Amount, String)> = None;
        for envelope in &bundle.burns_v2 {
            let burn =
                super::ledger_v2::decode_canonical_transaction_v2_envelope(envelope, &domain)?;
            if !burn.is_burn() {
                bail!("burn bundle contains a non-burn transaction v2");
            }
            let id = hex_encode(burn.transaction_id(&domain)?);
            if !seen_bundle_burns_v2.insert(id.clone()) {
                bail!("duplicate transaction v2 burn in burn bundle");
            }
            self.validate_transaction_v2_anchor_for_block(&burn, expected_height)?;
            let key = (burn.fee(), id);
            if let Some((previous_fee, previous_id)) = &previous_v2_key {
                if key.0 > *previous_fee || key.0 == *previous_fee && key.1 < *previous_id {
                    bail!("transaction v2 burn bundle is not fee ordered");
                }
            }
            previous_v2_key = Some(key);
        }
        Ok(())
    }
}

pub(super) fn objective_finality_quorum(committee_size: usize) -> usize {
    if committee_size == 0 {
        0
    } else {
        committee_size.saturating_mul(2) / 3 + 1
    }
}

fn matching_burn_by_signature<'a>(
    attested: &Transaction,
    transactions: &'a [Transaction],
) -> Option<&'a Transaction> {
    transactions.iter().find(|transaction| {
        transaction.is_burn()
            && transaction.signature() == attested.signature()
            && transaction.canonical() == attested.canonical()
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::domain::{
        GenesisBurn, MICRO_IUNA, OutPoint, TxInput, TxOutput, UtxoLineageRoot, Wallet,
    };

    fn ledger() -> Ledger {
        Ledger::new(BTreeMap::new(), 1)
    }

    fn burn(signature: &str, fee: Amount) -> Transaction {
        Transaction::Burn {
            inputs: vec![TxInput {
                outpoint: OutPoint {
                    txid: format!("{signature:0<64}"),
                    index: 0,
                },
                owner: "owner".to_string(),
                signature: signature.to_string(),
            }],
            change: vec![TxOutput {
                address: "owner".to_string(),
                amount: 1,
            }],
            amount: 1,
            fee,
            anchor: None,
            signature: signature.to_string(),
        }
    }

    fn funded_ledger(wallets: &[Wallet]) -> Ledger {
        let allocations = wallets
            .iter()
            .map(|wallet| (wallet.address().to_string(), 10 * MICRO_IUNA))
            .collect::<BTreeMap<_, _>>();
        let genesis_burns = wallets
            .iter()
            .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
            .collect::<Vec<_>>();
        Ledger::new_with_genesis_burns(allocations, genesis_burns, 1).unwrap()
    }

    #[test]
    fn burn_quorum_depends_on_rank_and_available_committee() {
        let ledger = ledger();

        assert_eq!(
            ledger.required_explicit_burn_signatures(999, FinalizerMode::Ticket, 0, 5),
            2
        );
        assert_eq!(
            ledger.required_explicit_burn_signatures(999, FinalizerMode::Ticket, 0, 4),
            2
        );
        assert_eq!(
            ledger.required_explicit_burn_signatures(999, FinalizerMode::Ticket, 0, 3),
            2
        );
        assert_eq!(
            ledger.required_explicit_burn_signatures(999, FinalizerMode::Ticket, 0, 2),
            1
        );
        assert_eq!(
            ledger.required_explicit_burn_signatures(1_000, FinalizerMode::Ticket, 0, 5),
            3
        );
        assert_eq!(
            ledger.required_explicit_burn_signatures(1_000, FinalizerMode::Ticket, 0, 4),
            2
        );
        assert_eq!(
            ledger.required_explicit_burn_signatures(1_000, FinalizerMode::Ticket, 0, 3),
            2
        );
        assert_eq!(
            ledger.required_explicit_burn_signatures(1_000, FinalizerMode::Ticket, 1, 5),
            1
        );
        assert_eq!(
            ledger.required_explicit_burn_signatures(1_000, FinalizerMode::Ticket, 1, 2),
            1
        );
        assert_eq!(
            ledger.required_explicit_burn_signatures(1_000, FinalizerMode::Ticket, 2, 3),
            0
        );
        assert_eq!(
            ledger.required_explicit_burn_signatures(1_000, FinalizerMode::Recovery, 0, 3),
            0
        );
        assert_eq!(
            ledger.required_explicit_burn_signatures(1_000, FinalizerMode::Ticket, 0, 1),
            0
        );
    }

    #[test]
    fn finalizer_waits_until_the_required_burn_bundle_quorum_is_available() {
        let wallets = (0..3)
            .map(|index| Wallet::from_seed(&format!("bundle-quorum-wallet-{index}")))
            .collect::<Vec<_>>();
        let mut ledger = funded_ledger(&wallets);
        ledger.launch_profile.burn_lineage_maturity_heights = 0;
        for (index, wallet) in wallets.iter().enumerate() {
            let outpoint = OutPoint {
                txid: format!("{:064x}", index + 1),
                index: 0,
            };
            let root = UtxoLineageRoot {
                outpoint: outpoint.clone(),
                height: 0,
            };
            ledger.lineage_values.insert(root.clone(), 10);
            ledger.lineage_owners.insert(
                root,
                BTreeMap::from([(
                    wallet.address().to_string(),
                    BTreeMap::from([(outpoint, 10)]),
                )]),
            );
        }
        let committee_size = ledger.burn_committee_for_next_ticket_block(0).len();
        assert!(
            ledger.required_explicit_burn_signatures(
                ledger.height() + 1,
                FinalizerMode::Ticket,
                0,
                committee_size,
            ) > 0
        );

        let error = ledger
            .validate_next_block_burn_bundles_for_finalizer_rank(0, Vec::new())
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("not enough burn bundle signatures collected")
        );
    }

    #[test]
    fn only_post_activation_rank_zero_quorum_certifies_its_parent() {
        let ledger = ledger();
        let mut block = ledger.tip().clone();
        block.height = 1_001;
        block.finalizer_mode = FinalizerMode::Ticket;
        block.finalizer_rank = 0;
        block.burn_bundle_section.signatures = (1..=3)
            .map(|slot| BurnBundleSignature {
                slot,
                member: format!("member-{slot}"),
                reward_address: None,
                signature: format!("signature-{slot}"),
            })
            .collect();

        assert!(ledger.block_certifies_parent(&block, 5));

        block.height = OBJECTIVE_FINALITY_ACTIVATION_HEIGHT;
        assert!(!ledger.block_certifies_parent(&block, 5));
        block.height += 1;
        block.burn_bundle_section.signatures.pop();
        assert!(!ledger.block_certifies_parent(&block, 5));
        block
            .burn_bundle_section
            .signatures
            .push(BurnBundleSignature {
                slot: 3,
                member: "member-3".to_string(),
                reward_address: None,
                signature: "signature-3".to_string(),
            });
        block.finalizer_rank = 1;
        assert!(!ledger.block_certifies_parent(&block, 5));
    }

    #[test]
    fn burn_bundle_section_deduplicates_burns_and_tracks_member_masks() {
        let ledger = ledger();
        let high_fee_burn = burn("a", 10);
        let low_fee_burn = burn("b", 1);
        let bundles = vec![
            BurnBundle {
                height: 1,
                prev_hash: "parent".to_string(),
                slot: 1,
                member: "member-1".to_string(),
                reward_address: None,
                burns: vec![low_fee_burn.clone(), high_fee_burn.clone()],
                burns_v2: Vec::new(),
                signature: "sig-1".to_string(),
            },
            BurnBundle {
                height: 1,
                prev_hash: "parent".to_string(),
                slot: 2,
                member: "member-2".to_string(),
                reward_address: None,
                burns: vec![high_fee_burn.clone()],
                burns_v2: Vec::new(),
                signature: "sig-2".to_string(),
            },
        ];

        let section = ledger.burn_bundle_section_from_bundles(bundles).unwrap();

        assert_eq!(section.signatures.len(), 2);
        assert_eq!(section.burns.len(), 2);
        assert_eq!(section.burns[0].burn.signature(), high_fee_burn.signature());
        assert_eq!(
            section.burns[0].bundle_mask,
            burn_bundle_slot_mask(1).unwrap() | burn_bundle_slot_mask(2).unwrap()
        );
        assert_eq!(section.burns[1].burn.signature(), low_fee_burn.signature());
        assert_eq!(
            section.burns[1].bundle_mask,
            burn_bundle_slot_mask(1).unwrap()
        );

        let expanded = section.expand(1, "parent");
        assert_eq!(expanded[0].burns, vec![high_fee_burn.clone(), low_fee_burn]);
        assert_eq!(expanded[1].burns, vec![high_fee_burn]);
    }

    #[test]
    fn burn_bundle_rejects_different_burn_with_same_signature() {
        let alice = Wallet::from_seed("bundle-match-alice");
        let bob = Wallet::from_seed("bundle-match-bob");
        let mut ledger = funded_ledger(&[alice.clone(), bob.clone()]);
        let pending_burn = ledger.build_burn(&bob, 1, 1).unwrap();
        ledger.submit_transaction(pending_burn.clone()).unwrap();
        let member = ledger
            .burn_committee_for_next_block()
            .into_iter()
            .find(|member| member.owner == alice.address())
            .expect("alice should be in the burn committee");
        let mut attested_burn = pending_burn.clone();
        if let Transaction::Burn { amount, .. } = &mut attested_burn {
            *amount += 1;
        }
        let bundle = alice.burn_bundle(BurnBundlePayload {
            height: ledger.height() + 1,
            prev_hash: ledger.tip_hash().to_string(),
            slot: member.slot,
            member: alice.address().to_string(),
            reward_address: None,
            burns: vec![attested_burn],
            burns_v2: Vec::new(),
        });

        let error = ledger
            .validate_next_block_burn_bundles(vec![bundle])
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("burn bundle references a burn that is not in the mempool")
        );
    }

    #[test]
    fn burn_bundle_rejects_a_burn_queued_for_the_following_height() {
        let member = Wallet::from_seed("future-burn-bundle-member");
        let mut ledger = funded_ledger(std::slice::from_ref(&member));
        let tip = ledger.chain.last_mut().unwrap();
        tip.height = super::super::TIP_BOUND_BURN_ACTIVATION_HEIGHT - 1;
        tip.prev_hash = "a".repeat(64);
        tip.hash = "b".repeat(64);
        let future_burn = ledger.build_burn(&member, 1, 1).unwrap();
        let bundle = member.burn_bundle(BurnBundlePayload {
            height: ledger.height() + 1,
            prev_hash: ledger.tip_hash().to_string(),
            slot: 1,
            member: member.address().to_string(),
            reward_address: None,
            burns: vec![future_burn],
            burns_v2: Vec::new(),
        });
        let committee = BTreeMap::from([(
            1,
            BurnCommitteeMember {
                slot: 1,
                root: "c".repeat(64),
                owner: member.address().to_string(),
                weight: 1,
            },
        )]);

        let error = ledger
            .precheck_burn_bundle_for_block(
                ledger.height() + 1,
                ledger.tip_hash(),
                &committee,
                &bundle,
            )
            .unwrap_err();

        assert!(error.to_string().contains("block grandparent"));
    }

    #[test]
    fn invalid_committee_bundle_signature_is_rejected() {
        let alice = Wallet::from_seed("bundle-signature-alice");
        let bob = Wallet::from_seed("bundle-signature-bob");
        let ledger = funded_ledger(&[alice.clone(), bob.clone()]);
        let finalizer_address = ledger.expected_leader_for_next_block().unwrap();
        let finalizer = [&alice, &bob]
            .into_iter()
            .find(|wallet| wallet.address() == finalizer_address)
            .unwrap();
        let mut bundle = ledger.test_burn_bundle(finalizer, Vec::new());
        let replacement = if bundle.signature.starts_with('0') {
            "1"
        } else {
            "0"
        };
        bundle.signature.replace_range(0..1, replacement);

        assert!(
            ledger
                .validate_next_block_burn_bundles(vec![bundle])
                .unwrap_err()
                .to_string()
                .contains("signature")
        );
    }

    #[test]
    fn block_bundle_validation_rejects_duplicate_slots() {
        let alice = Wallet::from_seed("duplicate-slot-alice");
        let bob = Wallet::from_seed("duplicate-slot-bob");
        let ledger = funded_ledger(&[alice.clone(), bob.clone()]);
        let finalizer_address = ledger.expected_leader_for_next_block().unwrap();
        let finalizer = [&alice, &bob]
            .into_iter()
            .find(|wallet| wallet.address() == finalizer_address)
            .unwrap();
        let bundle = ledger.test_burn_bundle(finalizer, Vec::new());

        assert!(
            ledger
                .validate_next_block_burn_bundles(vec![bundle.clone(), bundle])
                .unwrap_err()
                .to_string()
                .contains("duplicate burn bundle slot")
        );
    }

    #[test]
    fn attested_burn_omitted_from_block_is_rejected_without_consulting_mempool_policy() {
        let alice = Wallet::from_seed("required-burn-alice");
        let bob = Wallet::from_seed("required-burn-bob");
        let mut ledger = funded_ledger(&[alice.clone(), bob.clone()]);
        let finalizer_address = ledger.expected_leader_for_next_block().unwrap();
        let finalizer = [&alice, &bob]
            .into_iter()
            .find(|wallet| wallet.address() == finalizer_address)
            .unwrap();
        let other = [&alice, &bob]
            .into_iter()
            .find(|wallet| wallet.address() != finalizer_address)
            .unwrap();
        let pending_burn = ledger.build_burn(other, 1, 1).unwrap();
        ledger.submit_transaction(pending_burn.clone()).unwrap();
        let anchor = ledger.build_burn(finalizer, 1, 1).unwrap();
        ledger.submit_transaction(anchor).unwrap();
        let mut block = ledger
            .prepare_next_block(finalizer.address(), 1)
            .unwrap()
            .finish(finalizer, "unused-vdf".to_string());
        let bundle = ledger.test_burn_bundle(finalizer, vec![pending_burn.clone()]);
        block.burn_bundle_section = ledger
            .burn_bundle_section_from_bundles(vec![bundle])
            .unwrap();
        block
            .transactions
            .retain(|transaction| transaction.signature() != pending_burn.signature());

        assert!(
            ledger
                .validate_burn_bundle_section_for_block(&block)
                .unwrap_err()
                .to_string()
                .contains("attested burn is not included")
        );
    }
}
