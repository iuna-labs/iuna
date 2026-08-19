#![no_main]

use iuna::adapters::{config_store, wallet_store};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() > 1024 * 1024 {
        return;
    }

    let _ = config_store::fuzz_parse_config(data);
    let _ = wallet_store::fuzz_parse_wallet_metadata(data);
});
