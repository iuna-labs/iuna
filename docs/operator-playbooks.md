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

- Before height `1000`, if the divergence is inside the six-block legacy finality window, keep nodes connected. The protocol can reorg to a taller valid fork or to a same-height fork with better leader quality.
- From height `1000`, compare `finalized_height` and `finalized_hash` in `/api/status` or `/api/network/health`. Keep nodes connected when one valid chain has a higher checkpoint: fork choice adopts it even if its tip is temporarily shorter.
- If your node is behind a healthy majority, add direct peers to that majority and let snapshot/range sync resolve it.
- If your node is alone on an old tip and does not converge, use Delete local chain in Settings. That clears local chain/UI data, keeps wallet and settings, and broadcasts a snapshot request to peers.
- If multiple public nodes report conflicting hashes at the same finalized height, preserve both databases and signing evidence immediately. Nodes deterministically choose the lower checkpoint hash, so operators do not need to trust the first peer seen, but the conflict is a quorum safety incident and must be investigated before resuming release activity.

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

## Legacy Pre-v6 Reset

The live candidate release writes compact snapshot v7 and accepts snapshot
versions v6 and v7. At startup, the node detects the legacy JSON schema and
compact snapshot versions older than v6, checkpoints the database, archives it
as `chain.sqlite3.pre-v6` (or the next available numbered suffix), and creates a
fresh database. The UI cache is then cleared normally. Wallet and configuration
files are left untouched.

The coordinated reset that created the live mainnet-candidate genesis is
complete. A current operator encountering a pre-v6 archive should join the
published candidate bootnode without `--genesis`; creating another genesis would
create a separate, incompatible network. Preserve the generated `.pre-v6`
archive if the old chain is needed as historical evidence.

Manual equivalent for operators who want to choose the archive names before starting:

```sh
mv ~/.iuna/chain.sqlite3 ~/.iuna/chain.sqlite3.pre-v6 2>/dev/null || true
mv ~/.iuna/ui_data.sqlite3 ~/.iuna/ui_data.sqlite3.pre-v6 2>/dev/null || true
iuna --join <trusted-peer-addr:port>
```

For the disposable Compose testnet, `docker compose down -v` removes all volumes and the configured bootstrap creates the fresh genesis on the next start.

Avoid:

- running multiple independent `--genesis` nodes;
- using `--genesis` to join or recover the live mainnet candidate;
- copying an old snapshot blob into the current chain database;
- deleting or replacing wallets as part of the chain reset;
- joining a peer before checking the published network identity and release checksum.

## Height 1000 Consensus Activation

Height `1000` is a coordinated consensus activation. At that height, VDF seeds
start committing to block content and ticket draws stop using the final block
hash. Burn-committee lineage draws make the same switch, closing the timestamp/
block-hash grinding path for both leader and committee selection. Transaction
signatures and native and Stratum mine proofs also switch to
chain-bound binary format v1. Rank `0` blocks also start requiring a strict
two-thirds committee quorum so their next rank `0` child can objectively certify
them. This preserves blocks, snapshots, and UTXOs below
`1000`, but nodes running the earlier rule will reject the upgraded chain or
build an incompatible fork at activation. No database reset, new genesis, or
migration command is needed for this height activation.

Required activation procedure:

1. Publish a tagged release, commit, checksums, and the activation height.
2. Upgrade every known public peer, finalizer, and bootstrap node.
3. Verify the reported package version on each managed node and compare tips.
4. Confirm every independent node agrees on the block hash at height `999`;
   upgraded nodes freeze pre-activation history once they reach `1000`.
5. Stop or isolate nodes that cannot be upgraded before activation.
6. Keep chain database backups from immediately before the activation window.

At and after height `1000`, compare height and tip hash across at least three
independent nodes. From height `1001`, also compare finalized height and hash.
If one chain has the higher valid checkpoint, normal sync should converge to it.
If equal-height checkpoint hashes conflict, preserve both histories and the
committee signatures, stop automated restarts, and investigate the quorum
failure; the deterministic lower-hash rule is the recovery decision and does
not by itself make the safety breach harmless.

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
4. Check VDF rounds and host performance if VDF work consistently finishes late. The Burn panel shows a speed hint when the local VDF estimate approaches the rank `1` fallback slot; such hosts should keep their burn low and participate through the burn committee.

Recovery:

- If recovery blocks are converging across peers, do not reset. Recovery is part of the liveness design.
- Bring ticket finalizer nodes back online and unlocked.
- Keep the fallback VDF threshold conservative on low-power hosts if too many nodes are wasting work on lower-ranked ticket paths. Every automatic node remains eligible for recovery once the recovery delay has elapsed.
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
