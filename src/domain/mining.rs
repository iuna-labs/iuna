use anyhow::Result;

use super::{
    MineSearchOutcome, Transaction, TransactionSigningDomain, hash_meets_difficulty, hex_hash,
    mine_signing_bytes,
};

#[derive(Clone, Debug)]
pub struct MineSearchWork {
    recipient: String,
    anchor: String,
    salt: u64,
    start_nonce: u64,
    max_attempts: u64,
    difficulty_bits: u32,
    signing_domain: TransactionSigningDomain,
}

impl MineSearchWork {
    pub fn anchor(&self) -> &str {
        &self.anchor
    }

    pub fn search(&self) -> Result<MineSearchOutcome> {
        let mut attempts = 0_u64;
        let mut nonce = self.start_nonce;
        while attempts < self.max_attempts {
            let signature = mine_signature(
                &self.signing_domain,
                &self.recipient,
                &self.anchor,
                self.salt,
                nonce,
                self.difficulty_bits,
            )?;
            attempts = attempts.saturating_add(1);
            let next_nonce = nonce.checked_add(1).unwrap_or(0);
            if hash_meets_difficulty(&signature, self.difficulty_bits) {
                return Ok(MineSearchOutcome {
                    transaction: Some(Transaction::Mine {
                        recipient: self.recipient.clone(),
                        anchor: self.anchor.clone(),
                        salt: self.salt,
                        nonce,
                        difficulty_bits: self.difficulty_bits,
                        proof_header: None,
                        signature,
                    }),
                    next_nonce,
                    attempts,
                });
            }
            nonce = next_nonce;
        }
        Ok(MineSearchOutcome {
            transaction: None,
            next_nonce: nonce,
            attempts,
        })
    }
}

pub(super) fn mine_search_work(
    recipient: String,
    anchor: String,
    salt: u64,
    start_nonce: u64,
    max_attempts: u64,
    difficulty_bits: u32,
    signing_domain: TransactionSigningDomain,
) -> MineSearchWork {
    MineSearchWork {
        recipient,
        anchor,
        salt,
        start_nonce,
        max_attempts,
        difficulty_bits,
        signing_domain,
    }
}

pub(super) fn mine_payload(
    recipient: &str,
    anchor: &str,
    salt: u64,
    nonce: u64,
    difficulty_bits: u32,
) -> String {
    format!("iuna-mine:{recipient}:{anchor}:{salt}:{nonce}:{difficulty_bits}")
}

pub(super) fn mine_signature(
    domain: &TransactionSigningDomain,
    recipient: &str,
    anchor: &str,
    salt: u64,
    nonce: u64,
    difficulty_bits: u32,
) -> Result<String> {
    if domain.is_chain_bound() {
        Ok(hex_hash(mine_signing_bytes(
            domain,
            recipient,
            anchor,
            salt,
            nonce,
            difficulty_bits,
        )?))
    } else {
        Ok(hex_hash(mine_payload(
            recipient,
            anchor,
            salt,
            nonce,
            difficulty_bits,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::{mine_payload, mine_signature};
    use crate::domain::{TransactionSigningDomain, Wallet};

    #[test]
    fn mine_signature_commits_binary_chain_domain() {
        let payload = mine_payload("recipient", "anchor", 1, 2, 12);

        assert_eq!(payload, "iuna-mine:recipient:anchor:1:2:12");
        let wallet = Wallet::from_seed("mine-signature-recipient");
        let domain = TransactionSigningDomain::new("test-chain", "0".repeat(64));
        assert_ne!(
            mine_signature(&domain, wallet.address(), &"1".repeat(64), 1, 2, 12).unwrap(),
            mine_signature(
                &TransactionSigningDomain::new("other-chain", "0".repeat(64)),
                wallet.address(),
                &"1".repeat(64),
                1,
                2,
                12,
            )
            .unwrap()
        );
    }

    #[test]
    fn legacy_mine_signature_keeps_the_original_payload_hash() {
        assert_eq!(
            mine_signature(
                &TransactionSigningDomain::legacy(),
                "recipient",
                "anchor",
                1,
                2,
                12,
            )
            .unwrap(),
            "47f9ad353685fdb9b4932cefa9dd1d27f8af70e27eaedf15c4f9ffbbb64300a3"
        );
    }
}
