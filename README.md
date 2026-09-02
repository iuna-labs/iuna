# iuna

iuna is an experimental cryptocurrency network running a live mainnet-candidate chain.

It combines three ideas:

- **VDF finalization:** each block waits on verifiable sequential delay work.
- **Burn lottery:** nodes burn IUNA to enter the lottery for who may finalize the next block.
- **Proof-of-work issuance:** new IUNA is created through open PoW mine actions.

## Status

iuna is still in development. Its mainnet-candidate network is live, but it is not mainnet yet and remains experimental. The live candidate ledger is intended to be preserved if it proves stable enough for promotion, but that outcome is not guaranteed.

The goal right now is to operate and harden the live candidate with mainnet-like release, security, and recovery procedures while learning how the protocol behaves with real users.

## Why Another Crypto?

Most chains lean heavily on one scarce resource:

- Proof-of-work chains rely on hashpower.
- Proof-of-stake chains rely on existing stake.

iuna tries a different split. Finalization is lightweight and based on a burn lottery plus VDF timing, while new supply stays open to proof-of-work. Burns do not remove wealth advantage, but they make finalization power temporary and repeatedly paid for instead of a permanent stake position. The intended benefit is better decentralization pressure than pure PoW or pure PoS: finalizing blocks should not require owning specialized mining scale, and burn-based timing power should expire instead of accumulating into lasting control.

This is still an experiment. The design needs real-world testing before those goals can be treated as proven.

## Install

The simplest way to run iuna is:

1. Go to [getiuna.org/downloads/](https://getiuna.org/downloads/).
2. Download the latest available build.
3. Start the app or binary.
4. Follow the setup screen.

The setup flow helps you create or import a wallet, back up your recovery phrase, and connect to the mainnet-candidate network.

You do not need Rust or Cargo unless you want to work on the code.

## Source

The public source browser is published at [getiuna.org/git/iuna/](https://getiuna.org/git/iuna/).

Clone the static HTTP repo with:

```sh
git clone https://getiuna.org/git/iuna.git
```

## Static Site Image

The Docker image publishes the website, a static git browser, a clonable HTTP repo, and release downloads.

```sh
docker build -t iuna-static-site:test .
docker run --rm -p 8080:80 iuna-static-site:test
```

The deployment script builds Linux CLI archives for x86_64 and aarch64, builds the macOS desktop artifact on Apple silicon, and tries to cross-build the Windows NSIS installer in Docker. Prebuilt desktop artifacts can still be added before the image build:

- `downloads/iuna-v0.4.7-macos-aarch64-desktop.app.zip`
- `downloads/iuna-v0.4.7-windows-x86_64-desktop-setup.exe`

Release and deploy with:

```sh
./deployment.sh 0.4.7
```

To deploy only the website from the current commit, without changing the
project version, creating a release tag, rebuilding release artifacts, or
restarting the node:

```sh
./deployment.sh --website-only
```

The website-only deployment requires a clean worktree and tags the image with
the current Git commit, for example `iuna-www:git-2f9e36fabc12`. Use
`./deployment.sh --help` to see all supported modes.

By default, deployment audits all three Rust lockfiles, enforces the dependency
source/license policy, runs the regular unit tests, verifies that the fuzz
targets compile against their locked dependencies, and runs the extended
adversarial, fuzz, post-height-1000 six-node E2E, and release-property suites.
Release hosts therefore need `cargo-audit`, `jq`, and Docker Compose in addition
to the pinned Rust 1.88 toolchain. To
explicitly skip the long-running suites:

```sh
./deployment.sh --skip-long-tests 0.4.7
```

To start a new chain, deploy with `--genesis`. This asks for confirmation,
deletes and recreates the permanent `local-path-db-pvc`, and starts the node
once with `--genesis`. The existing chain data is permanently removed:

```sh
./deployment.sh --genesis 0.4.7
```

Deployment publishes two images to the `jhx-app` k3s cluster:

- `https://getiuna.org/` routes to the static website image.
- `https://admin.iuna.jhx.app/` routes to the IP-restricted node management UI.
- `iuna.jhx.app:9444` routes to the node P2P listener.

Useful overrides:

```sh
IUNA_DEPLOY_HOST=root@jhx.app IUNA_KUBECTL_CONTEXT=jhx-app ./deployment.sh 0.4.7
```

## What You Can Run

You can use iuna as a wallet, a node, or a public peer.

- **Wallet:** send, receive, and inspect activity.
- **Node:** keep a local chain copy and participate in mining/finalization settings.
- **Public peer:** same as a node, but reachable by other nodes through a public P2P address.

Keep the management UI local or behind a strict access control such as the production IP allowlist. Only the P2P listener should be generally reachable by other nodes.

## Optional: CLI

Release archives also include a command-line binary.

On macOS or Linux:

```sh
chmod +x ./iuna
./iuna
```

On Windows PowerShell:

```powershell
.\iuna.exe
```

The binary prints a local management URL. Open it and follow setup.

## Optional: Local Docker Testnet

For local P2P and consensus testing, start a six-node testnet with Docker
Compose:

```sh
docker compose up --build
```

For repeatable end-to-end tests around mature protocol heights such as 999,
1000, and 1001, use the isolated 5-second harness documented in
[`e2e/README.md`](e2e/README.md). It captures and restores all six chain
databases and test identities rather than re-mining from genesis for every run.

The compose file starts the static website, one bootstrap genesis node, and five
joining nodes on an isolated Docker network. Every node automatically mines with
one PoW worker and enables burn/finalization. Joining nodes can therefore earn
their first spendable IUNA without a bootstrap transfer and begin burning
afterward. The website and management UIs are exposed on:

- website: <http://127.0.0.1:8080/>
- bootstrap: <http://127.0.0.1:18661/>
- node2: <http://127.0.0.1:18662/>
- node3: <http://127.0.0.1:18663/>
- node4: <http://127.0.0.1:18664/>
- node5: <http://127.0.0.1:18665/>
- node6: <http://127.0.0.1:18666/>

Node3 also exposes Stratum on `127.0.0.1:3333`. P2P ports `19444` through
`19449` are mapped for local inspection, while nodes announce their
stable Docker-network addresses to each other.

The local compose file uses `testtesttest` as the management UI and wallet
password for all six nodes. On first start this configures the management UI password and
encrypts the wallet; on restart it unlocks the encrypted wallet so finalization
and automatic mining can continue without UI login. Compose also sets
`IUNA_SETUP_COMPLETE=true`, so after unlocking the management UI you land
directly in the node instead of the initial setup wizard. Override the shared
local-testnet password from your shell or a `.env` file:

```sh
IUNA_TESTNET_PASSWORD='change-this-testnet-password' \
docker compose up --build
```

Do not use compose-file default passwords for public nodes or valuable wallets.
If the volumes were created with the older per-node passwords, change those
passwords first or recreate the disposable local-testnet volumes before starting
the updated compose stack.

Bootstrap uses `--genesis` only while `/data/chain.sqlite3` is absent or empty.
Once the chain database exists, container restarts launch bootstrap normally and
preserve the existing chain.

Fresh nodes prefill automatic burns at `0.0001 IUNA` per block with a
`0.000001 IUNA` per-byte fee.

For unattended local nodes, startup environment flags can also persist mining
settings:

- `IUNA_SETUP_COMPLETE=true|false`
- `IUNA_AUTOMATIC_BURN_ENABLED=true|false`
- `IUNA_POW_MINING_ENABLED=true|false`
- `IUNA_POW_MINING_WORKERS=1..32`

The compose bootstrap also selects the isolated `iuna-local-testnet-v1` launch
profile. Its PoW burn-committee lineages are eligible immediately, so joining
nodes can provide independent committee attestations as soon as their first mine
actions are confirmed. Six distinct owners allow the testnet to exercise a full
five-member rank-1 committee even though the missed rank-0 owner is excluded.
The normal launch profile retains the 20-block lineage maturity.
Existing compose volumes created with the normal profile must be reset once
with `docker compose down -v`, because consensus launch profiles cannot be
changed in place. The five-slot committee is also a consensus reset: volumes
created by the earlier three-slot protocol must likewise be recreated.

The current release writes compact snapshot v7 and accepts snapshot versions v6
and v7. Legacy JSON databases and compact versions older than v6 are archived
with a `.pre-v6` suffix and replaced by a fresh database; wallet and
configuration files are retained. The coordinated reset that created the live
mainnet-candidate chain is complete. Existing and new operators should join that
chain instead of creating another genesis. See the operator playbooks for
details and manual archive commands.

Stop the network while keeping chain data:

```sh
docker compose down
```

Reset the local testnet volumes and create a fresh genesis:

```sh
docker compose down -v
```

## Optional: Stratum Mining

iuna can expose a Stratum V1 endpoint for SHA-256 ASIC miners such as a Bitaxe:

```sh
./iuna --stratum <bind-address>:<port>
```

Use the checksummed Bech32m address shown by your iuna wallet as the worker
username (`iuna1...` on mainnet/mainnet-candidate or `tiuna1...` on local
testnet). Hex public keys and addresses for another network are rejected.
Accepted shares become PoW mine actions in the node mempool and are gossiped to
peers.

## Operator Docs

- [Protocol](docs/protocol.md)
- [Operator failure playbooks](docs/operator-playbooks.md)
- [Security review checklist](docs/security-review.md)

## Contributing

We are open to PRs and help running, testing, and improving the mainnet candidate.

See [THANKS.md](THANKS.md) for people who have helped test and improve iuna.

## License

iuna is licensed under the Apache License 2.0. See `LICENSE`.
