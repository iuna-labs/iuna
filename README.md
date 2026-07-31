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

iuna tries a different split. Finalization is lightweight and based on a burn lottery plus VDF timing, while new supply stays open to proof-of-work. The intended benefit is better decentralization pressure than pure PoW or pure PoS: finalizing blocks should not require owning specialized mining scale, and issuing new coins should not require already being a large holder.

This is still an experiment. The design needs real-world testing before those goals can be treated as proven.

## Install

The simplest way to run iuna is:

1. Go to [GitHub Releases](https://github.com/iuna-labs/iuna/releases).
2. Download the latest build for your platform.
3. Start the app or binary.
4. Follow the setup screen.

The setup flow helps you create or import a wallet, back up your recovery phrase, and connect to the devnet.

You do not need Rust or Cargo unless you want to work on the code.

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

## License

iuna is licensed under the Apache License 2.0. See `LICENSE`.
