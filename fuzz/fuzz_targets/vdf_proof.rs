#![no_main]

use iuna::domain::verify_vdf;
use libfuzzer_sys::fuzz_target;

const HEX: &[u8; 16] = b"0123456789abcdef";

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    let mut proof = [0u8; 200];
    let hex_data = data.strip_suffix(b"\n").unwrap_or(data);
    if hex_data.len() == proof.len() * 2 {
        for (output, encoded) in proof.iter_mut().zip(hex_data.chunks_exact(2)) {
            let Some(high) = decode_nibble(encoded[0]) else {
                return;
            };
            let Some(low) = decode_nibble(encoded[1]) else {
                return;
            };
            *output = high << 4 | low;
        }
    } else {
        let copied = data.len().min(proof.len());
        proof[..copied].copy_from_slice(&data[..copied]);
    }

    let mut encoded = String::with_capacity(30 + proof.len() * 2);
    encoded.push_str("classgroup-wesolowski-bqfc-v1:");
    for byte in proof {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }

    let _ = verify_vdf("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB", 300, &encoded);
});

fn decode_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
