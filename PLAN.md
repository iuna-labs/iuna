# Review follow-up: Heartbeat/Ledger consensus WIP

Findings from the review of the uncommitted consensus/UI work. Each phase is
verified (`cargo test`, `node --test tests/*.cjs`) before the next one starts.

## Phase 1 — Security and correctness (backend)

- [x] 1.1 Trusted loopback management: require a browser `Origin` that equals the
      node's own loopback `Host` (blocks requests without Origin, other localhost
      ports and DNS rebinding). `src/adapters/consensus_http.rs` `authorize`.
- [x] 1.2 Document `IUNA_INSECURE_OPERATOR_API` and what it trusts
      (`docs/operator.md`, README).
- [x] 1.3 Fix failing test `block_details_expose_transactions_from_a_ledger_sidecar`
      (ticket budget below consensus minimum).
- [x] 1.4 Do not serve the wallet from the main site root (`getiuna.org/wallet/`):
      no wallet CSP there and analytics share the origin. `nginx.conf`.
- [x] 1.5 Public wallet view uses a per-owner UTXO index rebuilt once per tip and
      returns at most 1,000 UTXOs; mempool relay policy needs 1 unit per started
      1,000 bytes. Open: per-IP rate limiting in the wallet ingress (Traefik).
- [x] 1.6 Move `GENESIS_SECRET` out of the consensus core / gate on the network.
- [x] 1.7 Make the Heartbeat nonce search cancellable (shutdown, new peer tip,
      disabled mining, 10 s template refresh).
- [x] 1.8 Align password minimums: 10 characters for new passwords everywhere
      (enforced in the wallet store); unlocking accepts existing passwords.
- [x] 1.10 Heartbeat pacing comes from PoW only: removed the miner's 60 s wait,
      timestamp = max(now, MTP + 1), relative ASERT (parent target × 2^((solvetime −
      60)/τ), rounded to nearest) instead of genesis-anchored ASERT.
      Production values: τ = 60 Heartbeats (1 h; was 288, too slow to follow hashrate swings), future drift 10 min, PoW limit =
      genesis target = 26 zero bits (Bitcoin-style difficulty 1, ~60 s per ~1 MH/s
      thread), pinned genesis nonce/hash; unit tests use regtest-like 12-bit params.
      **Consensus-breaking / new genesis hash:** reset local Docker volumes and the
      k8s node data.
- [x] 1.9 Document that the recovery phrase uses BIP39 entropy directly as the
      Ed25519 seed (not importable in other wallets).

## Phase 2 — Browser wallet bugs

- [x] 2.1 Escape all server values rendered via `innerHTML` (`wallet/app.js`).
- [x] 2.2 `requestJson` must fail on non-JSON responses instead of returning `{}`.
- [x] 2.3 Import the signing key as non-extractable; clear `pendingSeed` on lock;
      idle auto-lock; pause polling while hidden. Recovery phrase is no longer kept
      in memory (password required to view), random check words, clipboard
      fallback, and polling no longer re-renders over the send form/backup screen.
- [x] 2.4 Remove empty `wallet/crypto/` and `wallet/crypto-wasm/` leftovers.
- [x] 2.5 Add a Rust-generated signature test vector to `tests/wallet-app.test.cjs`.

## Phase 3 — Node UI bugs

- [x] 3.1 Clipboard fallback when `navigator.clipboard` is unavailable.
- [x] 3.2 Setup wizard: real connection state instead of always "Connected";
      randomise recovery-check word positions.
- [x] 3.3 "Chain composition" Heartbeat bar is hard-coded to 100%.
- [x] 3.4 Flash messages `aria-live`, `aria-current` on nav, dialog focus handling,
      `prefers-reduced-motion`.
- [x] 3.5 Operator token entry screen for non-loopback/desktop use.
- [x] 3.6 Polling: pause when hidden, back off on errors.
- [ ] 3.7 Alpine CSP build so `'unsafe-eval'` can be dropped from the node CSP.
      `'unsafe-inline'` is already gone from `script-src`. Deferred, by choice: the
      CSP build only accepts bare property/method names, and 244 of the 404
      bindings in `www/node.html` use operators, template strings or arguments.
      The UI renders no HTML from data (no `x-html`/`innerHTML`), so the gain is
      small against a large regression risk.
- [x] 3.8 `/api/status` exposes `management_requires_token`; the UI distinguishes
      "node offers management" from "this browser may manage" (token mode), and
      the setup wizard accepts the operator token. Metrics exclude genesis from
      the average Heartbeat interval.

## Phase 4 — Port lost UX from the previous interface (protocol-independent)

- [x] 4.1 Transaction history + detail, mempool panel, UTXO overview, "Max".
- [x] 4.2 Confirm-transfer step before sending.
- [x] 4.3 Address book (browser-local contacts, recipient picker, names in activity
      and confirm dialog) and clickable addresses.
- [x] 4.4 Keyboard shortcuts (Cmd/Ctrl+number, Escape closes modals).
- [x] 4.5 Interface tour (spotlight), separate from the setup wizard.
- [x] 4.6 Metric charts (SVG line charts with tooltips) for Heartbeats/Ledger.
- [x] 4.7 Peer management (add/remove, details, failure diagnostics).
- [x] 4.8 Settings: change password, peer networking (P2P screen) and a sync notice
      when peers are ahead. Open: danger zone (delete local chain).
- [x] 4.9 Dashboard state classes (good/warn/error).
- [x] 4.10 Web wallet: activity list (local sent log + incoming outputs), multiple
      wallets with migration, endpoint settings screen, confirm step and Max.
- [x] 4.11 Shared design tokens with `www/index.html` (CSS variables) for node UI
      and wallet.
- [x] 4.12 Replace heading-grep UI test with behaviour-level checks.

Dropped deliberately (old protocol only): burn bundles/outlook/leaderboards,
finalizers, quantum migration (wasm crypto), network migration.

## Known gaps found while testing

- [x] The mempool is saved to `mempool.json` and revalidated on start.
- [x] Heartbeat mining resumes after a restart (`mining.json`) once the wallet
      is unlocked.
