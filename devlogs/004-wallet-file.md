# Devlog 004: Wallet File

The node no longer has a baked-in dev wallet seed.

On first real startup, `--start` or `--join`, iuna creates a wallet file and reuses it next time. The default is `.iuna/wallet.json`, or `<data-dir>/wallet.json` when a node uses its own data directory.

That matters for friend testing. You can restart your node and keep the same address, but friends do not need to pass a seed just to be someone else. They join your chain, get their own fresh local wallet, and start with 0 IUNA until you send them some.

This is still prototype-wallet simple: the file contains the seed, so it should be treated like a private key.
