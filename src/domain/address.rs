use anyhow::{Context, Result, bail};
use ed25519_dalek::VerifyingKey;

use super::{PUBLIC_KEY_BYTES, decode_hex_array, hex_encode};

const ADDRESS_VERSION: u8 = 0;
const BECH32M_CONST: u32 = 0x2bc8_30a3;
const BECH32_CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const MAX_BECH32_LENGTH: usize = 90;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressNetwork {
    Mainnet,
    Testnet,
}

impl AddressNetwork {
    pub fn from_profile_id(profile_id: &str) -> Self {
        if matches!(profile_id, "iuna-local-testnet-v1" | "iuna-local-e2e-5s-v1") {
            Self::Testnet
        } else {
            Self::Mainnet
        }
    }

    fn hrp(self) -> &'static str {
        match self {
            Self::Mainnet => "iuna",
            Self::Testnet => "tiuna",
        }
    }
}

/// Encodes the internal canonical Ed25519 public-key representation for display.
pub fn encode_address(public_key_hex: &str, network: AddressNetwork) -> Result<String> {
    let public_key = decode_hex_array::<PUBLIC_KEY_BYTES>(public_key_hex)
        .context("address public key must be 32-byte hexadecimal")?;
    validate_public_key(&public_key)?;

    let mut data = vec![ADDRESS_VERSION];
    data.extend(convert_bits(&public_key, 8, 5, true)?);
    let checksum = create_checksum(network.hrp(), &data);
    let mut encoded = String::with_capacity(network.hrp().len() + 1 + data.len() + 6);
    encoded.push_str(network.hrp());
    encoded.push('1');
    for value in data.into_iter().chain(checksum) {
        encoded.push(char::from(BECH32_CHARSET[usize::from(value)]));
    }
    Ok(encoded)
}

/// Decodes a user-facing Bech32m address to the internal canonical public-key hex.
///
/// Legacy hexadecimal addresses are deliberately not accepted here. They remain
/// valid only inside existing consensus data and wallet files.
pub fn decode_address(address: &str, expected_network: AddressNetwork) -> Result<String> {
    let address = address.trim();
    if address.is_empty() {
        bail!("address is required");
    }
    if address.len() > MAX_BECH32_LENGTH || !address.is_ascii() {
        bail!("address is not valid Bech32m");
    }
    let has_lower = address.bytes().any(|byte| byte.is_ascii_lowercase());
    let has_upper = address.bytes().any(|byte| byte.is_ascii_uppercase());
    if has_lower && has_upper {
        bail!("Bech32m address must not mix uppercase and lowercase");
    }
    let normalized = address.to_ascii_lowercase();
    let separator = normalized
        .rfind('1')
        .context("address is missing the Bech32m separator")?;
    if separator == 0 || separator + 7 > normalized.len() {
        bail!("address has an invalid Bech32m structure");
    }
    let hrp = &normalized[..separator];
    if hrp != expected_network.hrp() {
        bail!(
            "address belongs to {} instead of {}",
            network_label_for_hrp(hrp),
            network_label(expected_network)
        );
    }
    let data = normalized[separator + 1..]
        .bytes()
        .map(decode_charset)
        .collect::<Result<Vec<_>>>()?;
    if !verify_checksum(hrp, &data) {
        bail!("address checksum is invalid");
    }
    let payload = &data[..data.len() - 6];
    let Some((&version, encoded_key)) = payload.split_first() else {
        bail!("address payload is empty");
    };
    if version != ADDRESS_VERSION {
        bail!("unsupported address version {version}");
    }
    let public_key = convert_bits(encoded_key, 5, 8, false)?;
    let public_key: [u8; PUBLIC_KEY_BYTES] = public_key.try_into().map_err(|bytes: Vec<u8>| {
        anyhow::anyhow!("address public key has {} bytes", bytes.len())
    })?;
    validate_public_key(&public_key)?;
    Ok(hex_encode(public_key))
}

/// Converts a pre-mainnet hex address for one-time display/migration tooling.
pub fn migrate_legacy_address(address: &str, network: AddressNetwork) -> Result<String> {
    encode_address(&address.to_ascii_lowercase(), network)
}

fn validate_public_key(public_key: &[u8; PUBLIC_KEY_BYTES]) -> Result<()> {
    let verifying_key = VerifyingKey::from_bytes(public_key)
        .context("address payload is not a valid Ed25519 verifying key")?;
    if verifying_key.is_weak() {
        bail!("address payload contains a weak Ed25519 verifying key");
    }
    Ok(())
}

fn network_label(network: AddressNetwork) -> &'static str {
    match network {
        AddressNetwork::Mainnet => "mainnet",
        AddressNetwork::Testnet => "testnet",
    }
}

fn network_label_for_hrp(hrp: &str) -> &'static str {
    match hrp {
        "iuna" => "mainnet",
        "tiuna" => "testnet",
        _ => "an unknown network",
    }
}

fn decode_charset(byte: u8) -> Result<u8> {
    BECH32_CHARSET
        .iter()
        .position(|candidate| *candidate == byte)
        .map(|index| index as u8)
        .context("address contains a character outside the Bech32 alphabet")
}

fn create_checksum(hrp: &str, data: &[u8]) -> [u8; 6] {
    let mut values = hrp_expand(hrp);
    values.extend_from_slice(data);
    values.extend_from_slice(&[0; 6]);
    let polymod = polymod(&values) ^ BECH32M_CONST;
    std::array::from_fn(|index| ((polymod >> (5 * (5 - index))) & 31) as u8)
}

fn verify_checksum(hrp: &str, data: &[u8]) -> bool {
    let mut values = hrp_expand(hrp);
    values.extend_from_slice(data);
    polymod(&values) == BECH32M_CONST
}

fn hrp_expand(hrp: &str) -> Vec<u8> {
    let mut expanded = Vec::with_capacity(hrp.len() * 2 + 1);
    expanded.extend(hrp.bytes().map(|byte| byte >> 5));
    expanded.push(0);
    expanded.extend(hrp.bytes().map(|byte| byte & 31));
    expanded
}

fn polymod(values: &[u8]) -> u32 {
    const GENERATORS: [u32; 5] = [
        0x3b6a_57b2,
        0x2650_8e6d,
        0x1ea1_19fa,
        0x3d42_33dd,
        0x2a14_62b3,
    ];
    let mut checksum = 1_u32;
    for value in values {
        let top = checksum >> 25;
        checksum = (checksum & 0x01ff_ffff) << 5 ^ u32::from(*value);
        for (index, generator) in GENERATORS.iter().enumerate() {
            if (top >> index) & 1 != 0 {
                checksum ^= generator;
            }
        }
    }
    checksum
}

fn convert_bits(data: &[u8], from: u8, to: u8, pad: bool) -> Result<Vec<u8>> {
    let mut accumulator = 0_u32;
    let mut bit_count = 0_u8;
    let max_value = (1_u32 << to) - 1;
    let max_accumulator = (1_u32 << (from + to - 1)) - 1;
    let mut converted = Vec::new();
    for value in data {
        if u32::from(*value) >> from != 0 {
            bail!("address payload contains an out-of-range value");
        }
        accumulator = ((accumulator << from) | u32::from(*value)) & max_accumulator;
        bit_count += from;
        while bit_count >= to {
            bit_count -= to;
            converted.push(((accumulator >> bit_count) & max_value) as u8);
        }
    }
    if pad {
        if bit_count > 0 {
            converted.push(((accumulator << (to - bit_count)) & max_value) as u8);
        }
    } else if bit_count >= from || ((accumulator << (to - bit_count)) & max_value) != 0 {
        bail!("address payload has invalid padding");
    }
    Ok(converted)
}

#[cfg(test)]
mod tests {
    use super::{
        AddressNetwork, decode_address, decode_charset, encode_address, migrate_legacy_address,
        verify_checksum,
    };
    use crate::domain::Wallet;

    fn wallet_key() -> String {
        Wallet::from_seed("address-format-test")
            .address()
            .to_string()
    }

    #[test]
    fn checksum_matches_bip350_bech32m_vectors() {
        let valid = "lqfn3a"
            .bytes()
            .map(decode_charset)
            .collect::<anyhow::Result<Vec<_>>>()
            .unwrap();
        let old_bech32 = "g7sgd8"
            .bytes()
            .map(decode_charset)
            .collect::<anyhow::Result<Vec<_>>>()
            .unwrap();

        assert!(verify_checksum("a", &valid)); // BIP-350: A1LQFN3A
        assert!(!verify_checksum("a", &old_bech32)); // Bech32, not Bech32m
    }

    #[test]
    fn mainnet_and_testnet_addresses_roundtrip_to_the_same_key() {
        let key = wallet_key();
        let mainnet = encode_address(&key, AddressNetwork::Mainnet).unwrap();
        let testnet = encode_address(&key, AddressNetwork::Testnet).unwrap();

        assert!(mainnet.starts_with("iuna1q"));
        assert!(testnet.starts_with("tiuna1q"));
        assert_eq!(
            decode_address(&mainnet, AddressNetwork::Mainnet).unwrap(),
            key
        );
        assert_eq!(
            decode_address(&testnet, AddressNetwork::Testnet).unwrap(),
            key
        );
        assert_ne!(mainnet, testnet);
    }

    #[test]
    fn checksum_typos_and_network_mixups_are_rejected() {
        let key = wallet_key();
        let mainnet = encode_address(&key, AddressNetwork::Mainnet).unwrap();
        let mut typo = mainnet.clone();
        let replacement = if typo.ends_with('q') { 'p' } else { 'q' };
        typo.pop();
        typo.push(replacement);

        assert!(decode_address(&typo, AddressNetwork::Mainnet).is_err());
        assert!(
            decode_address(&mainnet, AddressNetwork::Testnet)
                .unwrap_err()
                .to_string()
                .contains("instead of testnet")
        );
    }

    #[test]
    fn production_reported_mainnet_address_is_valid() {
        let address = "iuna1q7fj9u5pqpuq0gfl4a3a7afsqfmfm22x4lgz4w8aqmggs4e33aqfqkx5kyt";

        assert!(decode_address(address, AddressNetwork::Mainnet).is_ok());
    }

    #[test]
    fn uppercase_is_accepted_but_mixed_case_is_rejected() {
        let key = wallet_key();
        let address = encode_address(&key, AddressNetwork::Mainnet).unwrap();

        assert_eq!(
            decode_address(&address.to_ascii_uppercase(), AddressNetwork::Mainnet).unwrap(),
            key
        );
        let mut mixed = address;
        mixed.replace_range(..1, "I");
        assert!(decode_address(&mixed, AddressNetwork::Mainnet).is_err());
    }

    #[test]
    fn malformed_keys_hex_and_legacy_user_input_are_rejected() {
        let invalid_key = "00".repeat(32);
        assert!(encode_address(&invalid_key, AddressNetwork::Mainnet).is_err());
        assert!(decode_address(&wallet_key(), AddressNetwork::Mainnet).is_err());
        assert!(
            migrate_legacy_address(&wallet_key(), AddressNetwork::Mainnet)
                .unwrap()
                .starts_with("iuna1q")
        );
    }
}
