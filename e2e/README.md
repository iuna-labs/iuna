# Docker Compose end-to-end tests

This harness runs the six-node network with a deliberately isolated consensus
profile, `iuna-local-e2e-5s-v1`. Its target block time is 5 seconds, its Docker
subnet is `172.29.0.0/24`, and its management ports are `28661` through `28666`.
The regular local testnet keeps its 10-minute target and can run alongside it.

The e2e binary refuses to start unless `IUNA_LOCAL_TESTNET=true`. Its profile ID
is part of the launch-profile hash, so a 5-second checkpoint cannot be loaded by
the regular local-testnet build.

## Everyday workflow

Start a fresh network:

```sh
./e2e/iuna_e2e.py reset
./e2e/iuna_e2e.py up --build
./e2e/iuna_e2e.py status
```

Start from a committed checkpoint and wait for all nodes to converge:

```sh
./e2e/iuna_e2e.py up --snapshot pre-objective-finality
./e2e/iuna_e2e.py wait 1001 --converge
```

Run the same flow as one test, including log collection on failure and teardown:

```sh
./e2e/iuna_e2e.py smoke \
  --from pre-objective-finality \
  --through 1001
```

Run the complete assertion suite (build the image on the first scenario):

```sh
./e2e/iuna_e2e.py test --build
```

Run the standard post-activation gate used by deployment:

```sh
./e2e/iuna_e2e.py test post-activation --build
```

This verifies the committed checkpoints, crosses 999 through 1001, then restores
the first objective checkpoint, advances the restarted network through 1007,
and runs a physical 3-3 P2P partition/recovery scenario.

Tests can also be selected individually:

```sh
./e2e/iuna_e2e.py test snapshots
./e2e/iuna_e2e.py test fallback-activation
./e2e/iuna_e2e.py test objective-finality
./e2e/iuna_e2e.py test checkpoint-restart
./e2e/iuna_e2e.py test partition-recovery
```

Preserve a machine-readable phase report and complete container logs:

```sh
./e2e/iuna_e2e.py test partition-recovery \
  --evidence-dir release-evidence
```

`snapshots` is fast and does not need Docker. It verifies checksums, manifest
metadata, the six stored chain databases, canonical heights and tips, and the
profile of every committed checkpoint. The network scenarios restore the
checkpoint before a protocol boundary and assert six-node convergence, the
isolated e2e profile, finality state, canonical block identity, and the read-only
blocks, network-health, peers, mempool, and wallet APIs on every node.

`fallback-activation` crosses height 300 and checks that objective finality has
not activated early. `objective-finality` crosses heights 1000 and 1001 and
requires a certified checkpoint. `checkpoint-restart` proves that the certified
checkpoint survives a full six-node restart, advances through height 1007, and
that block 1007 is finalized by a ticket derived from a burn included at height
1002 or later. At that height, the ticket maturity and expiry windows exclude
every pre-pipeline burn, so this covers a complete post-activation burn-to-block
lifecycle.

`partition-recovery` gives the e2e containers network-administration capability
and installs temporary firewall rules that split the six real node processes
into two connected groups of three. It requires both islands to converge
internally on different tips containing new recovery blocks, removes the rules,
requires six-node convergence, restarts node6 with its existing data, and waits
for a new rank-0 ticket block that finalizes at or beyond the canonical recovery
height. The disposable scenario deterministically selects one sampled recovery
worker per island and tip
while leaving the other island members out of the recovery race. It restores the
normal 50% configuration before healing, avoiding both a recovery-less small
island and unrestricted fallback-ticket production.

Each evidence run is stored in a timestamped directory with `report.json` and
`nodes.log`. The report records the base Git revision, dirty-worktree flag, an
exact SHA-256 fingerprint of all tracked working-tree contents, per-phase node
tips and checkpoints, both partition recovery heights, the canonical recovery
block, the restarted service, and the resumed rank-0 ticket. The tree fingerprint
also identifies the tested state while deployment has staged version changes
that are committed and tagged only after all gates pass.
It is updated after every completed phase so a failed run remains useful. Raw
node logs contain public node/wallet addresses but no configuration files,
passwords, wallet ciphertext, or recovery phrases. `deployment.sh` enables this
automatically under `release-evidence/`; set `IUNA_RELEASE_EVIDENCE_DIR` to copy
release evidence to another retained location.

Use `--keep` on `smoke` to leave a failed or successful network running. Stop a
network without deleting its mutable data with:

```sh
./e2e/iuna_e2e.py down
```

Runtime data lives in `e2e/.runtime` and is ignored by Git. Set
`IUNA_E2E_RUNTIME_DIR`, `IUNA_E2E_SNAPSHOTS_DIR`, `IUNA_E2E_PROJECT`, or the
`IUNA_E2E_*_PORT` variables when a test needs independent paths or ports.
`reset` only removes the six service directories beneath that configured runtime
directory; committed checkpoints are never touched.

## Building mature checkpoints

`checkpoints.json` records the protocol boundaries worth preserving:

- 299 and 300: fallback-ticket invalidation;
- 999 and 1000: signing v1, replay protection, grinding resistance, and
  objective-finality activation;
- 1001: the first block that can certify checkpoint 1000.

Generate all missing checkpoints during one continuous run:

```sh
./e2e/iuna_e2e.py reset
./e2e/iuna_e2e.py up --build
./e2e/iuna_e2e.py capture-plan
```

At five seconds per block, reaching height 1001 takes roughly 84 minutes plus
startup and synchronization overhead. This is a one-time fixture-building job;
normal tests restore the resulting checkpoints in seconds. Capture one custom
boundary with:

```sh
./e2e/iuna_e2e.py capture pre-objective-finality --height 999
```

Capture waits until the bootstrap SQLite checkpoint reaches the requested
height, then pauses all containers before copying anything. An e2e-only helper
materializes the canonical compact chain at the exact requested height, even if
the live chain advanced across that height between polls. That canonical chain
database and its prebuilt UI projection are paired with every node's encrypted
test wallet and configuration.
Keeping all six identities matters because their mature tickets and committee
lineage must remain usable after restore. Persisting the derived UI database
keeps mature-checkpoint startup fast. SQLite's backup API removes WAL dependence,
a manifest records the live source height plus every exact node height and tip,
and SHA-256 checksums are verified before restore.

Snapshots use the intentionally public Compose password `testtesttest` unless
`IUNA_TESTNET_PASSWORD` was set while they were generated. They are disposable
test credentials only. Use the same password when restoring a snapshot.

Commit reviewed snapshot directories under `e2e/snapshots/`. Do not commit the
mutable `e2e/.runtime` directory.
