# iuna

iuna is an experimental cryptocurrency built around two connected protocols:
a Proof-of-Work **Heartbeat Chain** that produces a block about once a minute,
and a **Ledger Chain** that settles UTXO transactions about once every ten
heartbeats.

Ledger mining uses **Proof of Expenditure**. A mining ticket locks a chosen IUNA
budget for a limited number of heartbeats. That budget buys temporary lottery
weight and is streamed to Heartbeat miners; it is not burned and it is not
returned to the ticket owner. Equal expenditure buys equal temporary weight,
regardless of an owner's existing balance.

iuna currently runs as a **candidate mainnet**. It is not mainnet yet, and the
protocol remains experimental. The chain is intended to carry forward if the
candidate is promoted, but that is not guaranteed.

## Protocol at a glance

- Heartbeats use SHA-256 Proof of Work and target 60 seconds.
- Ledger blocks target 10 heartbeats, or about 10 minutes.
- Mining tickets become active after confirmation and last 1–1,440
  heartbeats.
- Ticket expenditure is paid to Heartbeat miners over the ticket lifecycle.
- Ledger winners sign non-empty candidates; Heartbeat miners may only include them unchanged.
- A missed winner deadline settles as a deterministic empty, unsigned fallback.
- The Ledger winner receives the Ledger subsidy and its share of transaction
  fees.
- Legacy delay, burn, migration, recovery, and height-activation paths are not
  part of this protocol.

See [docs/protocol.md](docs/protocol.md) for the current protocol and
[docs/operator.md](docs/operator.md) for running the node and using its operator
API.

The browser wallet in `wallet/` is a separate self-custody client. It creates
or restores the same 24-word/Ed25519 wallet entirely in the browser, encrypts
the seed locally, reads public UTXOs from a node and submits only signed Ledger
transactions. The deployment serves it at `https://wallet.getiuna.com`.

## Run a node

```sh
cargo run --release
```

By default the operator screen is available at `http://127.0.0.1:8080` and the
P2P listener uses TCP port `9444`, with node data stored in `~/.iuna`. On the
first upgrade from the old protocol, recognized legacy files are renamed
to collision-safe `.backup` files before the new data is created. A chain of
another network or genesis is set aside the same way; the wallet is kept. Initial
Setup creates wallets with a
24-word recovery phrase and backup check, restores an existing phrase, or
unlocks the encrypted local wallet. Management is available directly on the
loopback-only interface; the operator guide explains the security model for
other deployments.

The public browser-wallet API is disabled by default. Public nodes can opt in
with a separate listener, for example `--wallet-endpoint 0.0.0.0:18662`; never
expose the operator listener as a substitute. The local Compose bootstrap and
the production DC node enable this listener explicitly.

## Contributing

iuna is an open experiment, and contributions of all kinds are welcome:

- Open a [pull request](https://github.com/iuna-labs/iuna/pulls) or
  [issue](https://github.com/iuna-labs/iuna/issues).
- Share feedback, questions, and failure reports on
  [Discord](https://discord.gg/wdk8cjkj2G).
- Run independent nodes and test synchronization, reorgs, tickets,
  wallet operations, and failure recovery.
- Review the protocol, test assumptions, report bugs, or suggest improvements.
