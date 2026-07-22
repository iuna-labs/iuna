# 013 - Chain Persistence

Until now the chain lived in memory. That made tests nice, but restarts were too fragile: a node could keep its wallet and still forget what chain it was on.

The new piece is a SQLite adapter that stores the latest validated chain snapshot in the node's data directory. On startup, if that database exists, the node loads it before looking at `--start` or `--join`. So a restart keeps following the same chain instead of creating a fresh genesis or needing the bootstrap peer to be online at exactly that moment.

This is still intentionally small. It saves one current snapshot, not a fully indexed block database. But it sits in the adapter layer, away from the ledger rules, and it is tested separately. That gives us the boring restart behavior now while leaving room to grow it into a richer block store later.

Persistence runs in the background and only saves when the tip changes. The web UI, gossip loop, and miner should not care that SQLite exists.
