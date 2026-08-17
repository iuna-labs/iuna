use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};

use super::ledger_ops::verify_address_signature;
use super::reveal::{reveal_bundle_slot_mask, reveal_committee_mask};
use super::{
    Amount, FinalizerMode, Ledger, MAX_REVEAL_BUNDLE_BYTES, MaskedBlindedReveal,
    REVEAL_BUNDLE_SIGNATURE_THRESHOLDS_HEIGHT, REVEAL_COMMITTEE_SIZE, RevealBundle,
    RevealBundlePayload, RevealBundleSection, RevealBundleSignature, Wallet,
};

impl Ledger {
    pub fn reveal_bundle_attestations_required_for_next_block(&self) -> bool {
        self.tip().height.saturating_add(1) >= REVEAL_BUNDLE_SIGNATURE_THRESHOLDS_HEIGHT
            && !self.active_blinded.is_empty()
    }

    pub fn build_reveal_bundle(&self, wallet: &Wallet) -> Result<Option<RevealBundle>> {
        let height = self.tip().height + 1;
        let prev_hash = self.tip().hash.clone();
        let Some(member) = self
            .reveal_committee_for_next_block()
            .into_iter()
            .find(|member| member.owner == wallet.address())
        else {
            return Ok(None);
        };
        let mut reveals = self.valid_pending_blinded_reveals();
        reveals.sort_by(|left, right| {
            self.reveal_fee_order_key(right)
                .cmp(&self.reveal_fee_order_key(left))
                .then_with(|| left.commitment.cmp(&right.commitment))
        });

        let mut selected = Vec::new();
        for reveal in reveals {
            let mut candidate = selected.clone();
            candidate.push(reveal);
            let bundle = wallet.reveal_bundle(RevealBundlePayload {
                height,
                prev_hash: prev_hash.clone(),
                slot: member.slot,
                member: wallet.address().to_string(),
                reveals: candidate.clone(),
            });
            if bundle.serialized_size_bytes()? <= MAX_REVEAL_BUNDLE_BYTES {
                selected = candidate;
            }
        }
        if selected.is_empty() && !self.reveal_bundle_attestations_required_for_next_block() {
            return Ok(None);
        }
        Ok(Some(wallet.reveal_bundle(RevealBundlePayload {
            height,
            prev_hash,
            slot: member.slot,
            member: wallet.address().to_string(),
            reveals: selected,
        })))
    }

    pub fn validate_next_block_reveal_bundles(
        &self,
        bundles: Vec<RevealBundle>,
    ) -> Result<Vec<RevealBundle>> {
        let expected_height = self.tip().height + 1;
        let expected_prev_hash = self.tip().hash.clone();
        self.validate_reveal_bundles_for_block(expected_height, &expected_prev_hash, bundles)
    }

    pub(super) fn reveal_bundle_section_from_bundles(
        &self,
        bundles: Vec<RevealBundle>,
    ) -> RevealBundleSection {
        let signatures = bundles
            .iter()
            .map(|bundle| RevealBundleSignature {
                slot: bundle.slot,
                member: bundle.member.clone(),
                signature: bundle.signature.clone(),
            })
            .collect::<Vec<_>>();
        let mut by_commitment: BTreeMap<String, MaskedBlindedReveal> = BTreeMap::new();
        for bundle in bundles {
            let slot_mask = reveal_bundle_slot_mask(bundle.slot).unwrap_or(0);
            for reveal in bundle.reveals {
                by_commitment
                    .entry(reveal.commitment.clone())
                    .and_modify(|masked| masked.bundle_mask |= slot_mask)
                    .or_insert(MaskedBlindedReveal {
                        reveal,
                        bundle_mask: slot_mask,
                    });
            }
        }
        let mut reveals = by_commitment.into_values().collect::<Vec<_>>();
        reveals.sort_by(|left, right| {
            self.reveal_fee_order_key(&right.reveal)
                .cmp(&self.reveal_fee_order_key(&left.reveal))
                .then_with(|| left.reveal.commitment.cmp(&right.reveal.commitment))
        });
        RevealBundleSection {
            signatures,
            reveals,
        }
    }

    pub(super) fn validate_reveal_bundle_section_for_block(
        &self,
        expected_height: u64,
        expected_prev_hash: &str,
        finalizer_mode: FinalizerMode,
        finalizer_rank: u32,
        section: &RevealBundleSection,
    ) -> Result<()> {
        if section.signatures.len() > REVEAL_COMMITTEE_SIZE {
            bail!("block has too many reveal bundle signatures");
        }
        if section
            .signatures
            .windows(2)
            .any(|pair| pair[0].slot >= pair[1].slot)
        {
            bail!("reveal bundle signatures are not in slot order");
        }
        let committee = self
            .reveal_committee_for_height(expected_height)
            .into_iter()
            .map(|member| (member.slot, member))
            .collect::<BTreeMap<_, _>>();
        let mut seen_slots = BTreeSet::new();
        let mut seen_members = BTreeSet::new();
        let mut included_mask = 0_u8;
        for signature in &section.signatures {
            if usize::from(signature.slot) >= REVEAL_COMMITTEE_SIZE {
                bail!("reveal bundle slot is invalid");
            }
            if !seen_slots.insert(signature.slot) {
                bail!("duplicate reveal bundle slot");
            }
            if !seen_members.insert(signature.member.clone()) {
                bail!("duplicate reveal bundle member");
            }
            let member = committee
                .get(&signature.slot)
                .context("reveal bundle slot is not assigned")?;
            if signature.member != member.owner {
                bail!("reveal bundle member is not assigned to slot");
            }
            included_mask |= reveal_bundle_slot_mask(signature.slot)?;
        }
        let required_signatures = self.required_reveal_bundle_signatures(
            expected_height,
            finalizer_mode,
            finalizer_rank,
            committee.len(),
        );
        if section.signatures.len() < required_signatures {
            bail!(
                "block has too few reveal bundle signatures: got {}, need {required_signatures}",
                section.signatures.len()
            );
        }

        let mut seen_reveals = BTreeSet::new();
        let mut previous_key: Option<((u128, Amount), String)> = None;
        for masked in &section.reveals {
            if masked.bundle_mask == 0 {
                bail!("masked blinded reveal is not assigned to a reveal bundle");
            }
            if masked.bundle_mask & !reveal_committee_mask() != 0 {
                bail!("masked blinded reveal references an invalid reveal bundle slot");
            }
            if masked.bundle_mask & !included_mask != 0 {
                bail!("masked blinded reveal references a missing reveal bundle signature");
            }
            if !seen_reveals.insert(masked.reveal.commitment.clone()) {
                bail!("duplicate blinded reveal in reveal bundle section");
            }
            self.pending_reveal_transaction(&masked.reveal)?;
            let key = (
                self.reveal_fee_order_key(&masked.reveal),
                masked.reveal.commitment.clone(),
            );
            if let Some((previous_fee_key, previous_commitment)) = &previous_key {
                if key.0 > *previous_fee_key
                    || key.0 == *previous_fee_key && key.1 < *previous_commitment
                {
                    bail!("reveal bundle section is not fee ordered");
                }
            }
            previous_key = Some(key);
        }

        for bundle in section.expand(expected_height, expected_prev_hash) {
            if bundle.serialized_size_bytes()? > MAX_REVEAL_BUNDLE_BYTES {
                bail!("reveal bundle exceeds max size");
            }
            verify_address_signature(
                &bundle.member,
                &bundle.canonical_payload(),
                &bundle.signature,
                "reveal bundle",
            )?;
        }
        Ok(())
    }

    fn required_reveal_bundle_signatures(
        &self,
        height: u64,
        finalizer_mode: FinalizerMode,
        finalizer_rank: u32,
        committee_size: usize,
    ) -> usize {
        if height < REVEAL_BUNDLE_SIGNATURE_THRESHOLDS_HEIGHT
            || self.active_blinded.is_empty()
            || committee_size == 0
        {
            return 0;
        }
        match finalizer_mode {
            FinalizerMode::Ticket if finalizer_rank == 0 => committee_size,
            FinalizerMode::Ticket if finalizer_rank == 1 => committee_size.min(2),
            FinalizerMode::Ticket => 1,
            FinalizerMode::Recovery => 0,
        }
    }

    fn validate_reveal_bundles_for_block(
        &self,
        expected_height: u64,
        expected_prev_hash: &str,
        mut bundles: Vec<RevealBundle>,
    ) -> Result<Vec<RevealBundle>> {
        if bundles.len() > REVEAL_COMMITTEE_SIZE {
            bail!("block has too many reveal bundles");
        }
        if bundles.windows(2).any(|pair| pair[0].slot >= pair[1].slot) {
            bail!("reveal bundles are not in slot order");
        }
        bundles.sort_by_key(|bundle| bundle.slot);
        let committee = self
            .reveal_committee_for_height(expected_height)
            .into_iter()
            .map(|member| (member.slot, member))
            .collect::<BTreeMap<_, _>>();
        let mut seen_slots = BTreeSet::new();
        let mut seen_members = BTreeSet::new();
        for bundle in &bundles {
            if bundle.height != expected_height {
                bail!("reveal bundle height is invalid");
            }
            if bundle.prev_hash != expected_prev_hash {
                bail!("reveal bundle parent hash is invalid");
            }
            if usize::from(bundle.slot) >= REVEAL_COMMITTEE_SIZE {
                bail!("reveal bundle slot is invalid");
            }
            if !seen_slots.insert(bundle.slot) {
                bail!("duplicate reveal bundle slot");
            }
            if !seen_members.insert(bundle.member.clone()) {
                bail!("duplicate reveal bundle member");
            }
            let member = committee
                .get(&bundle.slot)
                .context("reveal bundle slot is not assigned")?;
            if bundle.member != member.owner {
                bail!("reveal bundle member is not assigned to slot");
            }
            if bundle.serialized_size_bytes()? > MAX_REVEAL_BUNDLE_BYTES {
                bail!("reveal bundle exceeds max size");
            }
            verify_address_signature(
                &bundle.member,
                &bundle.canonical_payload(),
                &bundle.signature,
                "reveal bundle",
            )?;
            let mut seen_bundle_reveals = BTreeSet::new();
            let mut previous_key: Option<((u128, Amount), String)> = None;
            for reveal in &bundle.reveals {
                if !seen_bundle_reveals.insert(reveal.commitment.clone()) {
                    bail!("duplicate blinded reveal in reveal bundle");
                }
                self.pending_reveal_transaction(reveal)?;
                let key = (self.reveal_fee_order_key(reveal), reveal.commitment.clone());
                if let Some((previous_fee_key, previous_commitment)) = &previous_key {
                    if key.0 > *previous_fee_key
                        || key.0 == *previous_fee_key && key.1 < *previous_commitment
                    {
                        bail!("reveal bundle is not fee ordered");
                    }
                }
                previous_key = Some(key);
            }
        }
        Ok(bundles)
    }
}
