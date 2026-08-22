# iuna

iuna is an experimental cryptocurrency devnet.

It combines three ideas:

- **VDF finalization:** each block waits on verifiable sequential delay work.
- **Burn lottery:** nodes burn IUNA to enter the lottery for who may finalize the next block.
- **Proof-of-work issuance:** new IUNA is created through open PoW mine actions.

## Status

iuna is still in development. It is not a mainnet, not money, and not something to treat as financially valuable yet.

The goal right now is to run a real test network, improve the wallet and node software, and learn how the protocol behaves with real users.

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

The setup flow helps you create or import a wallet, back up your recovery phrase, and connect to the devnet.

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

- `downloads/iuna-v0.3.10-macos-aarch64-desktop.app.zip`
- `downloads/iuna-v0.3.10-windows-x86_64-desktop-setup.exe`

Release and deploy with:

```sh
./deployment.sh 0.2.48
```

Deployment publishes two images to the `jhx-app` k3s cluster:

- `https://getiuna.org/` routes to the static website image.
- `https://admin.iuna.jhx.app/` routes to the node management UI.
- `iuna.jhx.app:9444` routes to the node P2P listener.

Useful overrides:

```sh
IUNA_DEPLOY_HOST=root@jhx.app IUNA_KUBECTL_CONTEXT=jhx-app ./deployment.sh 0.2.48
```

## What You Can Run

You can use iuna as a wallet, a node, or a public peer.

- **Wallet:** send, receive, and inspect activity.
- **Node:** keep a local chain copy and participate in mining/finalization settings.
- **Public peer:** same as a node, but reachable by other nodes through a public P2P address.

Keep the management UI local. Only the P2P listener should be reachable by other nodes.

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

The compose file starts one bootstrap genesis node and five joining nodes on an
isolated Docker network. Every node automatically mines with one PoW worker and
enables burn/finalization. Joining nodes can therefore earn their first
spendable IUNA without a bootstrap transfer and begin burning afterward.
Management UIs are exposed on:

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

Use your iuna wallet address as the worker username. Accepted shares become PoW mine actions in the node mempool and are gossiped to peers.

## Operator Docs

- [Genesis and candidate rehearsal](docs/genesis.md)
- [Operator failure playbooks](docs/operator-playbooks.md)
- [Security review checklist](docs/security-review.md)

## Contributing

We are open to PRs and help running, testing, and improving the devnet.

See [THANKS.md](THANKS.md) for people who have helped test and improve iuna.

## License

iuna is licensed under the Apache License 2.0. See `LICENSE`.
