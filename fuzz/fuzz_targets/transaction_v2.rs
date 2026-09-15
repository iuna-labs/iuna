#![no_main]

use iuna::domain::{TransactionV2, fuzz_verify_ml_dsa44};
use libfuzzer_sys::fuzz_target;

const PUBLIC_KEY_BYTES: usize = 1_312;
const SIGNATURE_BYTES: usize = 2_420;
const MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    exercise(data);
    if let Some(hex) = data.strip_prefix(b"hex:") {
        if let Some(decoded) = decode_hex(hex) {
            exercise(&decoded);
        }
    }
});

fn exercise(data: &[u8]) {
    if let Ok((domain, transaction)) = TransactionV2::decode(data) {
        let _ = transaction.verify_authorizations(&domain);
        let _ = transaction.encode(&domain);
    }

    if let Some((public_key, rest)) = data.split_at_checked(PUBLIC_KEY_BYTES)
        && let Some((signature, payload)) = rest.split_at_checked(SIGNATURE_BYTES)
    {
        fuzz_verify_ml_dsa44(public_key, payload, signature);
    }
}

fn decode_hex(encoded: &[u8]) -> Option<Vec<u8>> {
    if encoded.len() % 2 != 0 || encoded.len() / 2 > MAX_INPUT_BYTES {
        return None;
    }
    encoded
        .chunks_exact(2)
        .map(|pair| Some(decode_nibble(pair[0])? << 4 | decode_nibble(pair[1])?))
        .collect()
}

fn decode_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
