# Changelog

All notable changes to Iuna are documented here.

## Unreleased

### Protocol

- Establish the canonical Iuna genesis and `iuna-devnet` network identity.
- Introduce one-minute Heartbeat blocks secured by hash-based proof of work.
- Introduce Ledger blocks every ten Heartbeats, selected through proof of
  expenditure mining tickets.
- Use Ed25519-only accounts and signatures with protocol-specific domains.
- Define exact chainwork, fork choice, ticket scoring, monetary limits,
  subsidy halvings, and canonical serialization rules.

### Node

- Persist consensus chain state and screen projections in separate SQLite
  stores behind repository ports.
- Relay Heartbeats, Ledger transactions, and mining tickets over the compact
  P2P protocol.
- Provide the Overview, Wallet, and Mining screens plus authenticated local
  management routes.
- Preserve the desktop wrapper, local Compose workflow, unit/property tests,
  and live two-node end-to-end test runner for the canonical protocol.
