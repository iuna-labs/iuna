# Mainnet-candidate manifest

Status: **v0.4.18 preparation; not approved for promotion**

Recorded/reviewed by: Codex

Review date: 2026-09-07

## Network identity

| Field | Value | Source |
| --- | --- | --- |
| Genesis hash | `3d677cd7ced1c04d3a276cbee7ea38076e34ac65f18a2c9b8286a4872d986a9a` | Read-only live-chain audit captured 2026-09-05 |
| Network ID | `iuna-mainnet-candidate` | Frozen protocol constant and `docs/protocol.md` |
| Launch profile | `iuna-mainnet-candidate` | Live-chain audit and frozen protocol profile |
| Bootnode | `142.132.164.59:9444` | Deployment manifest |
| Candidate release tag | `v0.4.18` | To be created only after exact-tree gates and artifact builds pass |
| Candidate commit | Pending | Recorded in the external release evidence after the release commit is created |

The narrow live exposure check on 2026-09-07 found P2P port 9444 reachable.
Connections to optional Stratum port 3333 and management port 18661 timed out.
The deployment manifest additionally places the management service behind an
IP allowlist.

## Release artifacts

No release artifact is approved yet. The files currently named v0.4.17 predate
the candidate work and belong to tag commit
`0b145227567c3432c414978ebaeb79ba892da695`:

| Existing artifact (not approved for `2db6cfb...`) | SHA-256 |
| --- | --- |
| `iuna-v0.4.17-linux-aarch64.tar.gz` | `9176cc6f7a149640395fd9dcd2ba9c2bc3aabed293c3aacf0c9cb8fbf3d97af4` |
| `iuna-v0.4.17-linux-x86_64.tar.gz` | `486d8c9b54cba4ac68e33a46403deaed9945e9a3ff94ffe02c6ce426295e9766` |
| `iuna-v0.4.17-macos-aarch64-desktop.app.zip` | `664c8c9e9b946efc29d597722e43d185141217cc68f5ef57e1b1320b330bb72f` |
| `iuna-v0.4.17-windows-x86_64-desktop-setup.exe` | `3ea1c5d99f990b2a1cf7f5da90f5ad1071270c63e212e7b0f97cd0c6b9218c1f` |

## Promotion blockers

- Run the complete release gate against the final v0.4.18 tracked tree.
- Create the release commit and annotated tag without changing that tested tree.
- Build Linux x86_64/aarch64, macOS aarch64, and Windows x86_64 artifacts from
  that tag; archive platform build logs and publish new checksums.
- Publish the full logs indexed by `docs/release-evidence-2db6cfb3.md` with the
  candidate release rather than relying on the ignored local directory.
- Obtain independent review of every security sign-off area and this manifest.
- Run the optional `chiavdf` compatibility gate on a macOS host with the
  reference package installed.

Reset, rollback, and operator recovery instructions are maintained in
`docs/operator-playbooks.md`; consensus and activation rules are maintained in
`docs/protocol.md`.
