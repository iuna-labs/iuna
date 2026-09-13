# iuna Fuzz Targets

These targets exercise external input boundaries and persistence decoders.

Install the release-pinned `cargo-fuzz` version once. Building the helper may use a newer stable
toolchain without changing Iuna's Rust 1.88 build contract:

```sh
cargo +1.98.1 install cargo-fuzz --version 0.13.2 --locked
```

Run a target:

```sh
cargo fuzz run p2p_envelope
cargo fuzz run compact_snapshot
cargo fuzz run domain_json
cargo fuzz run stratum_request
cargo fuzz run wallet_config
cargo fuzz run vdf_proof
cargo fuzz run transaction_v2
```

`cargo fuzz` requires a nightly Rust toolchain and adds sanitizer and coverage instrumentation.
For a bounded run, use for example:

```sh
cargo +nightly fuzz run transaction_v2 -- -max_total_time=300
```

The release gate also applies a 10-second per-input timeout so hangs are retained
as actionable fuzzing failures alongside crashes.

Short, uninstrumented crash smoke run without installing `cargo-fuzz`:

```sh
cargo run --manifest-path fuzz/Cargo.toml --bin p2p_envelope -- -runs=1 fuzz/corpus/p2p_envelope
cargo run --manifest-path fuzz/Cargo.toml --bin compact_snapshot -- -runs=1 fuzz/corpus/compact_snapshot
cargo run --manifest-path fuzz/Cargo.toml --bin domain_json -- -runs=1 fuzz/corpus/domain_json
cargo run --manifest-path fuzz/Cargo.toml --bin stratum_request -- -runs=1 fuzz/corpus/stratum_request
cargo run --manifest-path fuzz/Cargo.toml --bin wallet_config -- -runs=1 fuzz/corpus/wallet_config
cargo run --manifest-path fuzz/Cargo.toml --bin vdf_proof -- -runs=1 fuzz/corpus/vdf_proof
cargo run --manifest-path fuzz/Cargo.toml --bin transaction_v2 -- -runs=1 fuzz/corpus/transaction_v2
```

Targets intentionally accept malformed input. A parse error is fine; panics,
crashes, excessive allocation, or timeouts are failures.
