# Fuzzing

Targets for the bytes a node accepts from outside. They need a nightly toolchain and
[`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz):

```bash
cargo +nightly fuzz run p2p_envelope -- -max_total_time=60
cargo +nightly fuzz run signed_objects -- -max_total_time=60
cargo +nightly fuzz run wallet_file -- -max_total_time=60
```

- `p2p_envelope`: lines from peers, including the encode and decode roundtrip.
- `signed_objects`: transactions and tickets as JSON, from the public API and relay.
- `wallet_file`: damaged or hand-edited `wallet.json` files.

Seeds live in `corpus/<target>/`. A crash is saved in `artifacts/<target>/` and replays with
`cargo +nightly fuzz run <target> <file>`.
