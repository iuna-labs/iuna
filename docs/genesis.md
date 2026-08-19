# Genesis And Candidate Rehearsal

This is operator documentation for bootstrapping a iuna devnet or mainnet-candidate network. Most users should join an existing bootnode instead of creating genesis.

The mainnet-candidate genesis is not disposable by default. It is the genesis that can become mainnet if the candidate passes the agreed stability window and release gates. In that case, mined coins, UTXOs, tickets, and chain history remain on the same ledger; promotion is a coordinated release and network-identity cutover, not a second genesis.

Keep the management UI bound to `127.0.0.1`. Only the P2P listener should be internet-facing.

## Candidate Manifest

Before creating the mainnet-candidate genesis, publish one manifest in the release notes or operator coordination channel:

```text
version:
git commit:
release tag:
genesis operator:
genesis start time:
genesis hash:
stability window:
promotion policy:
bootnodes:
checksums:
```

Fill it with:

- the exact release artifact version and git commit;
- the release tag, once created;
- the genesis operator and UTC start time;
- the genesis hash after the first node starts;
- the agreed no-reset stability window, for example one week;
- whether a healthy candidate will be promoted to mainnet without a second genesis;
- every public bootnode as `<host>:<p2p-port>`;
- a link or pasted copy of `downloads/SHA256SUMS`.

Do not start the stability window until the fresh genesis exists, at least one published bootnode is reachable, and independent operators have verified the release checksums.

## Release Artifacts

Build and deploy from the candidate commit:

```sh
cargo test --locked
cargo check --locked --manifest-path fuzz/Cargo.toml
cargo run --locked --manifest-path fuzz/Cargo.toml --bin p2p_envelope -- -runs=256 fuzz/corpus/p2p_envelope
cargo run --locked --manifest-path fuzz/Cargo.toml --bin compact_snapshot -- -runs=256 fuzz/corpus/compact_snapshot
cargo run --locked --manifest-path fuzz/Cargo.toml --bin domain_json -- -runs=256 fuzz/corpus/domain_json
cargo run --locked --manifest-path fuzz/Cargo.toml --bin stratum_request -- -runs=256 fuzz/corpus/stratum_request
cargo run --locked --manifest-path fuzz/Cargo.toml --bin wallet_config -- -runs=256 fuzz/corpus/wallet_config
cargo test --locked --release --test properties -- --ignored
./deployment.sh <version>
```

The deployment script writes release packages to `downloads/` and creates `downloads/SHA256SUMS`.
It runs `256` iterations per fuzz target by default; set `IUNA_FUZZ_RUNS`
only for an explicitly documented emergency redeploy.

Verify the files before publishing them:

```sh
shasum -a 256 -c downloads/SHA256SUMS
```

On Linux, `sha256sum -c downloads/SHA256SUMS` is equivalent.

Publish the checksums with the release artifacts. A node operator should be able to download the artifact and verify it before starting a candidate node.

## Create Genesis

Genesis requires a fresh wallet path and a fresh chain database. Use a new data directory for the candidate genesis:

```sh
iuna --genesis --data-dir ~/.iuna-candidate-genesis --p2p 0.0.0.0:9444 --http 127.0.0.1:18661
```

For local source builds, use the same flags through Cargo:

```sh
cargo run -- --genesis --data-dir ~/.iuna-candidate-genesis --p2p 0.0.0.0:9444 --http 127.0.0.1:18661
```

Open `http://127.0.0.1:18661`, set the wallet password if prompted, write down the recovery phrase, and finish setup. The process prints the wallet file, config file, chain database, management UI, and P2P listener on startup.

Genesis bootstraps the chain with a 1 IUNA burn, creates launch tickets for the first blocks, measures an initial VDF delay, and leaves the starter wallet with spendable IUNA for early testing.

After startup, record the genesis hash from block `0` in the local UI or authenticated blocks endpoint:

```sh
curl -s 'http://127.0.0.1:18661/api/blocks?before_height=1&limit=1'
```

If `curl` returns an auth response, collect the same value from the local UI or include the browser session cookie in the request.

## Publish Bootnodes

Publish at least one stable public P2P address:

```text
your-host.example:9444
```

For every bootnode:

- allow inbound TCP traffic on the P2P port;
- keep the HTTP management UI local-only;
- publish the address operators should use with `--join`;
- record the operator or owner in the candidate manifest.

If a bootnode address changes during the stability window, treat it as an operational incident and update the manifest. Do not hide bootnode churn from candidate notes.

## Join Nodes

Start joining nodes with a fresh data directory and a published bootnode:

```sh
iuna --data-dir ~/.iuna-candidate --p2p 0.0.0.0:9445 --http 127.0.0.1:18661 --join your-host.example:9444
```

For a public peer, configure the reachable P2P listener and announce address in Settings or with CLI flags:

```sh
iuna --data-dir ~/.iuna-candidate --p2p 0.0.0.0:9445 --p2p-announce your-node.example:9445 --http 127.0.0.1:18661 --join your-host.example:9444
```

For a private wallet-only node, leave inbound P2P disabled in Settings and connect outbound to bootnodes.

## Backup And Restore Drill

Before the stability window starts, every core operator should prove they can restore their node.

Back up:

- `wallet.json`;
- `config.json`;
- `chain.sqlite3`;
- `ui_data.sqlite3`;
- the release artifact used to run the node;
- the matching `SHA256SUMS` entry;
- the wallet recovery phrase, stored separately from the machine.

With the default data directory these files live under `~/.iuna`. With `--data-dir`, they live under that directory unless `--wallet` or `--chain-db` overrides the path.

Restore rehearsal:

1. Stop the node.
2. Copy the backup into a new restore data directory.
3. Start the same release binary with `--data-dir <restore-dir>`.
4. Confirm the wallet address, local height, tip hash, and peers match expectations.
5. If restoring without chain state, start with a published bootnode and let the node sync:

```sh
iuna --data-dir <restore-dir> --join your-host.example:9444
```

Never use `--genesis` to recover a node. `--genesis` is only for creating a fresh network.

## No-Reset Stability Window

For the mainnet-candidate network, treat unplanned resets as launch-blocking incidents unless they were explicitly scheduled before the window started. Operators may mine and transact during this window with the expectation that the ledger can become mainnet if the candidate passes.

Start the window only after:

- genesis hash and start time are published;
- release artifacts and `SHA256SUMS` are published;
- at least one bootnode is reachable from an independent host;
- at least one non-genesis node has synced from the published bootnode;
- backup/restore has been rehearsed by core operators.

During the window, record daily:

- height and tip hash from at least two nodes;
- peer count and stale peer count;
- last block age;
- finalizer mode, recovery blocks, and fallback frequency;
- mempool size and rejected block or transaction errors;
- any node restart, bootnode change, restore, rollback, or manual chain deletion.

Use [operator failure playbooks](operator-playbooks.md) for stalled height, divergent tips, old snapshots, missing burn committee signatures, recovery blocks, and corrupted local persistence.

Exit criteria:

- no unplanned reset during the full agreed window;
- fresh nodes can still sync from genesis through published bootnodes;
- release artifact checksums are independently verified;
- backup/restore rehearsal has passed;
- all launch-blocking incidents are fixed or explicitly deferred before mainnet.

## Upgrade And Rollback

For routine candidate upgrades, preserve local state and replace only the
software artifact:

1. Stop the node.
2. Back up `wallet.json`, `config.json`, `chain.sqlite3`, and `ui_data.sqlite3`.
3. Verify the new release artifact against the published `SHA256SUMS`.
4. Start the new binary with the same `--data-dir`, `--wallet`, `--chain-db`,
   P2P, HTTP, and Stratum settings.
5. Confirm the wallet address, genesis hash, local height, tip hash, launch
   profile, peer count, and last block age.

Do not start upgrades with `--genesis`. Do not delete `chain.sqlite3` during a
candidate-to-mainnet promotion. The promoted release must load the existing
candidate chain database and continue from the current tip.

If the upgraded node fails before it mines or accepts blocks under new rules,
rollback is a software rollback:

1. Stop the upgraded node.
2. Restart the previous verified binary with the same data directory.
3. Confirm the node resumes the same height and tip it had before the upgrade.
4. Keep peers connected and let normal sync catch up if the network advanced
   while the node was offline.

If the upgraded node has already accepted blocks that older binaries reject, do
not silently roll back. Treat that as a possible hard-fork or release incident:
preserve chain/UI databases, stop public bootnode churn, compare tips across
operators, and publish a decision before asking operators to delete or replace
chain data.

## Promotion To Mainnet

If the candidate passes the stability window, publish a promotion notice instead of a new genesis plan. The notice should include:

- candidate genesis hash;
- promoted tip height and tip hash;
- final candidate release tag and mainnet release tag;
- bootnodes that will remain online through the cutover;
- whether the P2P network ID changes from `iuna-mainnet-candidate-v1` to `iuna-mainnet-v1`;
- the exact upgrade window for operators.

Do not delete chain data when promoting. Nodes should keep `chain.sqlite3`, wallet files, and UI data, then upgrade or restart with the promoted release. A P2P network ID change fences upgraded mainnet nodes away from old candidate binaries, but it must not change genesis, launch profile rules, or any existing ledger state.
