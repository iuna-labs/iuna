# End-to-end tests

The end-to-end suite exercises the current Iuna protocol directly. It does not
use committed chain snapshots: every run creates fresh temporary chain and UI
data and starts three real TCP peers in a line. A mining ticket and its containing
Heartbeat must both cross the middle node; the receiver must then resume the
persisted chain and tracked ticket after restart.

Run the complete gate:

```sh
./e2e/iuna_e2e.py
```

That runs the node, desktop, browser-wallet, website, and property tests first,
followed by the ignored TCP tests with `--nocapture`. To run just one part:

```sh
./e2e/iuna_e2e.py test
./e2e/iuna_e2e.py tcp
```

The TCP tests bind only ephemeral loopback ports and leave no repository data
behind. They live in the feature-gated `tests/properties.rs`, while the pure
property tests in `tests/consensus_properties.rs` run in every normal
`cargo test`. A TCP test can also be invoked directly:

```sh
cargo test --features e2e --test properties \
  three_nodes_regossip_persist_and_resume_a_heartbeat -- --ignored --nocapture
```
