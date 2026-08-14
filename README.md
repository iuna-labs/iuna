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

- `downloads/iuna-v0.3.5-macos-aarch64-desktop.app.zip`
- `downloads/iuna-v0.3.5-windows-x86_64-desktop-setup.exe`

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

## Optional: Stratum Mining

iuna can expose a Stratum V1 endpoint for SHA-256 ASIC miners such as a Bitaxe:

```sh
./iuna --stratum <bind-address>:<port>
```

Use your iuna wallet address as the worker username. Accepted shares become PoW mine actions in the node mempool and are gossiped to peers.

## Contributing

We are open to PRs and help running, testing, and improving the devnet.

See [THANKS.md](THANKS.md) for people who have helped test and improve iuna.

## License

iuna is licensed under the Apache License 2.0. See `LICENSE`.
