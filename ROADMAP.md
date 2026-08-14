# Roadmap

iuna is currently an experimental cryptocurrency devnet. This roadmap is the canonical planning document for moving from devnet/testnet hardening toward a mainnet candidate and, eventually, a mainnet launch.

## Current Phase

Testnet hardening.

The current goal is to keep a small real testnet stable while increasing confidence in consensus, sync, recovery, fork choice, transaction handling, and release operations.

## Mainnet Readiness Checklist

- [ ] Protocol rules are frozen for mainnet candidate.
- [ ] Block, transaction, ticket, VDF, recovery, fork-choice, and peer compatibility rules are documented.
- [ ] Long-running testnet has stayed stable with independent nodes for an agreed window.
- [ ] Mainnet-candidate testnet has launched from a fresh genesis using release artifacts.
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
- [ ] Genesis allocation plan is published and reviewed.
- [ ] Security review is complete for consensus validation, transaction validation, P2P input handling, and wallet/key storage.
- [ ] Upgrade and rollback instructions exist.
- [ ] Basic operational monitoring is available for height, tip hash, peers, last block age, finalizer mode, VDF rounds, mempool, and rejected blocks.

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
- [ ] Run a multi-day public testnet without manual chain resets.
- [ ] Add or improve operator-facing health metrics.
- [ ] Document common testnet failure/recovery playbooks.

### M2: Mainnet Candidate

Focus: rehearse mainnet with mainnet-like process, but without mainnet permanence.

- [ ] Freeze protocol parameters for the candidate.
- [ ] Create a fresh mainnet-candidate genesis.
- [ ] Publish bootnodes and release artifacts.
- [ ] Publish checksums for every release artifact.
- [ ] Document node setup, backup, restore, and upgrade steps.
- [ ] Run a candidate network for an agreed stability window.
- [ ] Treat resets as launch-blocking incidents unless explicitly planned.

### M3: Mainnet Launch

Focus: launch only after the candidate process has already made launch boring.

- [ ] Publish final genesis plan and genesis hash.
- [ ] Tag the mainnet release.
- [ ] Publish release artifacts and checksums.
- [ ] Start bootnodes.
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
- `cargo test --locked --test properties -- --ignored`

Normal local development may skip ignored long-running property tests, but deployment must not.

## Decisions

- 2026-08-14: Keep `ROADMAP.md` in the repo as the source of truth.
- 2026-08-14: Long-running property/soak tests are marked `#[ignore]` for normal local runs and are required in `deployment.sh`.
