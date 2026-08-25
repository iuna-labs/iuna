# Security Review Checklist

This document tracks the pre-mainnet security review. It is a review ledger, not
a claim that mainnet is safe. Keep the roadmap security-review checkbox open
until every launch-blocking item below is resolved or explicitly accepted.

## Review Scope

### Consensus Validation

Review:

- block validation, fork choice, finality depth, recovery blocks, and VDF checks;
- burn ticket eligibility, lineage limits, burn-list attestations, and bundle quorum;
- supply accounting across transfers, burns, fees, mine actions, and reorgs;
- genesis, snapshot adoption, and candidate-to-mainnet promotion rules.

Primary code:

- `src/domain/ledger_apply.rs`
- `src/domain/ledger_chain.rs`
- `src/domain/ledger_consensus.rs`
- `src/domain/ledger_reveal.rs`
- `src/domain/ticket.rs`
- `src/domain/vdf/mod.rs`
- `src/domain/protocol.rs`

Evidence already in the tree:

- adversarial consensus tests in `src/domain/adversarial_tests.rs`;
- release soak test in `tests/properties.rs`;
- protocol rules documented in `docs/protocol.md`;
- reset, joining, recovery, and rollback procedures in `docs/operator-playbooks.md`.

### Transaction And Mempool Validation

Review:

- signature validation and canonical transaction IDs;
- rejection of zero-fee non-genesis transfers and burns;
- double-spend and replay rejection;
- transaction count, block byte size, and economic byte-size accounting;
- block validation independence from local mempool contents;
- burn bundles cannot make the same burn count both as attested burn-list data
  and as normal block transaction space.

Primary code:

- `src/domain/ledger_ops.rs`
- `src/domain/ledger_mempool.rs`
- `src/domain/ledger_prepare.rs`
- `src/domain/transaction.rs`
- `src/domain/selection.rs`
- `src/domain/validation.rs`

Evidence already in the tree:

- focused adversarial tests for zero-fee burns, bundle import ordering,
  duplicate burn handling, mempool-independent block validation, and block
  selection bounds;
- compact economic-size tests in `src/domain/validation.rs`.

### P2P Input Handling

Review:

- network ID, genesis hash, protocol version, and setup-placeholder handshake
  behavior;
- inbound session caps and rejected-session metrics;
- gossip envelope item limits for blocks, transactions, burn bundles, and
  snapshots;
- snapshot validation before adoption or reorg;
- malformed compact snapshot and gossip JSON behavior.

Primary code:

- `src/adapters/p2p/handshake.rs`
- `src/adapters/p2p/line_codec.rs`
- `src/adapters/p2p/network.rs`
- `src/adapters/p2p/fetch.rs`
- `src/adapters/p2p/sync.rs`
- `src/adapters/chain_store/compact.rs`

Evidence already in the tree:

- P2P tests in `src/adapters/p2p/tests.rs`;
- compact snapshot malformed-input tests in `src/adapters/chain_store/compact.rs`;
- compact block-size boundary tests proving that selection and consensus use the
  same snapshot v6 block-body encoder;
- fuzz targets for `p2p_envelope`, `compact_snapshot`, and `domain_json`.

### Wallet, Key Storage, And HTTP Auth

Review:

- wallet seed encryption, KDF parameters, nonce handling, and authenticated
  encryption;
- atomic wallet/config/chain writes and crash consistency;
- password setup, login, session expiry, logout, and password change behavior;
- CSRF origin checks, forwarded-header handling, and login backoff;
- operational guidance that the management UI stays local-only.

Primary code:

- `src/adapters/wallet_store.rs`
- `src/adapters/config_store.rs`
- `src/adapters/http/request_auth.rs`
- `src/adapters/http/auth.rs`
- `src/adapters/http/actions.rs`

Evidence already in the tree:

- HTTP/auth abuse tests for CSRF, lockout/backoff, session expiry, and
  forwarded-header spoofing;
- wallet/config/chain crash-consistency tests;
- wallet/config persistence metadata fuzz target;
- local-only UI guidance in `README.md`.

### Stratum

Review:

- listener disabled by default unless enabled in settings or started with
  `--stratum`;
- line size, idle timeout, job cache, session, and connection limits;
- share parsing, worker address validation, and Stratum header validation;
- operational exposure guidance for public mining endpoints.

Primary code:

- `src/adapters/stratum.rs`
- `src/domain/stratum.rs`
- `src/domain/validation.rs`
- `src/cli.rs`
- `src/main.rs`

Evidence already in the tree:

- Stratum parser fuzz target;
- session-limit and request-limit tests;
- release soak test that exercises Stratum-enabled and Stratum-disabled nodes.

## Required Release Evidence

Before the mainnet-candidate launch, attach or publish logs for:

```sh
cargo test --locked
cargo check --locked --manifest-path fuzz/Cargo.toml
cargo run --locked --manifest-path fuzz/Cargo.toml --bin p2p_envelope -- -runs=256 fuzz/corpus/p2p_envelope
cargo run --locked --manifest-path fuzz/Cargo.toml --bin compact_snapshot -- -runs=256 fuzz/corpus/compact_snapshot
cargo run --locked --manifest-path fuzz/Cargo.toml --bin domain_json -- -runs=256 fuzz/corpus/domain_json
cargo run --locked --manifest-path fuzz/Cargo.toml --bin stratum_request -- -runs=256 fuzz/corpus/stratum_request
cargo run --locked --manifest-path fuzz/Cargo.toml --bin wallet_config -- -runs=256 fuzz/corpus/wallet_config
cargo run --locked --manifest-path fuzz/Cargo.toml --bin vdf_proof -- -runs=16 fuzz/corpus/vdf_proof
cargo test --locked domain::adversarial_tests:: -- --ignored
cargo test --locked --release --test properties -- --ignored
```

On macOS hosts with the optional Python/C++ `chiavdf` package installed, also
run the byte-for-byte compatibility test:

```sh
IUNA_CHIAVDF_PYTHON=/path/to/python cargo test --locked --release domain::vdf::wesolowski::tests::prover_matches_chiavdf_python_binding -- --ignored --nocapture
```

The deployment script runs these gates for release builds. Keep the exact
command output with the candidate release notes so independent operators can
see which revision was tested.

## Launch-Blocking Review Items

- VDF implementation: `docs/protocol.md` documents the Rust-only,
  Chia-compatible class-group Wesolowski VDF and its
  `classgroup-wesolowski-bqfc-v1` solution encoding. The implementation must not
  depend on GMP, MPIR, or native runtime libraries. On local Apple Silicon, a
  100,000-round release benchmark with seed `iuna-vdf-prover-benchmark` measured
  about `0.89s` to `0.92s` for the current Rust-only checkpoint prover and about `1.78s`
  for the Rust-only constant-memory prover after moving output squaring, proof
  composition, and proof squaring onto the local custom `Vec<u64>` limb backend
  with reusable division/GCD/reduction scratch buffers, Lehmer-style full and
  partial XGCD batching, x-only extended-GCD paths for call sites that do not
  need the second Bezout coefficient, positive-input left-GCD fast paths,
  mutable Lehmer linear-combination outputs for XGCD batch updates, `u64`
  Lehmer quotient windows, scratch-backed scalar combinations, one-limb scalar
  multiplication into scratch buffers, quotient/remainder-directed
  division outputs, exact power-of-two division fast paths, and owned
  add/sub/shift helpers for formula temporaries, clone-free signed subtraction,
  scratch-backed reduction steps with small-quotient fast paths and quotient
  comparison that avoids temporary doubled limbs,
  tighter add/sub limb loops, one-limb multiplication, small-shift fast paths,
  plus sparse proof buckets that keep empty buckets implicit instead of cloning
  full identity forms or composing identity aggregates, and per-pass incremental
  checkpoint bucket selection with a 100,000-round checkpoint parameter floor of
  `k = 10`, release thin-LTO/codegen-unit tuning, and replacement of
  per-checkpoint modular exponentiation with one modular exponentiation plus
  fixed modular steps; an official
  Python/C++ `chiavdf` reference measurement was about `0.673s`. Phase profiling
  measured about `0.78s` to `0.81s` in output squaring and about `0.11s` to `0.12s` in proof
  construction. The limb backend covers signed limb arithmetic, division, full
  and partial XGCD, production NUDUPL/NUCOMP, checkpoint bucket selection, and
  class-group exponentiation.
  Before promotion, keep running fixed vectors, differential VDF tests, fuzz
  targets, release benchmarks, and the optional `chiavdf` compatibility test on
  at least one macOS host. Windows MSVC release benchmarking remains a platform
  readiness item, not a wire-compatibility requirement.
- Wallet/key handling: reviewed for the mainnet-candidate run. Wallet files use
  versioned JSON. Plaintext seed files are still supported for legacy/setup
  flows, but setting a management password encrypts existing or newly generated
  wallets before normal authenticated use. Encrypted wallets store no plaintext
  seed, use `chacha20poly1305` with a random 16-byte salt, random 12-byte nonce,
  PBKDF2-SHA256 at 210,000 iterations, and bind the ciphertext to the wallet
  address as AEAD associated data. Unlock rejects unsupported algorithms, KDFs,
  unreasonable PBKDF2 iteration counts, wrong salt/nonce lengths, wrong
  passwords, and address/seed mismatches. Wallet writes are atomic and use
  `0600` temporary files on Unix. Management UI password hashes also use
  PBKDF2-SHA256 with bounded iteration counts, login backoff, session expiry,
  `HttpOnly`/`SameSite=Strict` cookies, CSRF same-origin checks, and trusted
  forwarded headers only from loopback proxies. Residual accepted risk for the
  candidate: PBKDF2 is CPU-hard rather than memory-hard, so operators must use
  strong unique passwords and keep the management UI local or otherwise
  protected.
- Public exposure: verify bootnodes expose only the intended P2P and optional
  Stratum ports, and that the management UI remains bound to a local or
  otherwise protected address.
- Candidate manifest: release coordination must publish the real genesis hash,
  network ID, bootnodes, checksums, reset and rollback instructions, release tag,
  and git commit before the stability window starts. The protocol and operator
  playbooks are the maintained in-tree references.
- Release evidence: keep successful release-gate logs from the exact tagged
  candidate revision.

## Sign-Off Table

Do not mark the roadmap security-review item complete until this table is
filled in for the candidate release.

| Area | Reviewer | Date | Result | Notes |
| --- | --- | --- | --- | --- |
| Consensus validation |  |  |  |  |
| Transaction and mempool validation |  |  |  |  |
| P2P input handling |  |  |  |  |
| Wallet, key storage, and HTTP auth | Codex | 2026-08-21 | Candidate accepted with residual operational risk | Reviewed encryption/auth paths; added PBKDF2 iteration and salt-length hardening. |
| Stratum |  |  |  |  |
| Release evidence |  |  |  |  |
| Candidate manifest |  |  |  |  |
