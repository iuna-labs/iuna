# Live Recovery Evidence

This note records a read-only audit of the local mainnet-candidate chain
snapshot. It does not contain wallet data, configuration, recovery phrases, or
private keys.

## Capture

- Captured at: `2026-09-05T17:24:50Z`
- Profile: `iuna-mainnet-candidate`
- Height: `1210`
- Tip: `441f59b34cac6df6253c7f6b2449369ab7c1c3065d065467022f6b52b067eeb8`
- Genesis: `3d677cd7ced1c04d3a276cbee7ea38076e34ac65f18a2c9b8286a4872d986a9a`
- Stable database-copy SHA-256:
  `fcf26863853ea51e0a865f330a2292fa2bcac6dc1cfaceb33a0880347a0d168e`
- Verification marker: valid under the current consensus ruleset

The audit command refuses a source database with a WAL file, copies a stable
source to a temporary database, decodes only that copy, and removes the copy and
its SQLite companion files afterward:

```sh
cargo run --locked --bin iuna-chain-audit -- \
  ~/.iuna/chain.sqlite3 \
  --output release-evidence/live-chain-audit-2026-09-05.json
```

## Observed recovery history

`parent gap` is the elapsed time from the preceding block to the recovery
block. `ticket delay` is measured from that recovery block to the first later
ticket block. Consecutive recovery rows therefore share the same eventual
ticket where applicable.

| Recovery height | Recovery hash | Parent gap (ms) | Next ticket height/rank | Next ticket hash | Ticket delay (blocks/ms) |
| ---: | --- | ---: | ---: | --- | ---: |
| 116 | `31d341ff29c24727ad9926850cc45401fa5f59378894f3d5b40980bc2e785495` | 5684830 | 117 / 0 | `5eafc133241dbc9235c522dab1659ba1b095cf9cede24e762ce1685209dff858` | 1 / 1457218 |
| 119 | `beba42e65f49fb5bb369920e420d6cfa1d6c8e2cb4051846fd06d2eb4171dd85` | 3600160 | 120 / 0 | `18b2d2be9b3a9f920bf1731bbdcbe064456c219953bbf7d3225c4fedb4c8ca21` | 1 / 2521415 |
| 134 | `02b9fd07eb39be179070e637c08621cf0342dfbcada20ca6a5b7ccceaedb9b53` | 3600608 | 135 / 0 | `cb2f106d309f6370ca9c3406c560d752f039c43a9493763b7cd4da09dfe5313c` | 1 / 182550 |
| 150 | `bf0fb179d209d420af7651483d7ac393eadcfdb5580f4a70fa3f8fa870d777ec` | 34350614 | 153 / 1 | `8fc33d01d0b603632c4aeb325bbdc9b587b499297b514fc6f97ec45492cb203b` | 3 / 8402100 |
| 151 | `1a6002adbfb2cecc95efe52707825dc62a032c9f1633c0fdcfcd7f0fe07544fd` | 3600714 | 153 / 1 | `8fc33d01d0b603632c4aeb325bbdc9b587b499297b514fc6f97ec45492cb203b` | 2 / 4801386 |
| 152 | `f521b171aff6023a42e0081b4aa2000fd5cabfcc1549aed86cd2cf11d83b380e` | 3600275 | 153 / 1 | `8fc33d01d0b603632c4aeb325bbdc9b587b499297b514fc6f97ec45492cb203b` | 1 / 1201111 |
| 189 | `7114c1a74d258c73fbe7f1d168c0ecdf628e9f73da529bb30fb3daa494a18e1e` | 3600028 | 190 / 1 | `49d1c9f7bb2aea87d117b9bd669b83839e215340c58d1b9bc297b99d7c5c3d83` | 1 / 1200000 |
| 232 | `b39c4ce092ad0e7aa1a01f6377895e1113acc5df7942badc6a6b1363903e5690` | 3600946 | 233 / 0 | `0be7c59ae4aafe1e1ef604dbb7f472402701e477662ef30cdd46111c664fbbb0` | 1 / 1034783 |
| 525 | `7290046cd2dbd18a7ca97ae1b708b92d4bd8bc0459cc206627b97916e965188e` | 3600273 | 528 / 0 | `a753f41a9fc77cc6226b49f06d51ea0e1b2ae5142b368acf8ac58590a48c7e09` | 3 / 16446423 |
| 526 | `df42e45217fd9e40d9c81709b92b13d0e45a75daf88d3d94dbd2542ab7da4a98` | 11892983 | 528 / 0 | `a753f41a9fc77cc6226b49f06d51ea0e1b2ae5142b368acf8ac58590a48c7e09` | 2 / 4553440 |
| 527 | `e423061e1075c70b439fb1378389733c8902dbabce79dacad4f4cd346d8c2e71` | 3600497 | 528 / 0 | `a753f41a9fc77cc6226b49f06d51ea0e1b2ae5142b368acf8ac58590a48c7e09` | 1 / 952943 |
| 788 | `b46bd39f637cfb213d177081a47673da106740edace3f78ce6f0d0f5a971b0be` | 3959212 | 789 / 0 | `61f3921223b84336e6ad84451865b8a1adbca16484fd44f86dbc5cfd788d991a` | 1 / 1845873 |

The 12 recovery blocks form 8 episodes. Every episode is followed by a normal
ticket block, including the two episodes containing three consecutive recovery
blocks. This is direct live-chain evidence that the recovery path has restored
ticket production after observed stalls.

## Gate decision

This capture supports the recovery-liveness gate, but does not close it by
itself: block history cannot prove that disappearance of the selected
finalizers caused each stall. Correlated node logs or a controlled live soak are
still required for that attribution.

The multiple-recovery-candidate convergence gate also remains open. Consecutive
recovery blocks are not evidence of competing candidates. The process-level
3-3 partition test proves deterministic convergence in the accelerated e2e
environment, while live soak evidence is still required for the roadmap's live
gate.
