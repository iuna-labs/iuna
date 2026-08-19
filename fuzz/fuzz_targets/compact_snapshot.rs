#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = iuna::adapters::chain_store::fuzz_decode_compact_snapshot(data);
});
