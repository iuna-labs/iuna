# Mainnet-candidate vervolgplan

Dit plan zet de resterende promotion-critical werkzaamheden in uitvoerbare
volgorde. Testbewijs en live bewijs worden apart bijgehouden: een groene lokale
test is nodig, maar sluit een live-soak gate niet automatisch.

## 1. Procesniveau P2P-partitietest

- [x] Voeg een versnelde echte TCP/P2P-partitietest toe aan de Rust e2e-suite.
- [x] Splits de zes Docker-nodes fysiek in twee groepen van drie.
- [x] Laat beide groepen onafhankelijk een recovery-block accepteren.
- [x] Herstel het netwerk en verifieer dat alle nodes op dezelfde tip convergeren.
- [x] Herstart een node met zijn bestaande persistente data.
- [x] Verifieer dat na recovery weer een normaal ticketblock wordt geproduceerd.
- [x] Neem het scenario op in de verplichte post-activation release-gate.
- [x] Bewaar bij falen en bij een candidate release de relevante node-logs en
      tip/checkpoint-samenvatting.

## 2. Live recovery-bewijs

- [x] Analyseer de chain-database onder `~/.iuna` uitsluitend read-only.
- [x] Leg recovery-hoogtes, hashes, voorafgaande stall en eerstvolgende
      ticketblock vast zonder walletmateriaal of secrets te kopiëren.
- [x] Bepaal welke recovery-gates hiermee objectief gesloten kunnen worden.
- [x] Laat gates voor meerdere recovery-candidates open tenzij de live historie
      of de procesniveau-partitietest dit daadwerkelijk bewijst.

Resultaat: [de live-chain-audit](docs/live-recovery-evidence.md) vond 12
recovery-blocks in 8 episodes en na iedere episode hervatte een ticketblock.
De roadmap-gates blijven bewust open omdat de chainhistorie niet bewijst welke
finalizers tijdens de stalls offline waren en geen concurrerende live
recovery-candidates bevat.

## 3. Sync-bewijs

- [x] Test een lege node die zonder handwerk vanaf genesis synchroniseert.
- [x] Test een stale node vanaf een oud snapshot via range/fork sync.
- [x] Test onderbreking en herstart tijdens beide sync-paden.
- [x] Archiveer hoogtes, tip-hashes, doorlooptijden en foutlogs als
      release-evidence.

Resultaat: de versnelde zeven-node `sync-resilience`-gate onderbrak een lege
bootstrap vóór de eerste persistente snapshot en hervatte tot zeven-node
convergentie op hoogte 1032. Daarna synchroniseerde dezelfde node vanaf hoogte
299, werd tijdens actieve range-validatie met een persistente tussenstand op
hoogte 811 afgebroken, en convergeerde na herstart met alle nodes op hoogte
1059.

## 4. Release- en security-sign-off

- [x] Draai alle gates uit `docs/security-review.md` op exact dezelfde revision.
- [ ] Bewaar dependency-, test-, fuzz-, e2e- en platform-buildlogs.
- [x] Vul reviewers, datum, resultaat en restrisico in voor consensus,
      transacties/mempool, P2P, Stratum, release-evidence en candidate manifest.
- [ ] Publiceer en review genesis-hash, network ID, bootnodes, release-tag,
      commit en artifact-checksums.

## 5. Promotie en hard-forkproces

- [ ] Leg het besluit vast om de candidate chain wel of niet zonder nieuwe
      genesis te promoveren.
- [ ] Documenteer protocolversies, activatiehoogtes, compatibiliteitsregels,
      rollout, rollback en noodprocedure voor toekomstige hard forks.
- [ ] Voer tijdens het launch-window alleen promotion-critical wijzigingen door.

## 6. Atomic BTC swaps

- [ ] Werk protocol, threat model en failure/recovery flows uit zonder de
      bevroren candidate-consensusregels te wijzigen.
- [ ] Bouw eerst een geïsoleerde testnet/prototype-implementatie.
- [ ] Plan activatie pas na promotion, security-sign-off en een vastgesteld
      hard-forkproces.

## Eerstvolgende definitie van klaar

Stap 4 is klaar wanneer alle gates uit `docs/security-review.md` op exact
dezelfde revision zijn gedraaid, de bijbehorende logs zijn gearchiveerd, iedere
sign-offrij een reviewer, datum, resultaat en restrisico bevat, en het candidate
manifest met genesis-hash, network ID, bootnodes, release-tag, commit en
artifact-checksums is gepubliceerd en gereviewd.
