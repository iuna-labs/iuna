# Roadmap

iuna is currently an experimental cryptocurrency devnet. This roadmap is the canonical planning document for moving from devnet/testnet hardening toward a mainnet candidate and, if the candidate stays healthy, promoting that same chain to mainnet.

## Current Phase

Testnet hardening.

The current goal is to keep a small real testnet stable while increasing confidence in consensus, sync, recovery, fork choice, transaction handling, and release operations.

## Mainnet Readiness Checklist

- [x] Protocol rules are frozen for mainnet candidate.
- [x] Block, transaction, ticket, VDF, recovery, fork-choice, and peer compatibility rules are documented.
- [ ] Long-running testnet has stayed stable with independent nodes for an agreed window.
- [ ] Mainnet-candidate network has launched from a fresh genesis using release artifacts.
- [ ] New nodes can sync from genesis without manual intervention.
- [ ] Stale nodes can reconnect and catch up from old snapshots/range sync.
- [ ] Network partitions heal according to fork choice.
- [ ] Recovery blocks restore liveness when selected finalizers disappear.
- [ ] Multiple recovery candidates converge safely.
- [ ] Clock skew and future timestamp cases do not stall the network.
- [ ] Blinded commit/reveal flows survive partitions and delayed gossip.
- [ ] Mempool state remains sane across reorgs.
- [ ] Block selection stays bounded by transaction count and size limits.
- [ ] Long-running chaos/property tests pass in release deployment.
- [ ] Release artifacts are tagged, checksummed, and reproducible enough for testers to verify.
- [ ] Candidate genesis allocation plan, genesis hash, and promotion policy are published and reviewed.
- [ ] Security review is complete for consensus validation, transaction validation, P2P input handling, and wallet/key storage.
- [ ] Upgrade and rollback instructions exist.
- [x] Basic operational monitoring is available for height, tip hash, peers, last block age, finalizer mode, VDF rounds, mempool, and rejected blocks.

## Pre-Candidate Launch Test Backlog

These items are not protocol rules. They are the attack and reliability checks to finish or consciously defer before the planned devnet reset that creates the mainnet-candidate network. If that candidate stays healthy through the agreed window, the same genesis, chain history, UTXOs, and mined coins should be promoted to mainnet instead of being reset again.

### Must Before Candidate Genesis

- [x] Burn bundle relay cannot import embedded burns before bundle metadata, membership, signature, fee ordering, and size are prechecked.
- [x] Block validation with burn attestations remains independent of local mempool contents, including empty and conflicting mempools.
- [x] Post-genesis transactions cannot spend with `genesis` input signatures.
- [x] P2P envelope item limits reject batches only above their configured boundaries.
- [x] Stratum endpoint has explicit DoS limits: maximum line size, maximum jobs per session, idle timeout, and connection/session caps.
- [x] Fork and snapshot adversarial tests cover same-height leader-quality choice, taller valid forks inside finality, invalid late snapshot blocks, and pending transaction carry-forward after reorg.
- [x] Compact snapshot decoder has malformed-input tests for huge lengths, oversized varints, trailing bytes, truncated payloads, invalid tags, and random byte inputs without panics or excessive allocation.
- [x] Supply invariant tests cover mixed burns, fees, PoW mine actions, reorgs, no replay, and no double spend.

### Should Before Mainnet

- [x] HTTP/auth abuse tests cover CSRF same-origin behavior, lockout/backoff behavior, forwarded-header spoofing from untrusted peers, and session expiry.
- [x] Multi-node in-memory simulation covers delayed gossip, withheld burn bundles, bundle equivocation, partitions, restarts, persistence reload, and convergence.
- [x] Long-running release-mode soak test runs with automatic burn/finalization, P2P sync, Stratum-disabled and Stratum-enabled nodes, and periodic node restarts.
- [x] Operator failure playbooks exist for stalled height, divergent tips, old snapshots, no burn committee signatures, recovery blocks, and corrupted local persistence.
- [x] Mainnet-candidate release rehearsal includes fresh genesis, published bootnodes, checksums, backup/restore instructions, and a no-reset stability window.

## Milestones

### M1: Testnet Hardening

Focus: make failure modes boring and observable.

- [x] Add property-style simulations for clock skew.
- [x] Add property-style simulations for network partitions and reconnect.
- [x] Add property-style simulations for multiple recovery candidates.
- [x] Add property-style simulations for late joiners syncing from genesis.
- [x] Add property-style simulations for future timestamp rejection.
- [x] Add property-style simulations for reorg mempool preservation.
- [x] Add property-style simulations for blinded commit/reveal under partition.
- [x] Add property-style simulations for expired blinded transaction pruning.
- [x] Add long-running soak chaos test.
- [x] Keep long-running tests out of normal local test runs.
- [x] Run long-running tests unconditionally during deployment.
- [x] Run a multi-day public testnet without manual chain resets.
- [x] Add or improve operator-facing health metrics.
- [x] Document common testnet failure/recovery playbooks.

### M2: Mainnet Candidate

Focus: launch the candidate with mainnet-like process and treat it as the chain that can become mainnet if it stays healthy.

- [x] Freeze protocol parameters for the candidate.
- [ ] Create a fresh mainnet-candidate genesis.
- [ ] Publish bootnodes and release artifacts.
- [ ] Publish checksums for every release artifact.
- [ ] Document node setup, backup, restore, and upgrade steps.
- [ ] Run a candidate network for an agreed stability window.
- [ ] Treat resets as launch-blocking incidents unless explicitly planned.
- [ ] Decide and publish whether the candidate ledger is promoted to mainnet without a second genesis.

### M3: Mainnet Launch

Focus: promote the stable candidate ledger. Mainnet launch should not create a second genesis unless the candidate failed and the reset is explicitly announced.

- [ ] Publish the promotion decision, candidate genesis hash, promoted tip height, and promoted tip hash.
- [ ] Tag the mainnet release from the promoted candidate code line.
- [ ] Publish release artifacts and checksums.
- [ ] Upgrade or restart bootnodes on the mainnet release while preserving chain data.
- [ ] If the P2P network ID changes from `iuna-mainnet-candidate-v1` to `iuna-mainnet-v1`, coordinate the cutover without changing genesis or launch profile rules.
- [ ] Monitor first blocks and first recovery/fallback events.
- [ ] Keep feature changes frozen during the launch window.
- [ ] Document any required hard-fork or emergency procedure before launch.

### M4: Post-Mainnet

Focus: improve usability, tooling, and governance after the base network is stable.

- [ ] Improve wallet UX and backup flows.
- [ ] Improve block explorer and public network visibility.
- [ ] Add safer upgrade prompts or update guidance.
- [ ] Add protocol versioning and hard-fork coordination process.
- [ ] Explore light client or mobile-friendly modes.
- [ ] Use observed network data to tune economic and operational assumptions.

## Release Gates

A release intended for deployment must pass:

- `cargo test --locked`
- `cargo check --locked --manifest-path fuzz/Cargo.toml`
- fuzz smoke runs for `p2p_envelope`, `compact_snapshot`, `domain_json`, and `stratum_request`
- `cargo test --locked --release --test properties -- --ignored`

Normal local development may skip ignored long-running property tests and long fuzzing sessions, but deployment must run the release gate smoke checks.

## Decisions

- 2026-08-14: Keep `ROADMAP.md` in the repo as the source of truth.
- 2026-08-14: Long-running property/soak tests are marked `#[ignore]` for normal local runs and are required in `deployment.sh`.
- 2026-08-19: The mainnet-candidate chain is intended to be promotable to mainnet without a second genesis if it satisfies the stability window and release gates.
