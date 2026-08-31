use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};

use super::ledger_ops::verify_address_signature;
use super::reveal::{burn_bundle_slot_mask, burn_committee_mask};
use super::{
    Amount, BURN_COMMITTEE_SIZE, Block, BurnBundle, BurnBundlePayload, BurnBundleSection,
    BurnBundleSignature, BurnCommitteeMember, FinalizerMode, Ledger, MAX_BURN_BUNDLE_BYTES,
    MaskedBurn, OBJECTIVE_FINALITY_ACTIVATION_HEIGHT, Transaction, Wallet,
};

impl Ledger {
    pub fn burn_bundle_attestations_required_for_next_block(&self) -> bool {
        self.pending.iter().any(Transaction::is_burn)
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
        let mut burns = self
            .valid_pending_transactions()
            .into_iter()
            .filter(Transaction::is_burn)
            .collect::<Vec<_>>();
        burns.sort_by(|left, right| {
            right
                .fee()
                .cmp(&left.fee())
                .then_with(|| left.signature().cmp(right.signature()))
        });

        let mut bundles = Vec::new();
        for member in memberships {
            let mut selected = Vec::new();
            for burn in &burns {
                let mut candidate = selected.clone();
                candidate.push(burn.clone());
                let bundle = wallet.burn_bundle(BurnBundlePayload {
                    height,
                    prev_hash: prev_hash.clone(),
                    slot: member.slot,
                    member: wallet.address().to_string(),
                    burns: candidate.clone(),
                });
                if bundle.serialized_size_bytes()? <= MAX_BURN_BUNDLE_BYTES {
                    selected = candidate;
                }
            }
            bundles.push(wallet.burn_bundle(BurnBundlePayload {
                height,
                prev_hash: prev_hash.clone(),
                slot: member.slot,
                member: wallet.address().to_string(),
                burns: selected,
            }));
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
        self.validate_burn_bundles_for_committee(
            expected_height,
            &expected_prev_hash,
            &committee,
            bundles,
        )
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
            burns,
        })
    }

    pub(super) fn burn_bundle_section_from_bundles(
        &self,
        bundles: Vec<BurnBundle>,
    ) -> BurnBundleSection {
        let signatures = bundles
            .iter()
            .filter(|bundle| bundle.slot != 0)
            .map(|bundle| BurnBundleSignature {
                slot: bundle.slot,
                member: bundle.member.clone(),
                signature: bundle.signature.clone(),
            })
            .collect::<Vec<_>>();
        let mut by_signature: BTreeMap<String, MaskedBurn> = BTreeMap::new();
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
        }
        let mut burns = by_signature.into_values().collect::<Vec<_>>();
        burns.sort_by(|left, right| {
            right
                .burn
                .fee()
                .cmp(&left.burn.fee())
                .then_with(|| left.burn.signature().cmp(right.burn.signature()))
        });
        BurnBundleSection { signatures, burns }
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
            bail!("burn bundle parent hash is invalid");
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
            let key = (burn.fee(), burn.signature().to_string());
            if let Some((previous_fee, previous_signature)) = &previous_key {
                if key.0 > *previous_fee || key.0 == *previous_fee && key.1 < *previous_signature {
                    bail!("burn bundle is not fee ordered");
                }
            }
            previous_key = Some(key);
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
    use crate::domain::{GenesisBurn, MICRO_IUNA, OutPoint, TxInput, TxOutput, Wallet};

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
                burns: vec![low_fee_burn.clone(), high_fee_burn.clone()],
                signature: "sig-1".to_string(),
            },
            BurnBundle {
                height: 1,
                prev_hash: "parent".to_string(),
                slot: 2,
                member: "member-2".to_string(),
                burns: vec![high_fee_burn.clone()],
                signature: "sig-2".to_string(),
            },
        ];

        let section = ledger.burn_bundle_section_from_bundles(bundles);

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
            burns: vec![attested_burn],
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
        block.burn_bundle_section = ledger.burn_bundle_section_from_bundles(vec![bundle]);
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
