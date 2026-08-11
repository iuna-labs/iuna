use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::{
    HASH_BYTES, decode_hex_array, hex_encode,
    validation::{validate_address, validate_hash},
};

pub const STRATUM_EXTRANONCE1_HEX: &str = "00000000";
pub const STRATUM_EXTRANONCE2_SIZE: usize = 4;
pub(super) const STRATUM_MINE_HEADER_BYTES: usize = 80;

const STRATUM_MINE_VERSION: [u8; 4] = [1, 0, 0, 0];
const STRATUM_MINE_NTIME: [u8; 4] = [0, 0, 0, 0];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StratumMineTemplate {
    pub recipient: String,
    pub anchor: String,
    pub salt: u64,
    pub difficulty_bits: u32,
    pub coinbase_prefix: Vec<u8>,
    pub version_hex: String,
    pub prev_hash_hex: String,
    pub nbits_hex: String,
    pub ntime_hex: String,
}

impl StratumMineTemplate {
    pub fn coinb1_hex(&self) -> String {
        hex_encode(&self.coinbase_prefix)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StratumMineShare {
    pub extranonce2: [u8; 4],
    pub header_nonce: [u8; 4],
}

pub fn pack_stratum_nonce(extranonce2: [u8; 4], header_nonce: [u8; 4]) -> u64 {
    let extra = u32::from_be_bytes(extranonce2) as u64;
    let nonce = u32::from_le_bytes(header_nonce) as u64;
    (extra << 32) | nonce
}

fn unpack_stratum_nonce(nonce: u64) -> ([u8; 4], [u8; 4]) {
    (
        ((nonce >> 32) as u32).to_be_bytes(),
        (nonce as u32).to_le_bytes(),
    )
}

fn stratum_coinbase_prefix(
    recipient: &str,
    anchor: &str,
    salt: u64,
    difficulty_bits: u32,
) -> Vec<u8> {
    format!("iuna-stratum-mine:{recipient}:{anchor}:{salt}:{difficulty_bits}:").into_bytes()
}

fn stratum_coinbase_bytes(
    recipient: &str,
    anchor: &str,
    salt: u64,
    nonce: u64,
    difficulty_bits: u32,
) -> Vec<u8> {
    let (extranonce2, _) = unpack_stratum_nonce(nonce);
    let mut coinbase = stratum_coinbase_prefix(recipient, anchor, salt, difficulty_bits);
    coinbase.extend_from_slice(&[0, 0, 0, 0]);
    coinbase.extend_from_slice(&extranonce2);
    coinbase
}

fn double_sha256(bytes: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(bytes);
    let second = Sha256::digest(first);
    second.into()
}

pub(super) fn stratum_mine_header_bytes(
    recipient: &str,
    anchor: &str,
    salt: u64,
    nonce: u64,
    difficulty_bits: u32,
) -> Result<[u8; 80]> {
    let mut header = [0_u8; STRATUM_MINE_HEADER_BYTES];
    header[0..4].copy_from_slice(&STRATUM_MINE_VERSION);
    let anchor_bytes =
        decode_hex_array::<HASH_BYTES>(anchor).context("mine transaction anchor is not hex")?;
    header[4..36].copy_from_slice(&anchor_bytes);
    let merkle_root = double_sha256(&stratum_coinbase_bytes(
        recipient,
        anchor,
        salt,
        nonce,
        difficulty_bits,
    ));
    header[36..68].copy_from_slice(&merkle_root);
    header[68..72].copy_from_slice(&STRATUM_MINE_NTIME);
    header[72..76].copy_from_slice(&difficulty_bits.to_le_bytes());
    let (_, header_nonce) = unpack_stratum_nonce(nonce);
    header[76..80].copy_from_slice(&header_nonce);
    Ok(header)
}

pub(super) fn stratum_mine_signature(header: &[u8; 80]) -> String {
    let mut digest = double_sha256(header);
    digest.reverse();
    hex_encode(digest)
}

pub(super) fn stratum_mine_template(
    recipient: impl Into<String>,
    anchor: &str,
    salt: u64,
    difficulty_bits: u32,
) -> Result<StratumMineTemplate> {
    let recipient = recipient.into();
    validate_address(&recipient, "mine recipient")?;
    validate_hash(anchor, "mine transaction anchor")?;
    let anchor_bytes =
        decode_hex_array::<HASH_BYTES>(anchor).context("mine transaction anchor is not hex")?;
    Ok(StratumMineTemplate {
        recipient: recipient.clone(),
        anchor: anchor.to_string(),
        salt,
        difficulty_bits,
        coinbase_prefix: stratum_coinbase_prefix(&recipient, anchor, salt, difficulty_bits),
        version_hex: hex_encode(STRATUM_MINE_VERSION),
        prev_hash_hex: hex_encode(anchor_bytes),
        nbits_hex: hex_encode(difficulty_bits.to_le_bytes()),
        ntime_hex: hex_encode(STRATUM_MINE_NTIME),
    })
}

pub(super) fn hash_meets_difficulty(hash: &str, difficulty_bits: u32) -> bool {
    let full_zero_nibbles = (difficulty_bits / 4) as usize;
    let remaining_bits = difficulty_bits % 4;
    if hash.len() < full_zero_nibbles + usize::from(remaining_bits > 0) {
        return false;
    }
    if !hash.as_bytes()[..full_zero_nibbles]
        .iter()
        .all(|byte| *byte == b'0')
    {
        return false;
    }
    if remaining_bits == 0 {
        return true;
    }
    let Some(next) = hash.as_bytes().get(full_zero_nibbles).copied() else {
        return false;
    };
    let Some(value) = (next as char).to_digit(16) else {
        return false;
    };
    value < (1 << (4 - remaining_bits))
}

#[cfg(test)]
mod tests {
    use super::{
        STRATUM_EXTRANONCE1_HEX, STRATUM_EXTRANONCE2_SIZE, hash_meets_difficulty,
        pack_stratum_nonce, stratum_mine_header_bytes,
    };

    #[test]
    fn stratum_nonce_packs_extranonce_big_endian_and_header_nonce_little_endian() {
        let nonce = pack_stratum_nonce([0x01, 0x02, 0x03, 0x04], [0x08, 0x07, 0x06, 0x05]);

        assert_eq!(nonce, 0x0102_0304_0506_0708);
    }

    #[test]
    fn stratum_public_extranonce_contract_is_stable() {
        assert_eq!(STRATUM_EXTRANONCE1_HEX, "00000000");
        assert_eq!(STRATUM_EXTRANONCE2_SIZE, 4);
    }

    #[test]
    fn stratum_header_rejects_non_hex_anchor() {
        let error = stratum_mine_header_bytes("recipient", "not-hex", 0, 0, 12).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("mine transaction anchor is not hex")
        );
    }

    #[test]
    fn difficulty_check_handles_nibble_and_partial_nibble_targets() {
        assert!(hash_meets_difficulty("00f", 8));
        assert!(!hash_meets_difficulty("010", 8));
        assert!(hash_meets_difficulty("1ff", 3));
        assert!(!hash_meets_difficulty("2ff", 3));
    }
}
