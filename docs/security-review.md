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
- `src/domain/vdf.rs`
- `src/domain/protocol.rs`

Evidence already in the tree:

- adversarial consensus tests in `src/domain/adversarial_tests.rs`;
- release soak test in `tests/properties.rs`;
- protocol rules documented in `docs/protocol.md`;
- candidate rehearsal and rollback process in `docs/genesis.md`.

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
cargo test --locked --release --test properties -- --ignored
```

The deployment script runs these gates for release builds. Keep the exact
command output with the candidate release notes so independent operators can
see which revision was tested.

## Launch-Blocking Review Items

- VDF trust assumption: `docs/protocol.md` documents that the devnet uses the
  public RSA-2048 challenge modulus and that production mainnet should use a
  purpose-specific trusted setup or class-group VDF. Before promotion, decide
  whether this is accepted for the candidate or blocks mainnet.
- Wallet/key handling: review the encrypted wallet format, PBKDF2 iteration
  count, password UX, recovery phrase exposure, and backup guidance.
- Public exposure: verify bootnodes expose only the intended P2P and optional
  Stratum ports, and that the management UI remains bound to a local or
  otherwise protected address.
- Candidate manifest: verify genesis hash, network ID, bootnodes, checksums,
  promotion policy, and rollback instructions before the stability window
  starts.
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
| Wallet, key storage, and HTTP auth |  |  |  |  |
| Stratum |  |  |  |  |
| Release evidence |  |  |  |  |
| Candidate manifest |  |  |  |  |
