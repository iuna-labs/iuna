# Quantum-resistance migration

## Status

Iuna is not currently post-quantum secure. The live mainnet-candidate protocol uses Ed25519 for
wallet transactions, leader proofs, burn-bundle attestations, peer identity, and release signing.
Its class-group Wesolowski VDF also does not carry a post-quantum security claim.

This document defines the migration constraints and staged protocol shape. The candidate-network
implementation now contains the fixed height-3000 transaction-v2 consensus gate. Shipping a
release that can reach that height still requires independent cryptographic review, adversarial
tests, and a multi-node migration rehearsal.

The standardized signature candidates are ML-DSA (FIPS 204) and SLH-DSA (FIPS 205). The initial
transaction candidate is a hybrid of Ed25519 and ML-DSA-44: both signatures must verify. Hybrid
mode protects the transition if either the classical or post-quantum component later fails, but it
does not remove the need to migrate before a cryptographically relevant quantum computer exists.

## Stable scheme identifiers

The protocol reserves these one-byte identifiers:

| ID | Scheme | Public key bytes | Signature bytes | Consensus status |
|---:|---|---:|---:|---|
| 0 | Ed25519 | 32 | 64 | active legacy scheme |
| 1 | ML-DSA-44 | 1,312 | 2,420 | reserved |
| 2 | Ed25519 + ML-DSA-44 | 1,344 | 2,484 | reserved |

IDs identify exact parameter sets and encodings, not algorithm families. A future parameter or
encoding change receives a new ID. Unknown IDs must fail closed.

## Address and authorization model

Version-0 addresses continue to contain an Ed25519 public key. This representation must remain
valid for historical consensus data.

Version-1 addresses should contain a fixed 32-byte, domain-separated commitment to:

1. the signature scheme ID;
2. the exact encoded public-key lengths;
3. the exact encoded public keys.

The public keys move into the spending authorization rather than the UTXO. Validation recomputes
the commitment before checking every required signature. This keeps outputs and user-facing
addresses compact while allowing large and variable-size post-quantum keys.

The consensus representation must retain the address version. Returning only the 32-byte payload
from Bech32m decoding is insufficient because a validator must know whether an output is an
Ed25519 key or a commitment. The Rust domain model should therefore replace bare address strings
at new protocol boundaries with a typed `(version, payload)` value.

## Transaction identity

Post-quantum signatures must not become transaction identifiers. Hybrid signatures are large and
ML-DSA may produce different valid signatures for the same payload. Version-2 transactions should
use a domain-separated transaction hash over the canonical signed transaction as their fixed-size
ID. Outpoints and indexes must refer to this ID. Legacy signature-based transaction IDs remain
valid for historical transactions.

Fee and block accounting must use actual canonical byte lengths. The current fixed 32-byte public
key and 64-byte signature assumptions must not be applied to version-2 transactions. Mempool,
gossip, JSON, SQLite compaction, block selection, and the one-megabyte block limit all require
boundary tests with maximum-size hybrid authorizations.

## Activation sequence

1. **Crypto agility:** centralize current Ed25519 operations; reserve exact scheme IDs; add typed,
   length-delimited key and signature encodings. This is consensus-neutral.
2. **Read support:** nodes parse version-1 addresses and version-2 transactions but reject them as
   not-yet-active. Unknown versions and schemes fail closed.
3. **Hybrid activation:** at an announced height, permit version-1 outputs and require both
   Ed25519 and ML-DSA-44 signatures when spending them. Keep version-0 spends valid.
4. **Wallet migration:** default all new receive addresses to version 1 and provide one action that
   consolidates every version-0 UTXO into version-1 outputs. Show remaining legacy value in node
   status and the wallet UI.
5. **Legacy sunset:** only after measured migration coverage and extensive notice, stop creating
   version-0 outputs. A later restriction on version-0 spending is a separate consensus decision;
   it can strand funds and cannot distinguish an owner from a quantum attacker.
6. **Classical removal:** removing Ed25519 from hybrid authorization requires a new scheme ID and
   activation. It is not implied by enabling ML-DSA.

All stages must be rehearsed across the height boundary with old and new nodes, snapshot restore,
fork recovery, mempool rebroadcast, compact-store reload, and lightweight-wallet signing.

## Release sequence

Application, transport, and consensus versions move independently:

1. A protocol-v2 application release advertises read capabilities in the optional `capabilities`
   field. Old nodes ignore the field and an omitted field means no advertised capabilities.
2. A later application release ships dormant transaction-v2 and hybrid verification code.
3. After deployment coverage is measured, the complete integration release advertises
   `transaction-v2-blocks` and announces candidate height 3000 as the consensus transition. The
   feature must not be introduced and activated in the same release.
4. Wallet defaults may change after activation without another consensus version. Refusing new
   legacy outputs, changing the VDF, or removing Ed25519 each requires its own later activation.

There is no separate public Iuna testnet. The `iuna-mainnet-candidate` network is the rehearsal
network for this migration. Once hybrid wallet keys and the complete transaction-v2 path are
available, that candidate network may begin value migration at the fixed, reviewed activation
height 3000. This does not turn activation into a runtime flag: nodes restored from old backups
must still deterministically reach the same rule at the same height.

Capability names are sorted, unique, lowercase ASCII tokens. A hello may advertise at most 16
tokens of at most 64 bytes each. These limits are enforced before the handshake is accepted.

### Transaction-v2 activation target

The transaction-v2 binary envelope and its canonical hash identifier are compiled into the node.
Blocks carry canonical lowercase-hex envelopes in a separate `transactions_v2` list so legacy
transaction JSON remains unchanged. Candidate height 3000 is compiled in as the consensus
activation target; it is not an operator-controlled feature flag and cannot be changed through
configuration. Nodes reject v2 mempool and block entries below that height.

The reserved format binds the chain ID and genesis hash, uses typed versioned addresses, stores one
length-delimited authorization per spending input, and hashes the complete canonical signed bytes
for its transaction ID. An explicit migration transaction tags and references legacy 32-byte hash
or 64-byte signature transaction IDs and retains Ed25519 authorization so existing value can move
to a version-1 output. Ordinary v2 transactions use 32-byte hash IDs; every later spend of a
version-1 output requires Ed25519 + ML-DSA-44.
Verification uses the exact-pinned RustCrypto `ml-dsa` 0.1.1 implementation. That implementation
has not been independently audited, so an independent review and an explicit backend acceptance
decision remain prerequisites for treating the active rules as production-ready. Nodes advertise
the transaction-v2 block capability and relay canonical transaction-v2 envelopes only to peers
that advertise the separate `transaction-v2-mempool` capability. This keeps 0.4.35 block-validating
peers connected during a gradual relay upgrade. The management
wallet can submit reviewed migration batches and ordinary hybrid transfers; address rotation and
broader recovery rehearsal remain release blockers.

Pending and confirmed transaction-v2 entries are included in the management wallet history and
chain views. Confirmed history is materialized from the canonical chain snapshot, so a chain
reorganization atomically replaces entries from the abandoned branch.
Pending transaction-v2 envelopes are journaled in the chain database before a wallet migration
reports durable success; a storage failure is surfaced separately from broadcast failure. On
restart they are decoded and validated against the restored
canonical chain before re-entering the mempool and normal periodic rebroadcast path. Mining,
reorganization, invalidation, and an explicit chain reset reconcile or clear the journal instead
of blindly replaying stale spends.

The verification tests include a small audit corpus pinned to exact NIST ACVP-Server and C2SP
Wycheproof commits and file hashes. It covers a valid NIST signature, Wycheproof's repeated-hint
regression, and a valid signature at the ML-DSA-44 norm boundary. A dedicated fuzz target exercises
both the transaction-v2 decoder and arbitrary ML-DSA-44 verification inputs; it remains part of the
release's time-bounded, coverage-guided `cargo fuzz` gate while wallet submission is gated. Seed
corpora, newly discovered coverage inputs, and crash artifacts are retained as release evidence.

### Hybrid wallet keys

The existing wallet seed phrase now deterministically derives a separate ML-DSA-44 seed using the
fixed `iuna-wallet-ml-dsa44-seed-v1` domain. The original Ed25519 derivation is unchanged, so
existing addresses, encrypted wallet files, and backups remain valid. The wallet can construct an
address-v1 commitment and create an Ed25519 + ML-DSA-44 authorization over one byte-identical
payload. ML-DSA secret intermediates use the backend's zeroization support.

This key capability alone does not create spendable address-v1 outputs. The wallet UI must not
offer the address until transaction-v2 submission, mempool, block, gossip, persistence, and fee
accounting are connected and activated together on the candidate network.

The domain layer now has a separate v2 pending pool. It checks the fixed height boundary, the
ledger-derived chain domain, canonical encoded byte limits, UTXO ownership, value conservation,
legacy/v2 double-spends, and dependent v2 transactions. At and after height 3000, block selection
can include those transactions; their exact envelopes are committed by the VDF seed and block
hash, counted against the shared transaction and byte limits, applied with UTXO lineage, persisted
in compact snapshot v8, and carried forward after reorgs. Snapshot v7 remains readable. Blocks
therefore propagate through the existing block P2P path. Standalone v2 mempool gossip now accepts,
validates, canonicalizes, rebroadcasts, and periodically re-announces v2 envelopes. The management
wallet reports legacy and hybrid balances, exposes an authenticated migration preview, submits one
reviewed block-bounded migration batch at a time, and can spend confirmed hybrid value to another
address-v1 recipient. Automatic address rotation and full recovery rehearsal remain incomplete.

## Other trust boundaries

- P2P node IDs need versioned, algorithm-tagged proofs independent of wallet activation.
- CLI and desktop update verification need dual classical/post-quantum signatures and an update
  path that installs the new trust root before it becomes mandatory.
- TLS and deployment credentials need a separate cryptographic inventory; they are not consensus
  rules.
- Wallet files should move from PBKDF2-SHA256 to a versioned memory-hard password KDF as general
  hardening. ChaCha20-Poly1305 with a 256-bit key does not need immediate replacement.

## VDF gate

The current `classgroup-wesolowski-bqfc-v1` format remains frozen for historical verification. A
replacement VDF needs its own format identifier, activation height, security argument, reference
implementation, known-answer vectors, performance measurements, and fork-choice simulations.
No candidate should be described as post-quantum merely because it avoids RSA setup.

## Release gate

Iuna must not claim quantum resistance until all of the following are true:

- the active transaction and consensus signature path is independently reviewed;
- existing value has an operational migration path and migration telemetry;
- peer and updater authentication have post-quantum transition paths;
- the VDF has a documented quantum threat model;
- adversarial and six-node tests cover every activation boundary;
- recovery playbooks cover a failed or rolled-back activation.
