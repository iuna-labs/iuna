# Starting The Genesis Node

This is operator documentation for bootstrapping a iuna devnet. Most users should join an existing seed node instead.

## Start Setup

Start with a public P2P bind and a local-only management UI:

```sh
cargo run -- --p2p 0.0.0.0:9444 --http 127.0.0.1:18661
```

Open `http://127.0.0.1:18661`, set the wallet password, write down the recovery phrase, and finish setup.

## Create Genesis

Restart from a fresh chain database with `--genesis`:

```sh
cargo run -- --genesis --p2p 0.0.0.0:9444 --http 127.0.0.1:18661
```

Genesis bootstraps the chain with a 1 IUNA burn, creates launch tickets for the first blocks, measures an initial VDF delay, and leaves the starter wallet with spendable IUNA for early testing.

## Invite Nodes

Give other node operators your public P2P address:

```text
your-host.example:9444
```

They can join with:

```sh
cargo run -- --data-dir .iuna --p2p 0.0.0.0:9445 --http 127.0.0.1:18661 --join your-host.example:9444
```

Keep the management UI bound to `127.0.0.1`.
