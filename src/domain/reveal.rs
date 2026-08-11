use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::{Amount, BlindedReveal, REVEAL_COMMITTEE_SIZE, hex_hash};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevealBundle {
    pub height: u64,
    pub prev_hash: String,
    pub slot: u8,
    pub member: String,
    pub reveals: Vec<BlindedReveal>,
    pub signature: String,
}

impl RevealBundle {
    pub fn canonical_payload(&self) -> String {
        RevealBundlePayload {
            height: self.height,
            prev_hash: self.prev_hash.clone(),
            slot: self.slot,
            member: self.member.clone(),
            reveals: self.reveals.clone(),
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
            .context("failed to serialize reveal bundle for size check")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevealBundleSignature {
    pub slot: u8,
    pub member: String,
    pub signature: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskedBlindedReveal {
    pub reveal: BlindedReveal,
    pub bundle_mask: u8,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevealBundleSection {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<RevealBundleSignature>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reveals: Vec<MaskedBlindedReveal>,
}

impl RevealBundleSection {
    pub fn is_empty(&self) -> bool {
        self.signatures.is_empty() && self.reveals.is_empty()
    }

    pub fn all_reveals(&self) -> Vec<&BlindedReveal> {
        self.reveals.iter().map(|masked| &masked.reveal).collect()
    }

    pub fn included_bundle_count(&self) -> usize {
        self.signatures.len()
    }

    pub fn expand(&self, height: u64, prev_hash: &str) -> Vec<RevealBundle> {
        self.signatures
            .iter()
            .map(|signature| {
                let slot_mask = reveal_bundle_slot_mask(signature.slot).unwrap_or(0);
                let reveals = self
                    .reveals
                    .iter()
                    .filter(|masked| masked.bundle_mask & slot_mask != 0)
                    .map(|masked| masked.reveal.clone())
                    .collect();
                RevealBundle {
                    height,
                    prev_hash: prev_hash.to_string(),
                    slot: signature.slot,
                    member: signature.member.clone(),
                    reveals,
                    signature: signature.signature.clone(),
                }
            })
            .collect()
    }

    pub fn reveal_bundle_hashes(
        &self,
        height: u64,
        prev_hash: &str,
    ) -> [String; REVEAL_COMMITTEE_SIZE] {
        let bundles = self.expand(height, prev_hash);
        reveal_bundle_hashes(&bundles)
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
        let reveals = self
            .reveals
            .iter()
            .map(|masked| format!("{}:{}", masked.bundle_mask, masked.reveal.canonical()))
            .collect::<Vec<_>>()
            .join("|");
        format!("reveal-bundle-section-v1:{signatures}:reveals:{reveals}")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RevealBundlePayload {
    pub(super) height: u64,
    pub(super) prev_hash: String,
    pub(super) slot: u8,
    pub(super) member: String,
    pub(super) reveals: Vec<BlindedReveal>,
}

impl RevealBundlePayload {
    pub(super) fn canonical(&self) -> String {
        let reveals = self
            .reveals
            .iter()
            .map(BlindedReveal::canonical)
            .collect::<Vec<_>>()
            .join("|");
        format!(
            "iuna-reveal-bundle-v1:{}:{}:{}:{}:{}",
            self.height, self.prev_hash, self.slot, self.member, reveals
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevealCommitteeMember {
    pub slot: u8,
    pub rank: u32,
    pub ticket_id: String,
    pub owner: String,
    pub amount: Amount,
}

pub fn default_reveal_bundle_hash(slot: usize) -> String {
    hex_hash(format!("iuna-default-reveal-bundle-v1:{slot}"))
}

pub(super) fn reveal_bundle_slot_mask(slot: u8) -> Result<u8> {
    if usize::from(slot) >= REVEAL_COMMITTEE_SIZE || slot >= 8 {
        bail!("reveal bundle slot is invalid");
    }
    Ok(1_u8 << slot)
}

pub(super) fn reveal_committee_mask() -> u8 {
    (0..REVEAL_COMMITTEE_SIZE).fold(0_u8, |mask, slot| mask | (1_u8 << slot))
}

pub(super) fn reveal_bundle_hashes(bundles: &[RevealBundle]) -> [String; REVEAL_COMMITTEE_SIZE] {
    std::array::from_fn(|slot| {
        bundles
            .iter()
            .find(|bundle| usize::from(bundle.slot) == slot)
            .map(RevealBundle::bundle_hash)
            .unwrap_or_else(|| default_reveal_bundle_hash(slot))
    })
}

pub(super) fn canonical_reveal_bundle_hashes(
    bundle_hashes: &[String; REVEAL_COMMITTEE_SIZE],
) -> String {
    bundle_hashes.join("|")
}

#[cfg(test)]
mod tests {
    use super::{
        MaskedBlindedReveal, RevealBundle, RevealBundlePayload, RevealBundleSection,
        RevealBundleSignature, default_reveal_bundle_hash, reveal_bundle_hashes,
        reveal_bundle_slot_mask,
    };
    use crate::domain::BlindedReveal;

    #[test]
    fn reveal_bundle_payload_is_canonical() {
        let payload = RevealBundlePayload {
            height: 7,
            prev_hash: "prev".to_string(),
            slot: 1,
            member: "member".to_string(),
            reveals: vec![BlindedReveal {
                commitment: "commitment".to_string(),
                key: "key".to_string(),
            }],
        };

        assert_eq!(
            payload.canonical(),
            "iuna-reveal-bundle-v1:7:prev:1:member:blinded-reveal:commitment:key"
        );
    }

    #[test]
    fn reveal_bundle_hashes_fill_missing_slots_with_defaults() {
        let bundle = RevealBundle {
            height: 1,
            prev_hash: "prev".to_string(),
            slot: 1,
            member: "member".to_string(),
            reveals: Vec::new(),
            signature: "sig".to_string(),
        };

        let hashes = reveal_bundle_hashes(&[bundle.clone()]);

        assert_eq!(hashes[0], default_reveal_bundle_hash(0));
        assert_eq!(hashes[1], bundle.bundle_hash());
        assert_eq!(hashes[2], default_reveal_bundle_hash(2));
    }

    #[test]
    fn reveal_bundle_section_expands_masked_reveals_by_slot() {
        let reveal = BlindedReveal {
            commitment: "commitment".to_string(),
            key: "key".to_string(),
        };
        let section = RevealBundleSection {
            signatures: vec![
                RevealBundleSignature {
                    slot: 0,
                    member: "a".to_string(),
                    signature: "sig-a".to_string(),
                },
                RevealBundleSignature {
                    slot: 1,
                    member: "b".to_string(),
                    signature: "sig-b".to_string(),
                },
            ],
            reveals: vec![MaskedBlindedReveal {
                reveal: reveal.clone(),
                bundle_mask: reveal_bundle_slot_mask(1).unwrap(),
            }],
        };

        let expanded = section.expand(3, "prev");

        assert!(expanded[0].reveals.is_empty());
        assert_eq!(expanded[1].reveals, vec![reveal]);
    }
}
