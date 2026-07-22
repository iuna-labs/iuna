# Luun

Luun is a tiny L1 prototype built from the node first, then explained as it grows.

The current devnet assumes friendly nodes. It has one binary that acts as wallet, node, miner, HTTP management UI, and P2P TCP listener. The core ledger is separated from the adapters so a whole network can be tested in memory without opening sockets.

## Run

To start a fresh chain, run genesis mode from an empty wallet path:

```sh
cargo run -- --genesis --http 127.0.0.1:18661 --p2p 127.0.0.1:9444
```

Open `http://127.0.0.1:18661` and complete the initial setup modal. The setup flow verifies the generated 24-word recovery phrase with a 4-word check, stores the wallet in `.luun/wallet.json`, and stores runtime config in `.luun/config.json`.

`--genesis` only works when the wallet file does not already exist and `.luun/chain.sqlite3` does not already contain a blockchain. It creates a fresh setup wallet, opens the setup modal so the recovery phrase can be recorded, creates the starter chain, adaptively measures VDF throughput locally, extrapolates that measurement to a 60-second initial round count, and persists the validated chain. The same data directory resumes automatically on later runs without `--genesis`.

For setup-only local development without creating a chain yet:

```sh
cargo run -- --http 127.0.0.1:18661 --p2p 127.0.0.1:9444
```

For fast local development, set `LUUN_DEV_SKIP_SEED_VERIFY=1` before starting the node to show a setup-only skip button for the recovery phrase check.

Mining is automatic. There is no "mine block" button and no exact sleep. Each node can burn its configured amount once per chain height. Those burns become one-shot leader tickets after the launch profile's maturity delay. The current devnet uses a 3-block maturity delay and a 3-block eligibility window: a burn included at height `h` can win heights `h + 3` through `h + 5`, then expires if it was not selected. Only the selected ticket owner builds the next block, signs a leader proof, performs the VDF work, and gossips the finished block. The VDF is the clock.

Genesis leaves the starter wallet with 100 spendable LUUN after the 1-LUUN bootstrap burn creates launch tickets for blocks 1, 2, and 3, and the genesis block pays its 100-LUUN reward. `--genesis` starts the automatic burn rate at 100 LUUN per block, so the starter can create the block 1 burn that becomes eligible at block 4.

The management UI is a small AlpineJS app served from local vendored assets. It polls JSON endpoints every few seconds and includes:

- wallet and transaction controls,
- runtime mining settings in the Mining screen,
- peer setup and status in the P2P screen,
- P2P peer status with gossip send/receive counters and last error,
- a blockchain explorer and mempool view.

For a second local node joining Alice's chain:

```sh
cargo run -- --data-dir .luun-bob --http 127.0.0.1:18662 --p2p 127.0.0.1:9445 --join 127.0.0.1:9444
```

`--join` fetches a chain snapshot from the peer before mining starts and announces this node's P2P listener back to that peer, so newly mined blocks can flow back without restarting the first node. If the peer cannot provide a snapshot, the node exits instead of silently starting a separate chain. Additional peers can be added from the P2P screen.

If `<data-dir>/chain.sqlite3` already exists, the node resumes that chain first. That makes restarts boring in the good way: `--genesis` will not create a new genesis over an existing local chain, and `--join` remains useful for reconnecting to peers without replacing local state. Pass `--chain-db path/to/chain.sqlite3` to override the database path.

Nodes also run a self-healing sync loop. They periodically compare known peer heights and tip hashes, request missing block ranges when a peer is ahead, and validate those blocks before importing them. Full snapshots are kept for initial join and fallback cases, not as the normal catch-up path. The mempool tolerates future-nonce transactions from peers and mines them once the missing nonce gap is filled.

The UI separates local height from shared height. Local height is the node's own validated tip. Shared height is the lowest recently reported peer height plus the local height, which is a better view of how far the connected network has actually converged.

## Friend Net

To start a small friendly network:

1. Start setup with a public P2P bind and complete the initial setup screen:

```sh
cargo run -- --p2p 0.0.0.0:9444 --http 127.0.0.1:18661
```

2. Restart with genesis mode:

```sh
cargo run -- --genesis --p2p 0.0.0.0:9444 --http 127.0.0.1:18661
```

3. Give friends your public `host:9444`.
4. Friends join your chain:

```sh
cargo run -- --data-dir .luun-friend --p2p 0.0.0.0:9445 --http 127.0.0.1:18661 --join your-host:9444
```

Friends who join after you start will adopt your genesis and current chain. The starter wallet begins with 100 spendable LUUN after the bootstrap burn and genesis reward, and `--genesis` starts it with a 100-LUUN automatic burn rate. After the starter mines additional block rewards, send friends LUUN from the UI; then they can choose a burn amount and compete for future blocks. Every joining node starts with a 0-LUUN automatic burn unless it is configured otherwise.

The genesis block bootstraps the chain with a 1-LUUN burn from the starter wallet. Genesis turns that burn into launch tickets for blocks 1 through 3 so the chain can move until normal burn tickets mature. Burns included after genesis create one-shot tickets through a deterministic ticket lottery. The selected leader creates the next block content, signs a proof for the selected ticket, and runs a hash-chain VDF before gossiping the block.

Every non-genesis block must consume the selected eligible ticket, include at least one burn transaction, and fit under the 100kB serialized block limit. The VDF seed is bound to the parent hash and child height; the block hash separately commits to the miner, timestamp, miner payout, rounds, previous hash, leader proof, VDF output, and transactions.

The protocol targets 60-second blocks by retargeting the expected VDF rounds after each block. It uses a rolling average of recent block intervals and only moves the next round count by about 10% per block, so short bursts do not make the delay swing wildly. Every node derives the same next-round count from the validated chain.

The base block reward is fixed at 100 LUUN, and miners collect transfer fees on top. Burns do not need an extra fee because the burned amount is already the cost of entering the leader lottery. The miner includes the best valid burn for liveness, then fills the remaining block space by fee-rate while respecting nonce and balance validity. The default burn is 0 LUUN per block, so new nodes can join before they own LUUN. Genesis starters begin at 100 LUUN per block; after another wallet has LUUN, raise its burn from the Mining screen.

The measured VDF round count is only the initial delay. After the first blocks, the protocol steers rounds toward the 60-second target.

## Wallet Storage

Luun creates a new wallet file the first time a node starts or joins a chain. By default it lives at `.luun/wallet.json`, or at `<data-dir>/wallet.json` when `--data-dir` is set. Pass `--wallet path/to/wallet.json` to choose a specific wallet file. New wallet files store a 24-word recovery phrase and the derived Ed25519 public key address.

There is no default wallet seed in the binary. Keep the wallet file private; it contains the local wallet seed used to derive the address.

## Node Config

Luun stores UI setup state, configured peers, and the configured automatic burn rate in `<data-dir>/config.json`. If `setup_complete` is false, the management UI opens the initial setup screen for wallet and peer setup. Completing setup and later runtime changes write the file through the HTTP API, so the choices follow the node data directory instead of a browser session.

## Chain Storage

Luun stores the latest validated `ChainSnapshot` in SQLite at `<data-dir>/chain.sqlite3`. The database is updated by a small background persistence task when the tip changes, so web requests, P2P sessions, and VDF work do not perform chain database writes on their main async paths.

## Architecture

- `src/domain.rs`: wallet, fee-paying transfers, fee-free burns, balances, genesis burn bootstrap, 100-LUUN base rewards, 100kB blocks, rolling-window leader tickets, leader proofs, fork choice, launch profile, and VDF checks.
- `src/app.rs`: node use cases, automatic VDF-paced mining, peer bookkeeping, and an in-memory network harness.
- `src/adapters/http.rs`: HTTP management UI and status endpoint.
- `src/adapters/p2p.rs`: line-delimited JSON gossip, block-range catch-up, and chain snapshots over one TCP port.
- `src/adapters/chain_store.rs`: SQLite chain snapshot persistence.
- `src/adapters/config_store.rs`: node-local UI setup config persistence.
- `src/adapters/wallet_store.rs`: local wallet file creation and loading.
- `assets/`: vendored browser assets for the management UI.

## Checks

```sh
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

To install the included pre-commit hook:

```sh
git config core.hooksPath .githooks
chmod +x .githooks/pre-commit
```

The hook runs formatting, clippy, and tests before each commit.
