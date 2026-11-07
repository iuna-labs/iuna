//! `wallet.json` can be damaged or hand-edited. Reading it must fail cleanly and must reject a
//! hostile KDF cost before deriving anything, so `load` stays fast on every input.
#![no_main]

use iuna::adapters::consensus_wallet_store::{load, public_key};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("wallet.json");
    std::fs::write(&path, bytes).expect("writing the input");
    let _ = public_key(&path);
    let _ = load(&path, "fuzzing password");
});
