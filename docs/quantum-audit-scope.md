# Quantum migration audit scope

## Purpose and decision boundary

This document defines the review package for Iuna's post-quantum migration. The current code
reserves versioned addresses and transaction encodings, verifies hybrid Ed25519 + ML-DSA-44
authorizations, and activated transaction v2 at height 3000. Live consensus and blocks accept v2
transactions, and nodes relay canonical v2 mempool envelopes only across sessions that negotiated
the `transaction-v2-mempool` capability. The management wallet exposes
migration telemetry, reviewed block-bounded migration submission, and hybrid transfers.

An audit of this scope must review the active consensus code and the enabled wallet migration
flow. It must cover the final integration commit and the
post-height-3000 migration rehearsal described in `quantum-migration.md`.

## Security claims

The candidate design intends to provide the following properties after a separately reviewed
activation:

1. Spending an address-v1 output requires valid Ed25519 and ML-DSA-44 signatures over exactly the
   same canonical payload.
2. An address-v1 output commits to the scheme identifier, component lengths, and exact public-key
   encodings without publishing those keys before the output is spent.
3. A transaction-v2 signature cannot be replayed across chain IDs, genesis blocks, transaction
   kinds, inputs, outputs, amounts, fees, or authorization order.
4. Unknown versions and signature schemes fail closed.
5. Transaction identity is a fixed-size domain-separated hash of the complete canonical
   transaction, not a signature value.
6. Malformed lengths, counts, and encodings are rejected before attacker-controlled allocation or
   cryptographic work becomes unbounded.
7. Version-0 historical data and transactions remain valid under their existing rules.
8. An explicit transaction-v2 migration may reference a tagged 32-byte legacy hash or 64-byte
   legacy signature ID and spend its version-0 output with the existing Ed25519 key. Ordinary v2
   transactions use 32-byte hash IDs, and the resulting version-1 output cannot be spent without
   both signature components.

The repository does not currently claim that wallet transactions, consensus identities, the VDF,
P2P identity, TLS, or software updates are post-quantum secure.

## Threat model

The review should consider:

- a remote unauthenticated peer supplying arbitrary transaction-v2 bytes;
- a malicious spender choosing related Ed25519 and ML-DSA keys, signatures, and encodings;
- replay across networks, genesis blocks, transaction kinds, inputs, and activation boundaries;
- parser differentials between mempool, block, snapshot, JSON, and compact encodings;
- denial of service through large counts, lengths, signature verification, or repeated invalid
  signatures;
- compromise of either the classical or post-quantum algorithm, but not both simultaneously;
- implementation or supply-chain defects in the pinned ML-DSA backend;
- a future cryptographically relevant quantum computer attacking public keys already visible on
  chain;
- rollback, partial deployment, and partitions involving nodes that do not understand the new
  protocol.

Compromise of both signature components, endpoint compromise while signing, malicious release
binaries, and recovery of value whose owner has lost all signing material remain out of scope for
the transaction-v2 cryptographic claim.

## Review targets

The minimum code-review scope is:

- `src/domain/signature.rs`: scheme identifiers, exact lengths, backend decoding, and verification;
- `src/domain/address.rs`: version retention, Bech32m parsing, and key commitments;
- `src/domain/transaction_v2.rs`: canonical encoding, parsing, domain separation, transaction IDs,
  authorization binding, resource limits, and the fixed activation gate;
- `src/domain.rs`: public boundaries and fuzz-only exposure;
- `tests/vectors/`: pinned NIST and Wycheproof provenance and expected verdicts;
- `fuzz/fuzz_targets/transaction_v2.rs`: decoder and verifier coverage;
- `deployment.sh`: the release fuzzing gate and retained evidence;
- all future call sites that connect transaction v2 to wallets, mempool, gossip, blocks, compact
  storage, fees, or fork validation.

The exact review commit and all three Cargo lockfile hashes must be recorded when an engagement
starts. Any subsequent change to the files above invalidates approval until the auditor assesses
the delta. From a clean review checkout, `scripts/quantum-audit-manifest.sh --output
quantum-audit-manifest.json` records these identifiers, tool versions, and upstream vector
provenance in one machine-readable file.

## Required invariants and negative tests

An auditor should independently confirm at least these cases:

- accepting either signature alone is impossible;
- a version-0 input accepts only its matching Ed25519 authorization, while a version-1 input
  accepts only its matching hybrid authorization;
- swapping either public-key or signature component fails;
- signatures over different chain IDs or genesis hashes fail;
- reordering inputs, outputs, or authorizations fails or produces the uniquely specified payload;
- duplicate inputs and authorization-count mismatches are rejected by the eventual consensus call
  site;
- non-canonical and trailing encodings fail rather than normalize;
- address commitments cannot be ambiguous across schemes or component boundaries;
- transaction IDs change when any authorization byte changes;
- maximum counts and lengths cannot overflow size accounting or cause excessive allocation;
- transaction v2 remains rejected through height 2999 and becomes eligible at height 3000;
- 0.4.30-shaped handshakes remain compatible before the activation boundary; new handshakes are
  rejected once either peer is preparing height 3000 and omits `transaction-v2-blocks`. Auditors
  must still verify the release/reconnect procedure for sessions opened before that boundary.

## Techniques adopted and rejected

Iuna adopts the key-hiding and versioned-output pattern proposed by Bitcoin P2QRH, but does not
depend on that proposal's deployment or exact script design. It adopts hybrid signatures for the
migration interval and a staged read-before-activation rollout.

XMSS, used by QRL and standardized for restricted use by NIST SP 800-208, is not selected for
ordinary wallets because safe signing depends on durable one-time-signature state across backups
and devices. Stateless SLH-DSA remains a possible emergency recovery scheme, but would receive a
new scheme identifier and a separate size, fee, and implementation review.

Algorand-style post-quantum state proofs motivate a later checkpoint phase, but Iuna must first
specify who owns post-quantum checkpoint keys and how signer authority follows consensus. A
maintainer-signed snapshot is useful release evidence, but is not a decentralized finality proof
and must never be presented as one.

## Reproduction

From the repository root, reviewers should run:

```sh
cargo test --all-targets --all-features --locked
cargo clippy --all-targets --all-features -- -D warnings
cargo build --locked --manifest-path fuzz/Cargo.toml --bins
cargo test ml_dsa44_matches_pinned_nist_and_wycheproof_vectors --lib
cargo +nightly fuzz run transaction_v2 -- -max_total_time=300 -timeout=10
```

The curated vector set is a regression suite, not a substitute for running complete upstream
corpora or reviewing the cryptographic backend. Reviewers should verify the upstream file hashes
recorded in `tests/vectors/ml_dsa44_audit.json` and retain tool versions, corpus, coverage data, and
crash artifacts with their report.

## Activation blockers

The active transaction-v2 and wallet-migration rules must not be described as production-ready
until all of the following are resolved:

- the cryptographic backend and Iuna integration are independently reviewed;
- the final consensus call sites and byte-based fee accounting receive independent review;
- wallet backup compatibility, migration batching, hybrid spending, recovery, and no-address-reuse
  behavior are reviewed; deterministic hybrid keys, block-bounded migration batches, and hybrid
  transfers exist, but address rotation remains incomplete;
- migration progress is observable without exposing wallet secrets;
- advertised `transaction-v2-blocks` behavior (including already-open sessions), restored
  snapshot-v7 behavior, and activation-boundary recovery are rehearsed on the mainnet-candidate
  network;
- post-quantum plans exist for peer identity and update signing;
- checkpoint signer authority is specified before any PQ checkpoint format is trusted;
- the VDF has a separate quantum threat analysis;
- an activation abort procedure exists before a release can reach activation height 3000.

## Expected audit deliverables

The engagement should produce a public report containing the reviewed commit, scope exclusions,
toolchain and dependency versions, findings with severity and exploit prerequisites, test evidence,
and an explicit verdict for dormant shipping versus mainnet activation. Fixes must be reviewed as a
documented delta rather than assumed resolved by the project team.

## Primary references

- NIST FIPS 204, Module-Lattice-Based Digital Signature Standard:
  <https://csrc.nist.gov/pubs/fips/204/final>
- NIST FIPS 205, Stateless Hash-Based Digital Signature Standard:
  <https://csrc.nist.gov/pubs/fips/205/final>
- NIST SP 800-208, stateful hash-based signature recommendations:
  <https://csrc.nist.gov/pubs/sp/800/208/final>
- RFC 8391, XMSS: <https://www.rfc-editor.org/rfc/rfc8391>
- Bitcoin BIP 360, Pay to Quantum Resistant Hash: <https://bips.dev/360/>
- Algorand State Proofs: <https://developer.algorand.org/docs/get-details/stateproofs/>
