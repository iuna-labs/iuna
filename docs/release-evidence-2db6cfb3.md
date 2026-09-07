# Release evidence for `2db6cfb3cf6c92eaaf73be1ad547dd1d5c7dd6c7`

Captured on 2026-09-07. All commands ran against the same clean tracked tree.
The full logs are kept locally under
`release-evidence/2db6cfb3cf6c92eaaf73be1ad547dd1d5c7dd6c7/`; this index does not by
itself publish those ignored files.

| Evidence | Result | SHA-256 |
| --- | --- | --- |
| `dependency-audit.log` | Passed | `3d9f0591aba9a19d1ef8866db8071c92e663750a3599dc19242e281c37ba4a5d` |
| `cargo-test-locked.log` | Passed: 328 library and 71 binary tests | `f08370c5fcfc0dc3a29fd5c19e8cd642b1fb6b74879e97d038f9554a656d9319` |
| `fuzz-cargo-check.log` | Passed | `b1849640315593396c2db958d61d340ea0d7c7fdffee97f1116e29b643f520f1` |
| `bounded-fuzz-targets.log` | Passed: five 256-run targets and one 16-run VDF target | `01dbf3b9d58bae8f659c45ba4c5a1df7dc936dd663664dbc3b3d32bc77d1235f` |
| `adversarial-ignored.log` | Passed: 30 tests | `320daecd9cc329c993a4177bf132ecbdb53ad2718e60555cd54214720a1689da` |
| `release-properties-soak.log` | Invalid evidence: documented command selected zero tests | `72dd29f28aed34b26342484eb78afd2c54ba3cbe9df506d548ed3ce041baad58` |
| `release-properties-soak-with-e2e-feature.log` | Passed: 3 tests | `3a0e8b182ee66dda65283ef369ecfd2b6218763d2abac2a9e7622c85c0a40224` |
| `e2e-post-activation.log` | Passed | `91de8a086936b2fe9c7ff37c71d42eb446ceb388b43de435a332928fd0c28aec` |
| `macos-desktop-cargo-check.log` | Passed | `5321f216dd68d4dc5275bf6bef8324e8374e9a001dec7792205b37a4b9c11dfa` |
| `live-port-exposure.log` | P2P reachable; Stratum and management timed out | `8e154dc155efae5caadba01fdafd55512400cbe2b3932635351aba7ce7530ef0` |
| `20260907T021309.775044Z-sync-resilience/report.json` | Passed | `eb1134cc80dbebcbb9622b40cb17e15b2b7bf1af0d35ebe3120ca93f89e53d88` |
| `20260907T021309.775044Z-sync-resilience/nodes.log` | Captured | `358123ad35937a0522bfad939dcd05b6535e32c84ff12a4a88d285d1b8b7fb4c` |
| `20260907T022808.771423Z-partition-recovery/report.json` | Passed | `8442fe7318f146b63e2bad1bdfd901aab24e0bb27bd0c271c4ed432319d2cb4e` |
| `20260907T022808.771423Z-partition-recovery/nodes.log` | Captured | `f8ae1d04c08c55b9ae72260cb7c7ccf492086f8a5d77071f63bd890e490aa390` |

The sync report records convergence of all seven nodes at height 1087. The
partition report records independent island recoveries, convergence on the
height-1007 recovery block after healing, a persistent restart of node6, and a
rank-0 ticket block at height 1009.

The optional byte-for-byte `chiavdf` compatibility gate was not run because the
Python package was not installed on this host. Tagged Linux, macOS, and Windows
release artifacts were not rebuilt: the tested revision has no release tag and
must not overwrite or reuse the existing v0.4.17 artifacts.
