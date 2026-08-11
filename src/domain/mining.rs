use super::hex_hash;

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
    recipient: &str,
    anchor: &str,
    salt: u64,
    nonce: u64,
    difficulty_bits: u32,
) -> String {
    hex_hash(mine_payload(
        recipient,
        anchor,
        salt,
        nonce,
        difficulty_bits,
    ))
}

#[cfg(test)]
mod tests {
    use super::{mine_payload, mine_signature};

    #[test]
    fn mine_signature_hashes_canonical_mine_payload() {
        let payload = mine_payload("recipient", "anchor", 1, 2, 12);

        assert_eq!(payload, "iuna-mine:recipient:anchor:1:2:12");
        assert_eq!(
            mine_signature("recipient", "anchor", 1, 2, 12),
            "47f9ad353685fdb9b4932cefa9dd1d27f8af70e27eaedf15c4f9ffbbb64300a3"
        );
    }
}
