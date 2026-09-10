# Changelog

All notable changes to iuna are documented in this file. Releases are generated
from the Git history and Conventional Commit titles by `deployment.sh`.

## [Unreleased]

## [0.4.30] - 2026-09-10

### Added

- add signed automatic updates

### Fixed

- include updater public key in Docker context
- use Rustls for cross-platform releases
- roll back failed release preparation
- use native roots for updater TLS
- cancel competing recovery finalizers

## [0.4.29] - 2026-09-10

### Added

- protect in-memory secrets

### Fixed

- reduce app icon mark size
- harden cheap denial of service paths
- pin candidate genesis
- evict lower-fee packages under pressure
- keep adversarial difficulty cache consistent
- honor local profile during initial sync
- tolerate stale fork gossip

### Changed

- type validation control flow

### Documentation

- update thanks
- remove stale release planning artifacts

## [0.4.28] - 2026-09-09

### Fixed

- show all transaction types by default

### Maintenance

- automate complete changelog generation

## [0.4.27] - 2026-09-09

### Added

- allow custom fee rates
- filter and paginate transactions

### Fixed

- explain oversized transactions
- restore macOS tray icon

## [0.4.26] - 2026-09-08

### Added

- support multiple and watch-only wallets

### Changed

- Refine landing page flow and spacing
- Add clean monochrome tray icon
- Show complete reward fee flow in wallet

## [0.4.25] - 2026-09-08

### Added

- highlight light wallet on downloads page

### Fixed

- use English throughout wallet

### Changed

- Show fees as reward source
- Optimize wallet consolidation preview
- Fix post-recovery ticket resumption test
- Stabilize sync resilience deployment test

## [0.4.24] - 2026-09-08

### Added

- add lightweight browser wallet
- expose wallet rewards and dedicated host

## [0.4.23] - 2026-09-07

### Added

- add public wallet endpoint

## [0.4.22] - 2026-09-07

### Changed

- Show wallet optimization only when relevant
- Show peer network issues as warnings
- Improve dashboard status details

## [0.4.21] - 2026-09-07

### Changed

- Add guided wallet UTXO optimization

## [0.4.20] - 2026-09-07

### Changed

- Restore automatic recovery liveness

## [0.4.19] - 2026-09-07

### Changed

- Start fallback VDFs before rank slots

## [0.4.18] - 2026-09-07

### Changed

- Test post-activation recovery convergence
- Fix P2P convergence after recovery partitions
- Add process-level P2P partition recovery gate
- Preserve P2P partition release evidence
- Audit live recovery chain evidence
- Add process-level sync resilience gate
- Update next release readiness milestone
- Refresh homepage positioning and roadmap

## [0.4.17] - 2026-09-05

### Changed

- Add compact Discord link to app sidebar
- Check for updates more often

## [0.4.16] - 2026-09-04

### Changed

- Refine development leaderboards UI

## [0.4.15] - 2026-09-04

### Changed

- Remove PLAN.md
- Show compact storage bytes in explorer
- Add chain storage and proof metrics
- Refine chain and leaderboard interactions

## [0.4.14] - 2026-09-04

### Changed

- Avoid finalizer anchor conflicts with pending transfers

## [0.4.13] - 2026-09-04

### Changed

- Fix ticket block size estimation
- Fix orphaned finalizer burn recovery
- Speed up and stabilize release soak
- Parallelize adversarial release tests

## [0.4.12] - 2026-09-03

### Changed

- Modernize website section navigation
- Add Cloudflare analytics to website build
- Add unified branding and site SEO
- Improve homepage scroll gesture handling
- Fix homepage scroll responsiveness
- Make wallet addresses open contact editor

## [0.4.11] - 2026-09-03

### Changed

- Trust locally verified chain across releases

## [0.4.10] - 2026-09-02

### Changed

- Add desktop tray background mode
- Fix Windows tray cross-build dependencies

### Documentation

- remove legacy protocol guidance

## [0.4.9] - 2026-09-02

### Fixed

- fit homepage title on mobile
- remove dashboard viewport overflow

### Changed

- Use mobile brand icon styling on desktop
- Restore brand icon hover animation
- Add non-moving menu click feedback

### Documentation

- simplify current protocol guide

## [0.4.8] - 2026-09-02

### Changed

- Make post-1000 release testing the default
- Refresh public site and live candidate docs
- Add website-only deployment mode
- Add management dashboard
- Refine homepage messaging
- Remove source build homepage link
- Skip repeated local chain VDF verification
- Update committee adversarial rejection tests
- Refine homepage visual design

## [0.4.7] - 2026-09-01

### Changed

- Queue burns for one block
- Extend post-activation burn E2E
- Run long deployment tests by default

## [0.4.6] - 2026-08-31

### Changed

- Improve transfer confirmation and address labels
- Bind burns to the current chain tip

## [0.4.5] - 2026-08-31

### Changed

- Add docker e2e checkpoints
- Fix transfer confirmation in production UI
- Add checkpoint e2e test scenarios

## [0.4.4] - 2026-08-30

### Changed

- Remove redundant management UI button
- Harden first-run password setup
- Bind transactions to chain identity
- Activate chain-bound signing at height 1000
- Add checksummed network addresses
- Add objective finality at height 1000
- Fix finalization stalls and committee grinding
- Fix vulnerable desktop dependencies
- Fix automatic burn fee rate handling
- Fix mine transaction replay inflation
- Fix cross-platform release builders

## [0.4.3] - 2026-08-29

### Changed

- Improve P2P chain synchronization
- Ignore .scratch

## [0.4.2] - 2026-08-28

### Changed

- Fix stale Windows installer selection
- Make management UI mobile friendly

## [0.4.1] - 2026-08-27

### Fixed

- resist block grinding from height 1000
- recover from forked block pages

### Changed

- Preserve client IPs for admin allowlist
- Rebuild incompatible UI caches on startup

### Performance

- fit large proofs to checkpoint budget

## [0.4.0] - 2026-08-26

### Changed

- Document fee distribution
- Validate reveal bundles before conflict handling
- Reset protocol around public burn bundles
- Add Stratum endpoint settings
- Harden burn bundle relay checks
- Add Stratum DoS limits
- Add fork snapshot adversarial tests
- Harden compact snapshot decoder
- Add supply invariant adversarial tests
- Harden HTTP auth abuse checks
- Add multi-node adversarial network test
- Add release-mode soak test
- Document operator failure playbooks
- Document mainnet candidate rehearsal
- Add operator health metrics
- Freeze mainnet candidate parameters
- Document candidate promotion path
- Add attack economics adversarial tests
- Add fuzzing plan and harnesses
- Run fuzz smoke checks during deployment
- Add mini-validator block precheck oracle
- Add mini-validator supply oracle
- Fix duplicate ticket creation with mini-validator
- Complete mini-validator consensus oracles
- Add economic sweep runner
- Model economic sweep dimensions
- Add performance budget adversarial tests
- Add partition chaos adversarial tests
- Harden persistence crash consistency
- Add candidate promotion rehearsal
- Document candidate upgrade rollback flow
- Document security review checklist
- Add wallet config fuzz target
- Add P2P and Stratum performance budget tests
- Cover local chain reset persistence
- Cover combined economic sweep pressure
- Cover recovery block oracle rules
- Align release fuzz gates
- Validate release fuzz run override
- Cover release fuzz gate wiring
- Remove release gate text tests
- Cover future block P2P policy
- Cover atomic P2P block batch rejection
- Add local Docker testnet startup passwords
- Configure local testnet mining roles
- Remove obsolete fee penalty style
- Improve Docker setup and block loss accounting
- Enforce rank-specific burn committee rewards
- Optimize Rust VDF prover
- Add chiavdf compatibility test
- Move long adversarial tests out of pre-commit
- Review wallet security for candidate
- Stabilize chain persistence test
- Rebroadcast local burn bundles
- Request missing burn bundles over gossip
- Keep candidate protocol version at one
- Simplify burn bundle gossip docs
- Fix genesis finalization display
- Limit setup peer discovery dials
- Fix testnet burn defaults and bundle quorum UI
- Fix local burn committee quorum handling
- Restore conservative genesis burn defaults
- Improve local testnet burn defaults and warnings
- Expand ticket-backed burn committees
- Cancel stale VDF work
- Project UI data outside requests
- Page P2P chain synchronization
- Block management UI while syncing
- Deduplicate automatic finalization skip logs
- Make metrics projection incremental
- Report incremental block sync progress
- Add burn bundle selection details
- Add network reset migration flow
- Prepare candidate network messaging and admin access
- Show block reward fee flow
- fix local testnet profile for compose nodes
- Add website service to Docker Compose
- Simplify VDF benchmark documentation
- Test and harden protocol invariants
- Keep admin IP allowlist outside version control
- Invalidate stale sync work on chain reset
- Use app version for UI cache busting
- Optimize compact block storage
- Use compact storage size for block consensus
- Archive legacy chain databases on startup
- Support genesis deployments with fresh PVCs
- Recreate permanent PVC for genesis deployments
- Harden genesis deployment rollout
- Fix adversarial fixture ticket IDs
- Speed up release checks and update fuzz lock

## [0.3.10] - 2026-08-18

### Changed

- Harden reveal committee lineage selection
- Style markdown files in git browser

## [0.3.9] - 2026-08-17

### Changed

- Harden p2p block import and mempool sizing
- Activate reveal bundle signature thresholds
- Fix automatic burn fees after activation
- Lower long-test budgets

## [0.3.8] - 2026-08-16

### Changed

- Test zero-fee blinded creation rejection
- Fix recovery finalization liveness
- Harden peer discovery address handling
- Constrain block detail transaction list height

## [0.3.7] - 2026-08-15

### Changed

- Add testnet chaos property coverage
- Add project roadmap
- Add max send amount button
- Block zero-fee spam after activation
- Add devnet node runner callout

## [0.3.6] - 2026-08-14

### Changed

- Add adversarial reveal and p2p regression tests
- Gate reveal fee mask attribution at height 750

## [0.3.5] - 2026-08-14

### Changed

- Add development mode metrics UI

## [0.3.4] - 2026-08-14

### Changed

- Reject transactions too large for blocks
- Activate unique-owner reveal committees at height 500
- Fix deployment bug
- Refine downloads page layout

## [0.3.3] - 2026-08-13

### Changed

- Remove redundant build step
- Show reveal metrics and finalizer status
- Improve mobile homepage hero

## [0.3.2] - 2026-08-13

### Changed

- Make windows build cache persistent
- Use getiuna.org domain
- Support re-deploying
- Add redirect to new domain
- Polish static source browser
- Enable external links from desktop UI
- Fix version update badge comparison
- Specify thanks
- Move address book form into recipient picker
- Improve deployment script

## [0.3.1] - 2026-08-12

### Changed

- Fix release artifact container path
- Build Windows desktop artifact in Docker
- Use socket address for deployed node announce
- Add new admin UI endpoint
- Fix static git clone over HTTP
- Highlight Discord links on homepage
- Invalidate missed fallback tickets

## [0.3.0] - 2026-08-12

### Changed

- Add README community badges
- Fix VDF modulus and reset consensus gates
- Use crate PBKDF2 for auth passwords
- Remove CI test
- Add static deployment site
- Fix deployment version replacement
- Preserve deployment replacement captures

## [0.2.47] - 2026-08-11

### Changed

- Add thanks page
- Improve reveal bundle timing and UTXO selection
- Fix UTXO range selection clicks
- Improve UI data cache consistency

## [0.2.46] - 2026-08-11

### Changed

- Fix initial block rail rendering
- Show chain reset result as global flash

## [0.2.45] - 2026-08-11

### Changed

- Improve block loading skeleton
- Use warm UI cache for chain endpoints
- Persist UI chain indexes
- Request snapshot for single block fork errors
- Add local chain reset action

## [0.2.44] - 2026-08-11

### Changed

- Improve UI polling performance

## [0.2.43] - 2026-08-11

### Changed

- Split large modules
- Improve initial chain and metrics loading

## [0.2.42] - 2026-08-10

### Changed

- Improve gossip peer exchange discovery

## [0.2.41] - 2026-08-10

### Changed

- Add UI-configurable P2P bind port
- Improve UI chain data performance

## [0.2.40] - 2026-08-10

### Changed

- Aggregate finalizer fees at activation height

## [0.2.39] - 2026-08-10

### Changed

- Prune stale inbound peers
- Harden UI polling after idle
- Show wallet transaction timestamps
- Add wallet address book
- Defer heavy UI refreshes by tab
- Improve mining event log feedback

## [0.2.38] - 2026-08-09

### Changed

- Allow two mine actions per anchor

## [0.2.37] - 2026-08-09

### Changed

- Limit mine actions per anchor

## [0.2.36] - 2026-08-08

### Changed

- Add configurable PoW mining workers

## [0.2.35] - 2026-08-08

### Changed

- Improve local mining status and PoW resilience
- Stop automatic PoW worker when disabled

## [0.2.34] - 2026-08-08

### Changed

- Separate finalizer anchor burns

## [0.2.33] - 2026-08-08

### Changed

- Use default burn settings for genesis mode

## [0.2.32] - 2026-08-07

### Changed

- Address testnet feedback UI and recovery settings
- Add sparse network chaos stability test
- Improve mempool ordering and block byte breakdown

## [0.2.31] - 2026-08-07

### Changed

- Highlight new mempool items
- Make automatic burns preserve configured amount
- Increase fallback finalizer slot delay
- Count blinded inputs in circulation until reveal

## [0.2.30] - 2026-08-07

### Changed

- Fix metrics accounting and desktop sleep

## [0.2.29] - 2026-08-06

### Changed

- No notable changes recorded.

## [0.2.28] - 2026-08-06

### Changed

- No notable changes recorded.

## [0.2.27] - 2026-08-06

### Changed

- No notable changes recorded.

## [0.2.26] - 2026-08-06

### Changed

- No notable changes recorded.

## [0.2.25] - 2026-08-05

### Changed

- No notable changes recorded.

## [0.2.24] - 2026-08-05

### Changed

- No notable changes recorded.

## [0.2.23] - 2026-08-05

### Changed

- No notable changes recorded.

## [0.2.22] - 2026-08-05

### Changed

- Refine blinded mempool protocol

## [0.2.21] - 2026-08-05

### Changed

- Split blinded fees with reveal list makers

## [0.2.20] - 2026-08-05

### Changed

- Add reveal bundle committee
- Compact reveal bundle storage

## [0.2.19] - 2026-08-04

### Changed

- Persist owned blinded transactions

## [0.2.18] - 2026-08-04

### Changed

- No notable changes recorded.

## [0.2.17] - 2026-08-04

### Changed

- No notable changes recorded.

## [0.2.16] - 2026-08-04

### Changed

- No notable changes recorded.

## [0.2.15] - 2026-08-04

### Changed

- No notable changes recorded.

## [0.2.14] - 2026-08-04

### Changed

- No notable changes recorded.

## [0.2.13] - 2026-08-03

### Changed

- Queue automatic burns as blinded mempool items

## [0.2.12] - 2026-08-03

### Changed

- Require plaintext burn in blinded blocks

## [0.2.11] - 2026-08-03

### Changed

- Remove burn claims
- Add blinded transaction commit reveal

## [0.2.10] - 2026-08-03

### Changed

- Add iuna pronunciation to homepage
- Split claimed burn fees with attesters
- Add burn claim metrics screen

## [0.2.9] - 2026-08-03

### Changed

- Document protocol overview
- Fix burn leader ranks modal data mapping
- Enforce finalizer rank time slots

## [0.2.8] - 2026-08-02

### Changed

- Avoid visible loaders during polling
- Refresh chain data after setup completion
- Add initial setup node mode choice

## [0.2.7] - 2026-08-02

### Changed

- Add required burn for mine actions
- Clarify PoW mining controls
- Add recovery finalization path
- Add burn inclusion claims
- Remove required burn mine reward setting
- Show burn leader ranks in block detail
- Fix desktop app icon
- Normalize fallback VDF retarget samples

## [0.2.6] - 2026-07-31

### Changed

- Paginate management datasets
- Run automatic PoW mining independently

## [0.2.5] - 2026-07-31

### Changed

- Simplify README and landing page copy
- Clarify landing page join options
- Verify announced P2P address ownership
- Make inbound P2P explicitly public
- Close transient P2P verification sessions
- Show verified inbound peer addresses

## [0.2.4] - 2026-07-31

### Changed

- Update website join options
- Add configurable P2P announce address
- Dampen VDF retarget oscillation
- Use BIP-39 wallet seed phrases
- Add metrics block range filter
- Prefill setup bootstrap peer

## [0.2.3] - 2026-07-30

### Changed

- Fix metrics chart bootstrap points
- Allow VDF rounds above legacy limit
- Split transaction gossip batches

## [0.2.2] - 2026-07-30

### Changed

- Add optional blockchain metrics tracking

## [0.2.1] - 2026-07-30

### Changed

- Add settings screen
- Reject zero enabled burn rate

## [0.2.0] - 2026-07-29

### Changed

- Add desktop release artifacts
- Fix Windows desktop artifact upload
- Fix macOS desktop app signing

## [0.1.14] - 2026-07-29

### Changed

- Clean up runtime logging
- Constrain transaction scroll areas
- Show disabled pending UTXOs
- Filter wallet transactions from backend
- Limit inbound P2P connections

## [0.1.13] - 2026-07-29

### Changed

- Harden mempool sync and auth
- Harden block timing with network-adjusted time
- Harden management UI requests
- Simplify management UI modes
- Fix network time replay edge cases
- Refine brand icon
- Resize management UI brand mark
- Highlight basic peer errors

## [0.1.12] - 2026-07-28

### Changed

- Expose transaction gossip rejections
- Validate protocol addresses for canonical fees
- Fix PoW mine rewards and bounded search
- Sync mempools through peer status

## [0.1.11] - 2026-07-28

### Changed

- Move web assets under www
- Clarify iuna homepage positioning
- Reject conflicting mempool gossip acknowledgements

## [0.1.10] - 2026-07-28

### Changed

- Clean up stale mine actions

## [0.1.9] - 2026-07-28

### Changed

- Run background sync in setup mode

## [0.1.8] - 2026-07-28

### Changed

- Fix stale outbound peer cleanup

## [0.1.7] - 2026-07-28

### Changed

- Fix p2p setup sync and private peer discovery

## [0.1.6] - 2026-07-28

### Changed

- Ignore private peer discovery addresses

## [0.1.5] - 2026-07-28

### Changed

- Remove public default port examples
- Fix setup peer genesis adoption

## [0.1.4] - 2026-07-27

### Changed

- Improve peer management UI
- Add network health overview
- Track peer activity freshness
- Add peer bans and version notice
- Add bootstrap peer setup

## [0.1.3] - 2026-07-27

### Changed

- Fix release publishing without checkout

## [0.1.2] - 2026-07-27

### Changed

- Use current macOS Intel release runner
- Clarify homepage join options

## [0.1.1] - 2026-07-27

### Changed

- Add Linux ARM release artifact

## [0.1.0] - 2026-07-26

### Changed

- Initial commit
- Harden consensus and refresh node UI
- Move runtime configuration into UI
- Require burn transaction before VDF mining
- Move initial setup into UI config flow
- Add recovery phrase wallet setup
- Change default HTTP UI port
- Make genesis VDF calibration adaptive
- Improve setup seed verification feedback
- Reward genesis starter and show block miners
- Keep burn rate edits stable during polling
- Persist burn rate configuration
- Add fees and block size limits
- Move mining settings into mining screen
- Refine transaction card styling
- Validate wallet send form
- Rename project to Luun
- Add rolling burn ticket window
- Clarify wallet setup recovery phrase
- Require fresh wallet for genesis
- Add mining economics break-even
- Add P2P diagnostics and fix partial reads
- Load wallet transaction history from backend
- Migrate ledger transactions to UTXO model
- Guard against repeated wins from one burn ticket
- Add transaction UTXO detail modal
- Add UTXO coin selection regression tests
- Improve transaction visibility and p2p delivery
- Use microluun amount units
- Add manual UTXO selection for transfers
- Fix advanced UTXO transfer form
- Add UTXO selection shortcuts
- Add PoW mine actions for issuance
- Use mine action for genesis issuance
- Move genesis creation into chain UI
- Revert "Move genesis creation into chain UI"
- Revert "Use mine action for genesis issuance"
- Refine mining controls and PoW issuance
- Add miner-chosen mine fees
- Add management UI authentication
- Add wallet security and fee-rate mining UI
- Add Stratum mining support
- Add fallback block finalizers
- Move public site to GitHub Pages
- Add property tests for chain invariants
- Clarify Luun website USP
- Rename project to iuna
- Polish iuna docs and Pages setup
- Add release builds and fix Pages setup
- Update GitHub Actions to Node 24 versions
- Set block target to ten minutes
