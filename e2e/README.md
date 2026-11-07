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

That runs all unit and property tests first, followed by the ignored TCP test
with `--nocapture`. To run just one part:

```sh
./e2e/iuna_e2e.py test
./e2e/iuna_e2e.py tcp
```

The TCP test binds only ephemeral loopback ports and leaves no repository data
behind. It lives in `tests/properties.rs` behind the `e2e` Cargo feature so the
same test can also be invoked directly:

```sh
cargo test --features e2e --test properties \
  three_nodes_regossip_persist_and_resume_a_heartbeat -- --ignored --nocapture
```
