# Pre-mainnet security audit — 2026-09-09

Scope: repository revision `3f9e3c7` before the changes in this report. Testing
was local-only. The review prioritized attacks with an estimated cash cost below
EUR 10,000 and covered consensus validation, transaction admission, P2P,
Stratum, bootstrap trust, wallet storage, and HTTP authentication.

This is a focused implementation audit, not a proof of cryptographic or
economic security. The custom VDF and the burn/finality mechanism still require
independent specialist review before promotion.

## Executive result

Two remotely reachable denial-of-service weaknesses were confirmed and patched.
Two cheap admission/bootstrap risks remain promotion blockers or hardening work.
No confirmed supply-creation, signature-bypass, or consensus-split defect was
found in the reviewed validation paths.

| ID | Severity | Estimated attacker cost | Result |
| --- | --- | ---: | --- |
| IUNA-2026-001 | High | Effectively EUR 0 from one public IP | Patched |
| IUNA-2026-002 | High over chain lifetime | A basic host and bandwidth; well below EUR 100/month | Patched |
| IUNA-2026-003 | High during first sync | Below EUR 10,000 when DNS/routing/bootstrap access is available | Patched |
| IUNA-2026-004 | Medium | 10,000 minimum-fee transactions; protocol value likely far below EUR 10,000 | Patched |

## IUNA-2026-001 — one source could exhaust every Stratum session

The public Stratum listener used a global 64-permit semaphore but no per-source
limit. `mining.authorize` is intentionally permissionless, and even an entirely
silent TCP client retained a permit for up to the 120-second idle timeout. A
single machine could therefore open 64 connections and prevent all miners from
connecting. A client that stopped reading responses could retain a task longer
because response writes had no timeout.

Impact: loss of public mining availability and a practical reduction in PoW
issuance participation. This does not directly create coins or alter consensus.

Reproduction before the patch: instantiate `StratumSessionLimiter::new(64)` and
acquire all permits using the same source address; all 64 acquisitions succeed.
The regression tests now show that only four sessions are accepted for one IPv4
address or IPv6 /64 while other sources retain capacity:

```sh
cargo test --locked stratum_session_limiter
```

Patch:

- enforce four active sessions per IPv4 address or IPv6 /64 in addition to the
  global limit;
- release both counters through an RAII permit;
- bound response writes to ten seconds.

Residual risk: sixteen independently routed source groups can still consume the
global limit. Public deployments should add upstream connection/rate limiting.

## IUNA-2026-002 — invalid mine shares amplified into historical chain scans

Each mine transaction validation called
`mine_difficulty_bits_for_anchor_height`. That function iterated over every
retarget window and scanned the complete chain again for each window. At chain
height `H` the work was approximately `H * floor(H / 10)` block visits per
share. At height 100,000 that is about one billion predicate visits. Stratum
performed this work while holding the global node mutex, before rejecting an
invalid share. Any remote client could authorize with a syntactically valid
wallet address and repeatedly submit arbitrary nonces.

Impact: growing CPU exhaustion and starvation of block, gossip, wallet, and UI
operations. Exploit cost is only a network connection and request bandwidth.

Regression test:

```sh
cargo test --locked applied_window_cache_preserves_retarget_results_for_constant_time_lookup
```

Patch: maintain the derived difficulty after every completed ten-block window.
Normal mine validation is now an indexed lookup. Cache updates examine only the
latest ten blocks and snapshot restoration reconstructs the same cache while
replaying blocks. A linear compatibility fallback exists only for synthetic or
migration-created ledgers whose cache is absent.

The patch changes no consensus value or serialized chain format.

## IUNA-2026-003 — first sync trusts an unpinned bootstrap genesis

A setup-placeholder node accepts any self-consistent genesis and launch profile
served by its selected bootstrap peer. The network ID is checked, but the live
candidate genesis is not pinned in code or required through an independently
supplied checkpoint. P2P node identity proves continuity of the peer's
self-generated key; it does not establish that the key is an authorized
candidate bootstrap identity.

Impact: a new operator whose DNS, route, configuration, or only bootstrap peer
is controlled can be placed on a valid but attacker-created chain. Subsequent
same-genesis validation will keep that node isolated from the real candidate.

The production database and repository manifest both record genesis
`3d677cd7ced1c04d3a276cbee7ea38076e34ac65f18a2c9b8286a4872d986a9a`, but the
manifest still describes v0.4.18 while this audit targets v0.4.28.

Patch:

- pin the production genesis in the mainnet-candidate binary;
- reject a mismatched genesis during direct join, setup-placeholder bootstrap,
  and persisted candidate-chain startup;
- prevent `--genesis` from creating a second mainnet-candidate chain while
  leaving local testnet genesis creation available.

Residual hardening before promotion: publish and sign an updated candidate
manifest, pin a finalized checkpoint, and pin bootstrap identities or obtain
the checkpoint from at least two independently operated sources.

## IUNA-2026-004 — full mempool rejects higher-fee transactions

The mempool accepts any non-zero fee. At 10,000 entries it rejects every new
transaction before comparing fee rate or evicting lower-value entries. An
attacker with confirmed funds can create a long sequence of minimum-fee
transactions and pin admission until blocks drain the pool. The 8 MiB byte cap
bounds memory, but not admission fairness or repeated validation cost.

Impact: delayed transaction propagation and local CPU load. Consensus remains
valid and directly connected block producers can still include transactions.

Patch: once the count or byte bound is reached, the lowest-fee-rate independent
package becomes the dynamic relay floor. A candidate is admitted only when its
fee rate is strictly higher; eviction removes the package's pending and orphan
descendants atomically and recalculates both byte counters. Ordinary low-fee
admission and block consensus remain compatible. Regression tests prove that a
full pool accepts a higher-fee independent transaction, descendants are removed
as one package, and the byte/count counters remain exact.

## Verification evidence

The focused regression tests passed:

```text
stratum_session_limiter: 3 passed
applied_window_cache_preserves_retarget_results_for_constant_time_lookup: 1 passed
cargo clippy --locked --all-targets --all-features -- -D warnings: passed
```

The initial sandboxed run completed 330 tests successfully; its 15 socket-based
P2P tests could not call `bind(2)`. The suite was then repeated locally with
loopback permission and passed completely: 432 passed, 0 failed, and 36
long-running tests were ignored by their existing configuration.

`scripts/check-dependencies.sh` could not refresh RustSec because the sandbox
made the Cargo advisory database lock path read-only. Dependency audit status is
therefore not claimed by this report.

## Promotion recommendation

Do not promote to mainnet until the complete release gate is run on the exact
final revision, the candidate manifest is updated, and the custom VDF plus
economic finality assumptions receive independent review.
