# Mainnet-Candidate Attack Testing Plan

This plan tracks the remaining security and economics testing work before the
candidate chain is treated as promotable to mainnet.

## 1. Fuzzing Harnesses

Status: initial repository harnesses and deployment smoke runs are in place.

Goal: continuously throw malformed and semi-valid input at every external data
boundary and every compact persistence format.

Initial targets:

- P2P gossip envelopes and item-limit validation.
- Compact chain snapshot decoder.
- Transaction, block, burn bundle, and chain snapshot JSON decoding.
- Stratum JSON requests and line framing.
- Wallet/config persistence decoding.

Acceptance:

- Fuzz targets live in the repo and can be run locally.
- Seed corpora include valid examples for every public message family.
- Crashes, panics, unbounded allocation, and excessive parser time are failures.
- Release candidates run at least a short fuzz smoke test; longer fuzzing can run
  on a dedicated machine.

## 2. Independent Mini-Validator

Status: started with a test-only block precheck oracle for height, parent hash,
block hash, reward, VDF rounds, timestamps, block limits, burn presence, fee
policy, ticket finalizer proof selection, snapshot supply accounting, and ticket
lifecycle reconstruction. The oracle now also reconstructs burn committee
lineage selection, burn bundle quorum rules, and fork-choice finality decisions.

Goal: compare the production validator against a small, deliberately separate
oracle for consensus-critical facts.

Scope:

- block size and transaction count limits;
- supply accounting;
- ticket maturity, expiry, and consumption;
- burn committee membership and quorum;
- fork-choice finality constraints.

Acceptance:

- Generated chains are accepted by both implementations.
- Mutated invalid blocks are rejected by both implementations for the same class
  of reason.

## 3. Economic Sweep Runner

Status: started with a deterministic in-process sweep runner that maps attack
dimensions to adversarial strategies and classifies observed censorship as
unavailable, network-isolation dependent, fee-pressure dependent, or
finalizer-disruption dependent. Fee pressure, gossip latency, peer isolation, and
offline finalizer schedules are now modeled as runner inputs instead of only as
strategy labels.

Goal: quantify whether attacks are impossible, only possible under network
isolation, or possible but expensive.

Sweep dimensions:

- attacker burn weight;
- attacker matured lineage weight;
- peer isolation percentage;
- gossip latency;
- offline finalizer rate;
- fee pressure and blockspace fill.

Metrics:

- attacker finalization share;
- attacker committee share;
- third-party burn censorship rate;
- cost per censored burn;
- fallback and recovery rate;
- attacker net reward or loss.

## 4. Performance Budgets

Status: started with deterministic tests for consensus block count/byte caps,
burn bundle 10kB selection, snapshot replay, and blockspace-flood bounded
mempool selection.

Goal: make valid input DoS visible before launch.

Budgets:

- max-size block validation;
- max mempool block selection;
- max burn bundle processing;
- snapshot import/export;
- P2P batch parsing;
- Stratum request handling.

Acceptance:

- Tests assert upper bounds on item counts and bytes.
- Benchmark or timing tests produce repeatable local numbers that can be tracked
  before release.

## 5. Eclipse And Partition Chaos

Status: started with deterministic tests for delayed burn-bundle import across
partitions, late burn gossip deduplication after reconnect, and rejecting a
shorter attacker-only fork before recovering to a better majority tip.

Goal: ensure isolated or stale nodes reject bad histories and recover cleanly.

Scenarios:

- stale node receives old snapshots and invalid late blocks;
- attacker-only peers feed a minority fork;
- burn bundles are delayed across partitions;
- reconnect after recovery/fallback events.

Acceptance:

- invalid chains are not adopted;
- valid longer/better chains inside finality are adopted;
- old nodes catch up without manual database deletion.

## 6. Crash Consistency

Status: started with atomic JSON writes for config and wallet files, stale
temp-file regression coverage, and rollback tests that keep the last committed
chain snapshot/UI projection after failed persistence work.

Goal: prove local persistence survives process death at bad moments.

Scenarios:

- crash during block apply;
- crash during chain snapshot save;
- crash during wallet/config write;
- restart after partial UI index update;
- restart after local reset request.

Acceptance:

- node either resumes the last committed chain or reports a clear corruption
  error without silently replacing valid state.

## 7. Candidate Promotion Rehearsal

Status: started with explicit candidate/mainnet peer-network IDs and a restart
rehearsal that preserves the candidate genesis, tip, launch profile, UTXOs,
ticket ranks, and transfer history before mining the next promoted block.

Goal: prove candidate-to-mainnet promotion preserves coins.

Scenario:

- run a candidate chain;
- mine and transfer coins;
- upgrade to a release that fences peers with `iuna-mainnet-v1`;
- keep the same chain database and launch profile;
- continue mining from the existing tip.

Acceptance:

- genesis hash is unchanged;
- UTXOs and tickets are unchanged before the first promoted block;
- old candidate peers are rejected by network ID;
- upgraded nodes continue from the same chain tip.
