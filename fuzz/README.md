# iuna Fuzz Targets

These targets exercise external input boundaries and persistence decoders.

Install `cargo-fuzz` once:

```sh
cargo install cargo-fuzz
```

Run a target:

```sh
cargo fuzz run p2p_envelope
cargo fuzz run compact_snapshot
cargo fuzz run domain_json
cargo fuzz run stratum_request
```

Short smoke run without installing `cargo-fuzz`:

```sh
cargo run --manifest-path fuzz/Cargo.toml --bin p2p_envelope -- -runs=1 fuzz/corpus/p2p_envelope
cargo run --manifest-path fuzz/Cargo.toml --bin compact_snapshot -- -runs=1 fuzz/corpus/compact_snapshot
cargo run --manifest-path fuzz/Cargo.toml --bin domain_json -- -runs=1 fuzz/corpus/domain_json
cargo run --manifest-path fuzz/Cargo.toml --bin stratum_request -- -runs=1 fuzz/corpus/stratum_request
```

Targets intentionally accept malformed input. A parse error is fine; panics,
crashes, excessive allocation, or timeouts are failures.
