# iuna

iuna is a low-energy currency devnet where blocks are finalized by burning IUNA, while new supply is earned through open proof-of-work.

It is not a mainnet and not money yet. The goal right now is simple: run a small real network, learn where the protocol and software bend, and make it pleasant enough that friends can help test it.

## Download

Download the latest build for your platform from the [GitHub Releases](https://github.com/iuna-labs/iuna/releases) page. You do not need Rust or Cargo to run a node.

Release archives are published for:

- Linux x86_64
- macOS x86_64
- macOS Apple Silicon
- Windows x86_64

Unpack the archive and run the `iuna` binary from a terminal.

On macOS or Linux:

```sh
chmod +x ./iuna
./iuna --http 127.0.0.1:18661 --p2p 0.0.0.0:9445
```

On Windows PowerShell:

```powershell
.\iuna.exe --http 127.0.0.1:18661 --p2p 0.0.0.0:9445
```

For the commands below, use `.\iuna.exe` instead of `./iuna` on Windows.

Open `http://127.0.0.1:18661`, set a local password, and back up the recovery phrase. The wallet seed is encrypted on disk. Keep the management UI on `127.0.0.1`; only the P2P port should be reachable by other nodes.

## Join A Devnet

Ask for a seed node address and start iuna with `--join`:

```sh
./iuna --data-dir .iuna --http 127.0.0.1:18661 --p2p 0.0.0.0:9445 --join seed.example:9444
```

After the node syncs, you can receive IUNA, send transactions, burn for block finalization, or enable PoW mine actions from the Mining screen.

## Start A Fresh Devnet

Only the first operator of a devnet needs genesis mode:

```sh
./iuna --genesis --http 127.0.0.1:18661 --p2p 0.0.0.0:9444
```

Genesis requires a fresh wallet and an empty chain database. It creates the starter chain, measures an initial VDF delay, and leaves the starter wallet with spendable IUNA for early testing.

## Optional: Stratum Mining

iuna can expose a Stratum V1 endpoint for SHA-256 ASIC miners such as a Bitaxe:

```sh
./iuna --data-dir .iuna --stratum 0.0.0.0:3333
```

Use your iuna wallet address as the worker username. Accepted shares become PoW mine actions in the node mempool and are gossiped to peers.

## How It Works

iuna combines three mechanisms:

- **Proof of Burn:** nodes burn IUNA to enter the block-finalization lottery.
- **VDF clock:** the selected finalizer must run sequential delay work before publishing a block.
- **PoW issuance:** miners create new IUNA through mine actions and choose the fee paid to the finalizer that includes them.

The current devnet targets 10-minute blocks, uses local wallet encryption, stores chain state in SQLite, and includes an in-memory network test harness for protocol testing.

## Contributing

Contributions are welcome. Useful help includes running nodes, testing joins/restarts, improving P2P behavior, reviewing protocol incentives, cleaning up UI flows, and adding focused tests.

Contributors need Rust and Cargo. Before sending changes, run:

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

## Release Builds

Maintainers publish binaries by pushing a version tag:

```sh
git tag v0.1.0
git push origin v0.1.0
```

The release workflow builds platform archives, writes `SHA256SUMS`, and attaches everything to the GitHub Release for that tag.

## License

iuna is licensed under the Apache License 2.0. See `LICENSE`.
