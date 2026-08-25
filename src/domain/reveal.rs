use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::{Amount, BURN_COMMITTEE_SIZE, Transaction, hex_hash};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BurnBundle {
    pub height: u64,
    pub prev_hash: String,
    pub slot: u8,
    pub member: String,
    pub burns: Vec<Transaction>,
    pub signature: String,
}

impl BurnBundle {
    pub fn canonical_payload(&self) -> String {
        BurnBundlePayload {
            height: self.height,
            prev_hash: self.prev_hash.clone(),
            slot: self.slot,
            member: self.member.clone(),
            burns: self.burns.clone(),
        }
        .canonical()
    }

    pub fn canonical(&self) -> String {
        format!("{}:{}", self.canonical_payload(), self.signature)
    }

    pub fn bundle_hash(&self) -> String {
        hex_hash(self.canonical())
    }

    pub fn serialized_size_bytes(&self) -> Result<usize> {
        serde_json::to_vec(self)
            .map(|bytes| bytes.len())
            .context("failed to serialize burn bundle for size check")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BurnBundleSignature {
    pub slot: u8,
    pub member: String,
    pub signature: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskedBurn {
    pub burn: Transaction,
    pub bundle_mask: u8,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BurnBundleSection {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<BurnBundleSignature>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub burns: Vec<MaskedBurn>,
}

impl BurnBundleSection {
    pub fn is_empty(&self) -> bool {
        self.signatures.is_empty() && self.burns.is_empty()
    }

    pub fn required_burns(&self) -> Vec<&Transaction> {
        self.burns.iter().map(|masked| &masked.burn).collect()
    }

    pub fn included_bundle_count(&self) -> usize {
        self.signatures.len()
    }

    pub fn expand(&self, height: u64, prev_hash: &str) -> Vec<BurnBundle> {
        self.signatures
            .iter()
            .map(|signature| {
                let slot_mask = burn_bundle_slot_mask(signature.slot).unwrap_or(0);
                let burns = self
                    .burns
                    .iter()
                    .filter(|masked| masked.bundle_mask & slot_mask != 0)
                    .map(|masked| masked.burn.clone())
                    .collect();
                BurnBundle {
                    height,
                    prev_hash: prev_hash.to_string(),
                    slot: signature.slot,
                    member: signature.member.clone(),
                    burns,
                    signature: signature.signature.clone(),
                }
            })
            .collect()
    }

    pub fn burn_bundle_hashes(
        &self,
        height: u64,
        prev_hash: &str,
        finalizer: &str,
    ) -> [String; BURN_COMMITTEE_SIZE] {
        let bundles = self.expand(height, prev_hash);
        let mut hashes = burn_bundle_hashes(&bundles);
        hashes[0] = finalizer_attestation_hash(height, prev_hash, finalizer, &self.burns);
        hashes
    }

    pub(super) fn canonical(&self) -> String {
        let signatures = self
            .signatures
            .iter()
            .map(|signature| {
                format!(
                    "{}:{}:{}",
                    signature.slot, signature.member, signature.signature
                )
            })
            .collect::<Vec<_>>()
            .join("|");
        let burns = self
            .burns
            .iter()
            .map(|masked| format!("{}:{}", masked.bundle_mask, masked.burn.canonical()))
            .collect::<Vec<_>>()
            .join("|");
        format!("burn-bundle-section-v1:{signatures}:burns:{burns}")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BurnBundlePayload {
    pub height: u64,
    pub prev_hash: String,
    pub slot: u8,
    pub member: String,
    pub burns: Vec<Transaction>,
}

impl BurnBundlePayload {
    pub fn canonical(&self) -> String {
        let burns = self
            .burns
            .iter()
            .map(Transaction::canonical)
            .collect::<Vec<_>>()
            .join("|");
        format!(
            "iuna-burn-bundle-v1:{}:{}:{}:{}:{}",
            self.height, self.prev_hash, self.slot, self.member, burns
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BurnCommitteeMember {
    pub slot: u8,
    pub root: String,
    pub owner: String,
    pub weight: Amount,
}

pub fn default_burn_bundle_hash(slot: usize) -> String {
    hex_hash(format!("iuna-default-burn-bundle-v1:{slot}"))
}

pub(super) fn burn_bundle_slot_mask(slot: u8) -> Result<u8> {
    if usize::from(slot) >= BURN_COMMITTEE_SIZE || slot >= 8 {
        bail!("burn bundle slot is invalid");
    }
    Ok(1_u8 << slot)
}

pub(super) fn burn_committee_mask() -> u8 {
    (0..BURN_COMMITTEE_SIZE).fold(0_u8, |mask, slot| mask | (1_u8 << slot))
}

pub(super) fn burn_bundle_hashes(bundles: &[BurnBundle]) -> [String; BURN_COMMITTEE_SIZE] {
    std::array::from_fn(|slot| {
        bundles
            .iter()
            .find(|bundle| usize::from(bundle.slot) == slot)
            .map(BurnBundle::bundle_hash)
            .unwrap_or_else(|| default_burn_bundle_hash(slot))
    })
}

pub(super) fn finalizer_attestation_hash(
    height: u64,
    prev_hash: &str,
    finalizer: &str,
    burns: &[MaskedBurn],
) -> String {
    let canonical_burns = burns
        .iter()
        .map(|masked| masked.burn.canonical())
        .collect::<Vec<_>>()
        .join("|");
    hex_hash(format!(
        "iuna-finalizer-burn-attestation-v1:{height}:{prev_hash}:{finalizer}:{canonical_burns}"
    ))
}

pub(super) fn canonical_burn_bundle_hashes(
    bundle_hashes: &[String; BURN_COMMITTEE_SIZE],
) -> String {
    bundle_hashes.join("|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burn_bundle_hash_array_covers_slots_zero_through_four() {
        let slot_four = BurnBundle {
            height: 1,
            prev_hash: "parent".to_string(),
            slot: 4,
            member: "member".to_string(),
            burns: Vec::new(),
            signature: "signature".to_string(),
        };

        let hashes = burn_bundle_hashes(std::slice::from_ref(&slot_four));

        assert_eq!(hashes.len(), 5);
        for (slot, hash) in hashes.iter().enumerate().take(4) {
            assert_eq!(hash, &default_burn_bundle_hash(slot));
        }
        assert_eq!(hashes[4], slot_four.bundle_hash());
    }
}
