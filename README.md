# iuna

iuna is a low-energy currency devnet where blocks are finalized by burning IUNA, while new supply is earned through open proof-of-work.

It is not a mainnet and not money yet. The goal right now is simple: run a small real network, learn where the protocol and software bend, and make it pleasant enough that friends can help test it.

## Run A Node

You need Rust installed. Then start a node:

```sh
cargo run -- --http 127.0.0.1:18661 --p2p 0.0.0.0:9445
```

Open `http://127.0.0.1:18661`, set a local password, and back up the recovery phrase. The wallet seed is encrypted on disk. Keep the management UI on `127.0.0.1`; only the P2P port should be reachable by other nodes.

To join an existing devnet, ask for a seed node address and start with `--join`:

```sh
cargo run -- --data-dir .iuna --http 127.0.0.1:18661 --p2p 0.0.0.0:9445 --join seed.example:9444
```

After the node syncs, you can receive IUNA, send transactions, burn for block finalization, or enable PoW mine actions from the Mining screen.

## Start A Fresh Devnet

Only the first operator of a devnet needs genesis mode:

```sh
cargo run -- --genesis --http 127.0.0.1:18661 --p2p 0.0.0.0:9444
```

Genesis requires a fresh wallet and an empty chain database. It creates the starter chain, measures an initial VDF delay, and leaves the starter wallet with spendable IUNA for early testing.

## Optional: Stratum Mining

iuna can expose a Stratum V1 endpoint for SHA-256 ASIC miners such as a Bitaxe:

```sh
cargo run -- --data-dir .iuna --stratum 0.0.0.0:3333
```

Use your iuna wallet address as the worker username. Accepted shares become PoW mine actions in the node mempool and are gossiped to peers.

## How It Works

iuna combines three mechanisms:

- **Proof of Burn:** nodes burn IUNA to enter the block-finalization lottery.
- **VDF clock:** the selected finalizer must run sequential delay work before publishing a block.
- **PoW issuance:** miners create new IUNA through mine actions and choose the fee paid to the finalizer that includes them.

The current devnet targets 60-second blocks, uses local wallet encryption, stores chain state in SQLite, and includes an in-memory network test harness for protocol testing.

## Contributing

Contributions are welcome. Useful help includes running nodes, testing joins/restarts, improving P2P behavior, reviewing protocol incentives, cleaning up UI flows, and adding focused tests.

Before sending changes, run:

```sh
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

The property-style chain tests can also be run directly:

```sh
cargo test --test properties
```

To use the included pre-commit hook:

```sh
git config core.hooksPath .githooks
chmod +x .githooks/pre-commit
```

## License

iuna is licensed under the Apache License 2.0. See `LICENSE`.
