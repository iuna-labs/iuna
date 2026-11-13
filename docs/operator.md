# IUNA operator API

The `iuna` binary serves public, read-only operator data and optional
wallet management routes from the address configured with `--http`. The default
is `127.0.0.1:8080`.

`GET /` serves the responsive Dashboard, Wallet, Mining, P2P, Chain, Metrics and
Settings screens. Initial Setup can create a new encrypted wallet, restore one
from its 24-word recovery phrase, or unlock an existing wallet file. An entered
operator token is kept in browser `sessionStorage`, never persistent local
storage.

## Startup

- `IUNA_WALLET_PASSWORD` optionally unlocks an existing encrypted wallet. It
  deliberately does not create one: new wallets must go through Initial Setup
  so their recovery phrase is shown and checked.
- `IUNA_OPERATOR_TOKEN` enables mutating management routes. It must contain at
  least 32 bytes, should be generated with strong randomness, and is retained
  in application state only as a domain-separated SHA-256 hash.
- When management is enabled, `--http` must use a loopback address. A remote
  operator interface must be provided by a separately secured TLS proxy.
- `--wallet-endpoint ADDRESS` explicitly enables the narrow public browser-wallet
  API on a separate listener. It is disabled when this option is absent and its
  port must differ from the operator and P2P listeners.

The wallet password and operator token are separate credentials. On the default
loopback listener, management is available without a token and rejects
cross-origin browser requests. `POST /api/wallet` creates, restores, or unlocks
the encrypted wallet in process memory. Use an operator token for the packaged
desktop app or any separately secured deployment.

The packaged desktop app creates a random 256-bit operator token on first
launch and stores it with owner-only permissions on Unix. Users choose the
wallet password themselves in Initial Setup. Node data defaults to `~/.iuna`.
On first startup after upgrading from the old protocol, recognized legacy
chain, wallet, UI and configuration files are preserved alongside it with a
collision-safe `.backup` suffix before the new stores are created. A chain of
another network or genesis, such as the earlier `iuna-devnet`, is set aside the
same way, while the wallet stays in use.
The operator token is passed to the local UI in a URL fragment, moved into
`sessionStorage`, and then immediately removed from the visible URL and browser
history entry.

## Read-only routes

- `GET /api/status` returns Heartbeat, Ledger, mempool, wallet and management
  availability. Wallet totals distinguish the complete confirmed balance from
  the spendable balance after local mempool reservations.
  `management_requires_token` tells a browser whether it needs the operator
  token, and `pow_limit` is the easiest allowed target (difficulty 1), so
  clients can chart difficulty per block.
- `GET /api/heartbeats` returns rebuildable screen projection data. The optional
  `limit` (maximum `200`) and exclusive `before_height` query parameters page
  backwards through both Chain-screen rails.
- `GET /api/metrics` returns per-Heartbeat historical snapshots for the Metrics
  screen. `?limit=100` and `?limit=1000` select recent ranges; omitting `limit`
  returns all projected rows. Metrics include intervals, difficulty, Ledger
  transactions, compact chain bytes, supply, UTXOs and cumulative Ledger blocks.
  Mining-ticket counts and budgets follow their active lottery slots: a ticket
  appears after its six-block confirmation delay for exactly its duration, and
  its budget is split over those points with the protocol's exact integer weight
  distribution.
- `GET /api/mempool` returns pending Ledger transactions and Heartbeat mining
  tickets in separate arrays so the Chain screen can present both layers in a
  single overview.
- `GET /api/wallet/activity` returns the node wallet's UTXOs, its pending
  transactions and tickets, and one 200-Heartbeat page of confirmed activity:
  what it sent, received or spent on tickets, plus Heartbeat and Ledger rewards
  won by this wallet. Pass `next_before_height` as the `before_height` query
  parameter to continue chronologically towards genesis.
- `GET /api/peer-targets` lists the outbound peers the node keeps dialling,
  with their source, connection state and latest error.
- `GET /api/mining-ticket-quote?duration=60` returns the consensus minimum
  expenditure budget both with and without a Ledger sidecar in the ticket's
  inclusion Heartbeat. Its recommended budget is the higher value and therefore
  remains valid across that inclusion-order uncertainty, including at halvings.
  It also returns the current active weight of other miners and the affordable,
  valid ticket budget that maximizes subsidy-only net EV against that weight;
  the budget is `null` when the unlocked wallet cannot fund the consensus minimum.

These routes do not expose signing or wallet mutation and remain available when
management is disabled.

## Public browser-wallet routes

These routes are opt-in and exist only on the separate listener configured with
`--wallet-endpoint`; they are never mounted on the operator listener.

The self-custody browser wallet uses four deliberately narrow routes:

- `GET /api/public/wallet/{64-character-address}` returns only that address's
  confirmed UTXOs, balance, spendable balance, historical `used` flag and
  current chain heights. The
  node keeps a per-owner index that it rebuilds once per new tip, so a lookup
  does not scan the whole UTXO set. Balances always cover every output; the
  UTXO list stops at `1,000` entries and then sets `utxos_truncated`.
- `POST /api/public/wallet-addresses` takes `{"addresses": [...]}` with 1 to
  `64` addresses and returns, in request order, each address's `used` flag,
  balance, spendable balance and UTXO count at one Heartbeat tip, plus the
  mempool transactions spending any of them. The browser wallet discovers its
  receive and change addresses with one such request per gap window and then
  loads UTXO pages only for addresses that still hold outputs.
- `POST /api/public/wallet-activity` takes the wallet's public key and complete
  discovered address set. It returns the same transfers, mining tickets,
  Heartbeat rewards and Ledger rewards as the operator wallet. Each transfer
  amount is the absolute wallet balance delta: outputs to those addresses minus
  the resolved inputs owned by those addresses. This includes change and fees
  correctly instead of treating all non-wallet outputs as the sent amount.
  Confirmed activity is paged by 200 whole Heartbeats; pass the returned
  `next_before_height` as `before_height` to continue without cutting off a
  busy Heartbeat or applying a separate limit per activity type.
- `POST /api/public/transactions` accepts a complete, client-side signed
  current-protocol `Transaction`. Consensus validation is performed before it
  enters the mempool and relay.

Every node mempool, for all submission paths, applies a relay policy on top of
consensus: a Ledger transaction must pay at least `1` unit per started `1,000`
canonical bytes. A typical transaction therefore needs a fee of `0.00000001
IUNA`. Free transactions are refused instead of filling the mempool. This is
policy, not consensus: a block containing a cheaper transaction is still
valid. Per-IP rate limiting belongs in the ingress in front of these routes.

These routes never receive a seed or password. Browser access is restricted to
`https://wallet.getiuna.org` and loopback origins for local testing. The deployment ingress exposes only `/api/public` on the
wallet host; management routes remain on the protected operator interface.

## Management routes

Management requests use `Authorization: Bearer <IUNA_OPERATOR_TOKEN>` and JSON
bodies.

On an ordinary loopback-only node without an operator token, the same routes are
available to the node's own local interface. Requests must carry an `Origin`
that exactly matches the loopback `Host` they were sent to, so pages on other
localhost ports and DNS-rebound sites are rejected. This mode still trusts every
local user and process that can reach the port, because a non-browser client can
set both headers; use an operator token on shared machines.

`IUNA_INSECURE_OPERATOR_API=1` enables the same tokenless mode on a non-loopback
`--http` address. It is meant only for the Docker Compose development network:
any container on that bridge network can drive the nodes' wallets and mining.
Compose publishes the HTTP ports only on `127.0.0.1`. Never set it on a node
holding real funds.

`POST /api/wallet` accepts one of five actions:

```json
{
  "action": "create | restore | unlock | backup | change_password",
  "password": "at least 10 characters for create and restore",
  "recovery_phrase": "required only for restore",
  "new_password": "required only for change_password"
}
```

`create` returns the new 24-word recovery phrase once, with `Cache-Control:
no-store`. Initial Setup requires a three-word check before it lets the user
continue. `restore` accepts the complete phrase and creates an encrypted wallet
with the same Ed25519 key. `unlock` opens an existing wallet file. Creation and
restoration refuse to overwrite an existing wallet. `backup` verifies the
wallet password and returns the existing wallet's phrase, also with
`Cache-Control: no-store`. `change_password` re-encrypts the same seed under a
new password and atomically replaces the wallet file; the old password stops
working and the recovery phrase stays the same.

The recovery phrase is the durable backup. The password only encrypts the local
wallet file and can be different when a wallet is restored on another device.
Never share the phrase or keep it in a screenshot.

A new password needs at least 10 characters, in the node and in the browser
wallet alike. Unlocking accepts any existing password, so wallets created under
the earlier 8-character minimum still open.

The phrase uses the standard BIP39 English word list and checksum. Its 32 bytes
of entropy directly define the stable mining key and domain-separated hashes of
that entropy derive the rotating receive and change keys. It does not use the
BIP39 seed derivation or BIP32/SLIP-0010 paths that other wallets apply, so the
same 24 words restore this iuna wallet in the node and the browser wallet but
give different keys, or nothing useful, in other wallet software.

`POST /api/ledger-transactions` accepts:

```json
{
  "recipient": "32-byte-address-commitment-as-hex",
  "amount": 100000000,
  "fee": 1
}
```

`POST /api/mining-tickets` accepts:

```json
{
  "budget": 10000000,
  "duration": 60,
  "fee": 1
}
```

`POST /api/hash-mining` with `{ "enabled": true, "workers": 4 }` starts the
node's continuous Heartbeat miner with that many CPU threads; `false` stops it.
Worker counts are accepted from 1 through 256; using more workers than available
CPU cores is allowed but normally does not improve throughput.
The miner never waits for the 60-second interval: pacing comes only from proof
of work and the difficulty adjustment. It builds work from the current ticket
and signed-candidate pools, or the canonical empty deadline fallback, refreshes
that work every 10 seconds or when a peer
extends the tip, searches the SHA-256 nonce space, persists an active tip and
broadcasts a compact Heartbeat. Its state is reported as `heartbeat_mining_enabled` by
`GET /api/status`. `POST /api/hash-mining/heartbeat` remains available as a
low-level single-block operator endpoint.

`POST /api/ledger-mining` with `{ "enabled": true }` enables continuous Ledger
mining. The node immediately buys a ten-Heartbeat ticket at the subsidy-only
net-EV-maximizing budget against the current active weight of other miners, with
the consensus-recommended budget as its lower bound and available wallet inputs
as its upper bound. It recalculates that budget
before buying another after each ten-Heartbeat interval. This preference and
the last purchase height survive restarts; it runs without an open browser. The
state is reported as `ledger_mining_enabled` by `GET /api/status`.

Whenever the wallet is unlocked, the node also watches its active tickets. If
one wins before its deadline and pending transactions exist, the wallet signs
the exact candidate body and the node relays it. This signing does not depend on
continuous ticket purchases being enabled. Heartbeat miners may include that
candidate unchanged; at the third inclusion slot they instead build the
mandatory empty unsigned fallback.

Continuous Ledger mining also performs bounded UTXO housekeeping. An automatic
ticket may consume up to `32` confirmed, unreserved wallet outputs and returns
their excess as one change output. It preserves the largest output whenever the
remaining outputs can fund the ticket, keeping liquid funds available while the
ticket is pending. Manually submitted tickets retain minimal input selection.

On an otherwise untouched chain, enabling the first miner also spends the
public RFC 8032 genesis test output into a protocol-valid bootstrap mining
ticket. Its inclusion fee gives the first Heartbeat miner its first spendable funds;
the ticket then starts the Proof of Expenditure lottery. This is one-time
bootstrap behavior: Heartbeat Proof of Work itself has no fixed subsidy.

The node keeps two small files of local, non-consensus state in its data
directory. `mempool.json` holds pending transactions, tickets and signed candidates; it is written
at most every 5 seconds when the mempool changed and again on shutdown. At the
next start every entry goes through normal validation again, so anything that
was mined or became invalid meanwhile is dropped. `mining.json` remembers whether
Heartbeat mining was on and with how many workers; mining resumes after a
restart once the wallet is unlocked.

`chain.sqlite3` holds the canonical blocks and a few full state checkpoints.
These thin out with age, roughly at 1/2, 3/4 and 7/8 of the chain, so their number
grows with the logarithm of the height. They only speed up deep reorgs, and the
node deletes and rebuilds them on every start. Blocks of a deep fork wait in
memory, up to 256 MiB, until that branch has more work than the active chain.

`POST /api/peers` with `{ "action": "add", "address": "203.0.113.7:9444" }`
starts dialling another outbound peer; `"remove"` stops dialling it and closes
the session. Addresses are `IP:port` or `host:port`; a host name resolves on every connection attempt. Peers added
this way are saved in `peers.json` in the data directory and restored at the
next start, next to any `--peer` arguments. `--bootstrap-peer` fills `peers.json`
only when that file does not exist yet, so a removed bootstrap peer stays
removed. Without it a node uses the peers in `KNOWN_NODES.txt`, so a fresh
node finds the network on its own; `--no-bootstrap-peers` turns that off for a
private network. A node keeps at most `32` outbound
peers.

Accepted Heartbeats are re-advertised to other peers by inventory, while newly
accepted Ledger transactions and mining tickets are re-gossiped once. This lets
line and mesh topologies propagate data beyond a node's direct source peer
without creating endless echo loops.

The node generates the public lottery seed securely and uses the wallet public
key as the Ledger reward destination. Successful submissions return the object
ID and validated fee, reserve their inputs in the local mempool and relay the
object to connected peers.

Management routes return `401` for missing or invalid authentication, `423`
when the wallet is locked, `400` for invalid wallet actions, and `503` when
management is disabled.
