# Operator Failure Playbooks

These playbooks are for testnet and mainnet-candidate operators. They are intentionally practical: identify the failure, preserve useful evidence, recover the node, and avoid making a temporary network issue worse.

Assume the management UI is bound to `127.0.0.1` and the public P2P listener is the only internet-facing service. The default data directory is `~/.iuna`; the default chain database is `~/.iuna/chain.sqlite3`; the default config file is `~/.iuna/config.json`.

## First Checks

Before changing state, capture the local view:

```sh
curl -s http://127.0.0.1:18661/api/status
curl -s http://127.0.0.1:18661/api/network/health
curl -s http://127.0.0.1:18661/api/peers
curl -s http://127.0.0.1:18661/api/p2p/metrics
```

These API routes require the same authenticated management session as the UI. If `curl` returns an auth response, collect the same data from the local UI or include the browser session cookie in the request.

Record:

- local height and tip hash;
- best known peer height;
- peer count, stale peers, banned peers, and last peer error;
- current leader and `last_auto_finalization_status`;
- whether the wallet is locked;
- P2P listener, configured announce address, and whether inbound P2P is active.

If the node runs with non-default paths, get them from startup logs or flags:

```sh
iuna --help
```

The process prints the wallet file, config file, chain database, management UI, and P2P listener on startup.

## Legacy address-book migration

The upgraded wallet shows checksummed Bech32m receive addresses: `iuna1...` for
mainnet/mainnet-candidate and `tiuna1...` for local testnet. Transfer,
address-book, and Stratum input no longer accepts a 64-character hex public key.
This is intentional: silently accepting hex would bypass typo and
wrong-network protection.

Before genesis or before reusing an old contact, ask the recipient to copy a new
address from their upgraded wallet and replace the old address-book entry.
Never change `iuna` to `tiuna` (or the reverse) by hand; the checksum covers the
prefix. Existing wallet files and chain databases retain their internal hex
keys and must not be rewritten. Controlled migration tools may call the domain
`migrate_legacy_address` function only when the target network is explicitly
known.

No block-height activation is needed because on-chain and P2P address bytes do
not change. Treat this as a client compatibility rollout instead: deploy the
wallet UI and backend together, have recipients publish their new display
address, and update Stratum worker usernames before restarting miners.

## Stalled Height

Symptoms:

- local height does not increase for longer than the target block time;
- `/api/network/health` reports `isolated`, `stale`, `peer errors`, or `syncing`;
- `last_auto_finalization_status` repeatedly says the wallet is locked, automatic mining is off, collecting burns, waiting for another finalizer, or VDF work is running too long.

Checks:

1. Compare local height with peers in `/api/network/health`.
2. Check `/api/peers` for recent successful contact and peer heights.
3. Check `/api/status` for wallet lock state, current leader, automatic mining, burn amount, VDF rounds, and recovery VDF threshold.
4. Check host clock synchronization. Blocks too far in the future are rejected and peer clock warnings appear in network health.

Recovery:

- If the node has no healthy peers, add a known bootnode in the Peers screen or restart with `--join <addr:port>` only when there is no local chain database.
- If the wallet is locked and this node is expected to finalize, unlock it in the UI.
- If automatic mining is off and this node should participate, enable Mine settings and set a fee-paying burn policy.
- If every healthy peer is at the same height, wait through fallback and recovery windows before resetting anything. A recovery block is the expected liveness escape hatch when ticket finalizers fail.
- If peers are ahead, keep the node online and let range sync catch up. Use local chain reset only if the node remains stuck after it has healthy peers.

Avoid:

- deleting the wallet file;
- repeatedly resetting multiple public nodes at once;
- starting a new genesis while the network still has a valid chain.

## Divergent Tips

Symptoms:

- local height matches peers but tip hash differs;
- peers report different tips at the same or nearby heights;
- logs mention fork, snapshot, orphan, or block validation errors.

Checks:

1. Compare `/api/status` tip hash and height across at least two healthy peers.
2. Inspect recent blocks in the UI or `/api/blocks`.
3. Check `/api/p2p/metrics` for parse errors, session failures, and data envelope counts.
4. Look for repeated snapshot requests in logs. Nodes request snapshots when a peer appears to be on a better valid fork.

Recovery:

- If the divergence is inside the finality window, keep nodes connected. The protocol can reorg to a taller valid fork or to a same-height fork with better leader quality.
- If your node is behind a healthy majority, add direct peers to that majority and let snapshot/range sync resolve it.
- If your node is alone on an old tip and does not converge, use Delete local chain in Settings. That clears local chain/UI data, keeps wallet and settings, and broadcasts a snapshot request to peers.
- If multiple public nodes disagree beyond the finality window, stop automated restarts and preserve chain databases from both sides for analysis.

Avoid:

- forcing a manual database replacement from an untrusted peer;
- resetting the apparent majority tip before confirming it validates on more than one node.

## Old Snapshots Or Long Catch-Up

Symptoms:

- a node starts from an old persisted height;
- `/api/network/health` reports `syncing` with peer height ahead;
- startup logs say the node resumed a chain database at an old height.

Checks:

1. Confirm the node has at least one healthy outbound or inbound peer.
2. Confirm peers know a higher height.
3. Check `/api/p2p/metrics` for outbound connect failures or queue pressure.
4. Confirm the configured P2P announce address is reachable from outside if the node is meant to be public.

Recovery:

- Leave the node online while it requests block ranges and snapshots from peers.
- Add one or two stable peers manually if peer exchange has not found healthy peers.
- Restart the process once if sessions are stale but the chain database loads successfully.
- Use Delete local chain only if the node cannot extend its local snapshot and healthy peers are available. This does not remove wallet or settings.

Avoid:

- deleting the chain database before capturing the startup error;
- using `--genesis` to recover an old node. `--genesis` is only for creating a fresh network.

## Coordinated Snapshot V6 Reset

The next release accepts compact local snapshot format v6 only and does not migrate earlier chain history. This is a planned consensus/network reset, not a corrupted-database incident. At startup, the node detects the legacy JSON schema and compact snapshot versions older than v6, checkpoints the database, archives it as `chain.sqlite3.pre-v6` (or the next available numbered suffix), and creates a fresh v6 database. The UI cache is then cleared normally. Wallet and configuration files are left untouched.

All operators must still agree on the release, genesis, network identity, bootnodes, and start time before bringing public nodes back online. Automatic local archiving does not coordinate genesis.

Before upgrading:

1. Stop the node and back up `wallet.json` and `config.json`; verify that the backups are readable.
2. Start exactly one designated node with a fresh chain database and `--genesis`, record its genesis hash, and publish that hash with the release commit and checksums.
3. Start every other node without `--genesis` and join a trusted published bootnode. Its incompatible chain database is archived automatically.
4. Preserve the generated `.pre-v6` archive if the old chain is needed as historical evidence.

Manual equivalent for operators who want to choose the archive names before starting:

```sh
mv ~/.iuna/chain.sqlite3 ~/.iuna/chain.sqlite3.pre-v6 2>/dev/null || true
mv ~/.iuna/ui_data.sqlite3 ~/.iuna/ui_data.sqlite3.pre-v6 2>/dev/null || true
iuna --join <trusted-peer-addr:port>
```

For the disposable Compose testnet, `docker compose down -v` removes all volumes and the configured bootstrap creates the fresh genesis on the next start.

Avoid:

- running multiple independent `--genesis` nodes;
- copying an old snapshot blob into a v6 database;
- deleting or replacing wallets as part of the chain reset;
- starting before the published genesis hash and release checksum are available.

## Height 1000 Consensus Upgrade

Height `1000` is a coordinated consensus activation. At that height, VDF seeds
start committing to block content and ticket draws stop using the final block
hash. Transaction signatures and native and Stratum mine proofs also switch to
chain-bound binary format v1. This preserves blocks, snapshots, and UTXOs below
`1000`, but nodes running the earlier rule will reject the upgraded chain or
build an incompatible fork at activation. No database reset, new genesis, or
migration command is needed for this height activation.

Before height `1000`:

1. Publish a tagged release, commit, checksums, and the activation height.
2. Upgrade every known public peer, finalizer, and bootstrap node.
3. Verify the reported package version on each managed node and compare tips.
4. Stop or isolate nodes that cannot be upgraded before activation.
5. Keep chain database backups from immediately before the activation window.

At and after height `1000`, compare height and tip hash across at least three
independent nodes. If upgraded nodes disagree, preserve both histories and stop
automated restarts; do not reset the apparent majority until both forks have
been validated.

## No Burn Committee Signatures

Symptoms:

- `last_auto_finalization_status` stays on collecting burns or says a block has too few burn bundle signatures;
- there are fee-paying burns in mempools but rank `0` blocks do not finalize;
- fallback blocks start appearing more often than expected.

Checks:

1. Check `/api/mempool` for fee-paying burn transactions.
2. Check `/api/peers` for enough healthy peers. Burn bundles are gossip; finalizers request missing burn-bundle slots during collection, but isolated nodes still cannot receive responses.
3. Check whether committee members are online and unlocked if they are expected to sign bundles.
4. Inspect recent blocks: rank `0` needs the strictest burn-list attestation threshold; fallback ranks relax it for liveness.

Recovery:

- Improve peer connectivity first. Add stable peers and keep committee-capable nodes online.
- Make sure automatic mining is enabled and wallets that should finalize/sign are unlocked.
- Wait through fallback windows. Missing committee signatures should not permanently halt the chain; fallback ranks need fewer attestations.
- If the network reaches recovery blocks, treat it as a warning that normal ticket finalization or committee gossip is unhealthy.

Avoid:

- treating local mempool contents as proof that a remote block is invalid. Block validation must be independent of each validator's mempool.
- resetting a node merely because it did not personally see a burn bundle.

## Recovery Blocks

Symptoms:

- recent blocks show finalizer mode `Recovery`;
- normal ticket blocks stopped for about the recovery delay;
- `last_auto_finalization_status` mentions recovery VDF.

Checks:

1. Confirm whether recovery blocks are accepted by multiple peers.
2. Check if selected ticket finalizers were offline, locked, not burning, or missing committee attestations.
3. Check clock warnings. Large clock skew can cause valid-looking local candidates to be rejected by peers.
4. Check VDF rounds and host performance if VDF work consistently finishes late.

Recovery:

- If recovery blocks are converging across peers, do not reset. Recovery is part of the liveness design.
- Bring ticket finalizer nodes back online and unlocked.
- Keep recovery VDF threshold conservative on low-power hosts if too many nodes are wasting work on fallback/recovery paths.
- After recovery, watch the next normal ticket blocks. Persistent recovery means the normal ticket path needs investigation.

Avoid:

- banning peers only because they produced a recovery block;
- assuming recovery is an emergency fork. It is valid, weaker-for-fairness liveness behavior.

## Corrupted Local Persistence

Symptoms:

- startup fails with `failed to load chain database`;
- SQLite reports it cannot open or parse `chain.sqlite3`;
- the UI loads but chain views are empty or inconsistent while peers are healthy.

Checks:

1. Preserve the files before recovery:

```sh
cp ~/.iuna/chain.sqlite3 ~/.iuna/chain.sqlite3.bak 2>/dev/null || true
cp ~/.iuna/ui_data.sqlite3 ~/.iuna/ui_data.sqlite3.bak 2>/dev/null || true
```

2. Keep `wallet.json` and `config.json`; they are not chain state.
3. Confirm at least one trusted peer is available before deleting local chain state.

Recovery:

- Preferred: use Settings, Delete local chain. This clears local chain/UI data, keeps wallet and settings, and asks peers for a snapshot.
- CLI fallback: stop the node, move only the chain/UI data files aside, then restart with the same wallet/config and healthy peers:

```sh
mv ~/.iuna/chain.sqlite3 ~/.iuna/chain.sqlite3.corrupt 2>/dev/null || true
mv ~/.iuna/ui_data.sqlite3 ~/.iuna/ui_data.sqlite3.corrupt 2>/dev/null || true
iuna
```

- If starting from an empty chain database, join a known peer:

```sh
iuna --join <trusted-peer-addr:port>
```

Avoid:

- deleting `wallet.json`;
- editing SQLite files in place;
- accepting a snapshot from an unknown or isolated peer as the only recovery source.

## Escalation Bundle

When a problem needs developer investigation, collect:

- command line and version;
- startup log lines showing wallet/config/chain paths and listeners;
- `/api/status`, `/api/network/health`, `/api/peers`, and `/api/p2p/metrics`;
- the last 20 blocks from `/api/blocks?limit=20`;
- whether the wallet was locked and whether automatic mining, P2P inbound, and Stratum were enabled;
- copies of corrupted chain/UI databases if persistence failed.

Do not include recovery phrases, wallet passwords, or private keys in reports.
