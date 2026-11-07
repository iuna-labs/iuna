//! Peers send arbitrary bytes. A line must decode or fail cleanly, and whatever decodes must
//! survive the same shape checks and re-encoding the node applies.
#![no_main]

use iuna::adapters::consensus_p2p::{decode_line, encode_line};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|line: &[u8]| {
    let Ok(envelope) = decode_line(line) else {
        return;
    };
    let _ = envelope.validate_shape();
    if let Ok(encoded) = encode_line(&envelope) {
        let decoded = decode_line(&encoded).expect("an encoded envelope decodes");
        assert_eq!(
            encode_line(&decoded).expect("a decoded envelope encodes"),
            encoded
        );
    }
});
