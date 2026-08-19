#![no_main]

use iuna::{
    app::GossipEnvelope,
    domain::{Block, BurnBundle, ChainSnapshot, Transaction},
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() > 2 * 1024 * 1024 {
        return;
    }
    let Ok(line) = std::str::from_utf8(data) else {
        return;
    };

    let _ = serde_json::from_str::<Transaction>(line);
    let _ = serde_json::from_str::<BurnBundle>(line);
    let _ = serde_json::from_str::<Block>(line).map(|block| {
        let _ = block.serialized_size_bytes();
        let _ = block.compute_hash();
    });
    let _ = serde_json::from_str::<ChainSnapshot>(line);
    let _ = serde_json::from_str::<GossipEnvelope>(line);
});
