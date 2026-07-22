# Mivora

Mivora is a tiny L1 coin prototype built from the node first, then explained as it grows.

The current devnet assumes friendly nodes. It has one binary that acts as wallet, node, miner, HTTP management UI, and P2P TCP listener. The core ledger is separated from the adapters so a whole network can be tested in memory without opening sockets.

## Run

```sh
cargo run -- --start --http 127.0.0.1:8443 --p2p 127.0.0.1:9444
```

Open `http://127.0.0.1:8443`. The wallet is generated into `.mivora/`.
The validated chain is persisted to `.mivora/chain.sqlite3` and resumes automatically when the same data directory is started again.

Mining is automatic. There is no "mine block" button and no exact sleep. Each node can burn its configured amount once per chain height. Those burns become one-shot leader tickets for a future height after the launch profile's maturity delay. Only the selected ticket owner builds the next block, signs a leader proof, performs the VDF work, and gossips the finished block. The VDF is the clock.

The plain command above creates the default zero-balance starter chain: genesis mints 1 coin and immediately burns it into the first leader ticket. That is enough to mine block 1 and earn the first reward. For a self-running local demo, leave one extra coin after genesis and burn it into block 1 so block 2 already has a ticket:

```sh
cargo run -- --start --genesis-amount 2 --burn-per-block 1 --http 127.0.0.1:8443 --p2p 127.0.0.1:9444
```

The management UI is a small AlpineJS app served from local vendored assets. It polls JSON endpoints every few seconds and includes:

- wallet and transaction controls,
- fixed burn-per-block settings,
- P2P peer status with gossip send/receive counters and last error,
- a blockchain explorer and mempool view.

For a second local node joining Alice's chain:

```sh
cargo run -- --name bob --data-dir .mivora-bob --http 127.0.0.1:8444 --p2p 127.0.0.1:9445 --join 127.0.0.1:9444
```

`--join` fetches a chain snapshot from the peer before mining starts and announces this node's P2P listener back to that peer, so newly mined blocks can flow back without restarting the first node. If the peer cannot provide a snapshot, the node exits instead of silently starting a separate chain. Plain `--peer` only adds a gossip peer and does not require bootstrap success.

If `<data-dir>/chain.sqlite3` already exists, the node resumes that chain first. That makes restarts boring in the good way: `--start` will not create a new genesis over an existing local chain, and `--join` remains useful for reconnecting to peers without replacing local state. Pass `--chain-db path/to/chain.sqlite3` to override the database path.

Nodes also run a self-healing sync loop. They periodically compare known peer heights and tip hashes, request missing block ranges when a peer is ahead, and validate those blocks before importing them. Full snapshots are kept for initial join and fallback cases, not as the normal catch-up path. The mempool tolerates future-nonce transactions from peers and mines them once the missing nonce gap is filled.

The UI separates local height from shared height. Local height is the node's own validated tip. Shared height is the lowest recently reported peer height plus the local height, which is a better view of how far the connected network has actually converged.

## Friend Net

To start a small friendly network:

1. Start your node with a public P2P bind:

```sh
cargo run -- --start --genesis-amount 2 --burn-per-block 1 --p2p 0.0.0.0:9444 --http 127.0.0.1:8443
```

2. Give friends your public `host:9444`.
3. Friends join your chain:

```sh
cargo run -- --data-dir .mivora-friend --p2p 0.0.0.0:9445 --http 127.0.0.1:8443 --join your-host:9444
```

Friends who join after you start will adopt your genesis and current chain. With the default genesis amount, the starter wallet begins with a 0 balance because genesis mints 1 coin and immediately burns it as the first leader ticket. For a moving demo, `--genesis-amount 2 --burn-per-block 1` leaves the starter one coin to burn into block 1, creating the ticket for block 2. After the starter mines the first block reward, send friends coins from the UI; then they can choose a burn amount and compete for future blocks. Every joining node starts with a 0-coin automatic burn unless it is configured otherwise.

The genesis block bootstraps the chain with a 1-coin burn from the starter wallet. Burns included in a block create one-shot tickets for a future height through a deterministic ticket lottery. The selected leader creates the next block content, signs a proof for the selected ticket, and runs a hash-chain VDF before gossiping the block.

Every non-genesis block must consume the selected mature ticket. A block may contain zero burns, but then it does not create future tickets. The VDF seed is bound to the parent hash and child height; the block hash separately commits to the miner, timestamp, reward, rounds, previous hash, leader proof, VDF output, and transactions.

The protocol targets 60-second blocks by retargeting the expected VDF rounds after each block. It uses a rolling average of recent block intervals and only moves the next round count by about 10% per block, so short bursts do not make the delay swing wildly. Every node derives the same next-round count from the validated chain.

The block reward is fixed at 100 coins. The default burn is 0 coins per block, so new nodes can join before they own coins. After a wallet has coins, raise the burn from the UI or with:

```sh
cargo run -- --burn-per-block 25
```

The default VDF round count is only the initial delay. After the first blocks, the protocol steers rounds toward the 60-second target. For fast local demos and tests, pass a smaller initial value:

```sh
cargo run -- --vdf-rounds 10000
```

## Wallet Storage

Mivora creates a new wallet file the first time a node starts or joins a chain. By default it lives at `.mivora/wallet.json`, or at `<data-dir>/wallet.json` when `--data-dir` is set. Pass `--wallet path/to/wallet.json` to choose a specific wallet file.

There is no default wallet seed in the binary. Keep the wallet file private; it contains the local wallet seed used to derive the address.

## Chain Storage

Mivora stores the latest validated `ChainSnapshot` in SQLite at `<data-dir>/chain.sqlite3`. The database is updated by a small background persistence task when the tip changes, so web requests, P2P sessions, and VDF work do not perform chain database writes on their main async paths.

## Architecture

- `src/domain.rs`: wallet, transactions, balances, genesis burn bootstrap, fixed 100-coin rewards, blocks, mature leader tickets, leader proofs, fork choice, launch profile, and VDF checks.
- `src/app.rs`: node use cases, automatic VDF-paced mining, peer bookkeeping, and an in-memory network harness.
- `src/adapters/http.rs`: HTTP management UI and status endpoint.
- `src/adapters/p2p.rs`: line-delimited JSON gossip, block-range catch-up, and chain snapshots over one TCP port.
- `src/adapters/chain_store.rs`: SQLite chain snapshot persistence.
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
