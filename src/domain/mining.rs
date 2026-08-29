use anyhow::Result;

use super::{TransactionSigningDomain, hex_hash, mine_signing_bytes};

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
    Ok(hex_hash(mine_signing_bytes(
        domain,
        recipient,
        anchor,
        salt,
        nonce,
        difficulty_bits,
    )?))
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
}
