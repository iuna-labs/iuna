use anyhow::{Result, anyhow, bail};
use sha2::{Digest, Sha256};

pub fn hex_hash(input: impl AsRef<[u8]>) -> String {
    hex_encode(Sha256::digest(input.as_ref()))
}

pub(super) fn decode_hex_array<const N: usize>(input: &str) -> Result<[u8; N]> {
    let bytes = decode_hex(input)?;
    let len = bytes.len();
    bytes
        .try_into()
        .map_err(|_| anyhow!("expected {} hex bytes, got {len}", N))
}

pub(super) fn decode_hex(input: &str) -> Result<Vec<u8>> {
    if input.len() % 2 != 0 {
        bail!("hex string has odd length");
    }

    let mut bytes = Vec::with_capacity(input.len() / 2);
    for pair in input.as_bytes().chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_value(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid hex character"),
    }
}

pub(super) fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::{decode_hex, decode_hex_array, hex_encode, hex_hash};

    #[test]
    fn hex_roundtrips_bytes_and_accepts_uppercase() {
        let bytes = [0x00, 0x0f, 0x10, 0xab, 0xff];

        assert_eq!(hex_encode(bytes), "000f10abff");
        assert_eq!(decode_hex("000F10ABff").unwrap(), bytes);
        assert_eq!(decode_hex_array::<5>("000f10abff").unwrap(), bytes);
    }

    #[test]
    fn hex_decoder_rejects_odd_length_invalid_digits_and_wrong_array_size() {
        assert!(decode_hex("0").is_err());
        assert!(decode_hex("zz").is_err());
        assert!(decode_hex_array::<2>("00").is_err());
    }

    #[test]
    fn hex_hash_is_sha256_hex() {
        assert_eq!(
            hex_hash("iuna"),
            "a66946533b68cc0eb75a82632d7a28256633f5a06ef04e3906c1960d437239aa"
        );
    }
}
