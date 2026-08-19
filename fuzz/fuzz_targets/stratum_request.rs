#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(line) = std::str::from_utf8(data) {
        let _ = iuna::adapters::stratum::fuzz_parse_stratum_request(line);
    }
});
