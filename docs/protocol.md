# iuna protocol in simple terms

iuna is an experimental devnet protocol that combines three mechanisms:

- **Burn lottery:** burning IUNA creates tickets for future block finalization.
- **VDF timing:** the selected finalizer must do sequential delay work before publishing a block.
- **Proof-of-work issuance:** new IUNA enters the chain through PoW mine actions.

The goal is to avoid relying on only one scarce resource. Proof-of-work chains tend to centralize around hardware and cheap energy. Proof-of-stake chains tend to centralize around existing wealth and staking pools. iuna tries a split design: burns choose who finalizes blocks, VDFs pace block production, and PoW keeps new issuance open to anyone who can find valid work.

Burns do not remove wealth advantage. More capital can still buy more lottery weight. The difference from stake is that burn power is paid again and again: it expires, does not unbond, and does not accumulate into a permanent stake position. The design converts wealth-bias from a growing asset into a recurring cost.

This is still experimental. The rules below describe the current devnet protocol, not a proven mainnet design.

## Coins and Transactions

iuna uses a UTXO-style ledger. The main transaction types are:

1. **Transfer:** moves IUNA from one address to another and pays a sender-chosen fee.
2. **Burn:** destroys an amount of IUNA, pays a sender-chosen fee, and creates a future lottery ticket.
3. **Mine action:** proves SHA-256-style PoW against the current chain tip. A valid mine action mints a fixed `1 IUNA` reward to its recipient and pays a fixed `1 IUNA` fee to the block finalizer.
4. **Blinded transaction envelope:** commits encrypted transaction content to a block before the finalizer can inspect whether it is a burn or transfer.
5. **Blinded reveal:** publishes the decryption key for a previously committed envelope so nodes can validate and execute the hidden transaction.

Burn and transfer fees are chosen by the sender. Mine action reward and mine action fee are deterministic protocol values.

## Burns Become Tickets

A burn does not immediately select its own block. Instead:

1. A burn is included in a block.
2. It becomes a ticket after the maturity delay.
3. The ticket stays eligible for a short expiry window.
4. Its lottery weight is the burned amount.

In the devnet profile, tickets mature after `3` blocks and remain eligible for `3` block heights.

The lottery draw for the next height is deterministic. Nodes rank all eligible burn tickets using the parent block hash, the parent VDF output, the target height, and the ticket amounts. More burned IUNA means more weight, but the winner is still drawn by the protocol.

## Finalizing Blocks

For each block height, eligible tickets are ranked:

- Rank `0` is the primary finalizer.
- Rank `1`, `2`, and later ranks are fallback finalizers.

The selected finalizer must prove ownership of the selected ticket, respect its rank time slot, and run the required VDF work. A block is valid only if the finalizer matches its ranked ticket, carries the correct leader proof, has a valid timestamp for its rank, includes a valid VDF output, and follows the transaction selection rules.

Starting at height `300`, fallback finalization invalidates missed ticket opportunities. If a ticket block is finalized by rank `1` or higher, nodes invalidate all tickets ranked from `0` through the finalizing rank for that height. They also invalidate any other currently eligible tickets owned by those same addresses. Future tickets from those addresses that are not yet eligible remain pending. Rank `0` ticket blocks continue to consume only the winning ticket.

Every normal block must include at least one plaintext burn. A blinded transaction envelope does not satisfy that rule, because the finalizer and validators cannot know whether the encrypted payload is a burn until reveal. A node that may finalize prepares a local plaintext anchor burn for the next block from the finalizer wallet. This anchor burn is not gossiped as normal wallet traffic.

This mandatory anchor burn is a liveness rule for the ticket pool, not a fairness rule for ticket distribution. It guarantees that normal block production keeps creating future tickets. Fairness against self-serving finalizers comes from blinded third-party burns.

Wallet-created transfers and burns are not gossiped as plaintext. Their blinded envelopes expose and lock UTXO inputs before reveal, so declared fees are backed by spendable coins. Mine actions are public mempool items, because they do not reveal burn or transfer intent and must be possible without owning coins. The local plaintext anchor burn for finalization is separate from the configured automatic blinded burn per block. When automatic burning is enabled, the configured burn amount enters the network as a blinded envelope like other wallet-created burns. When a blinded payload is revealed and executed, `35%` of its fee goes to the finalizer that originally committed the envelope, up to `35%` goes to the reveal-block finalizer, and `10%` goes to each included explicit signed reveal-list maker. Missing reveal-list shares, missing reveal-finalizer shares, and rounding dust are burned. The local plaintext anchor burn required for block liveness is part of the block reward like other plaintext block items.

## VDF Timing

The VDF is there to make block production sequential and time-based. It uses repeated squaring in an unknown-order RSA group: validators know the public modulus, but not its factorization. That unknown order is essential. If the factorization were known, a finalizer could skip the delay with normal modular exponentiation.

The devnet uses the public RSA-2048 challenge modulus. A production mainnet should use a purpose-specific trusted setup ceremony with destroyed factors, or a class-group VDF that avoids trusted setup.

The target block time is `5 minutes`. The protocol retargets VDF rounds from recent observed block times:

- It uses a `20` block observation window.
- It uses rank `0` ticket blocks for retargeting.
- It ignores fallback and recovery blocks for retargeting because their timestamps include intentional waiting.
- It has a `10%` deadband and a maximum `2%` retarget step per adjustment.
- Extremely fast or slow samples are clamped before they affect the next target.

Fallback finalizers use more VDF work: rank `0` uses the base rounds, rank `1` uses `2x`, rank `2` uses `3x`, and so on. This gives the primary finalizer the first chance while still allowing the network to move if the primary does not publish.

VDF rounds alone are not the fallback gate. Faster hardware could otherwise finish a lower-ranked VDF before a slower primary finalizer. iuna therefore also uses rank time slots:

- rank `0` blocks are valid as soon as their timestamp is greater than the parent timestamp;
- rank `1` blocks are valid from `parent timestamp + 2 * target block time`;
- rank `2` blocks are valid from `parent timestamp + 4 * target block time`;
- and so on.

If a fallback finalizer finishes the VDF early, it must wait until its slot opens before publishing. Rank `0` does not wait on a rank slot; that keeps the primary path useful as the clean VDF-speed signal for retargeting. If a rank `0` finalizer finishes late, the block timestamp should reflect that later completion/publication time so VDF retargeting can observe slow rounds. Other nodes reject fallback blocks whose timestamp is before their rank slot.

## Timestamp Checks

Rank slots depend on block timestamps, so timestamps are constrained by consensus:

- a block timestamp must be greater than its parent timestamp;
- it must exceed median-time-past;
- it must not be too far in the future relative to the validating node's network-adjusted clock;
- for fallback ticket blocks, it must be at or after the finalizer rank slot.

The future drift limit is `2 minutes`. A finalizer can lie within that small margin, but cannot skip an entire `10 minute` fallback rank slot by claiming a far-future timestamp. P2P treats too-early future/slot blocks as temporal errors rather than peer-banning evidence.

## Recovery Blocks

If selected ticket finalizers do not publish for long enough, recovery finalization becomes available. The recovery delay is `6` target block times.

A recovery block:

- does not use a burn-ticket leader proof;
- must include at least one burn from the recovery finalizer;
- uses normal base VDF rounds;
- is ignored by VDF retarget observations.

Recovery is a liveness mechanism. It is not meant to be the normal block path.

## Proof-of-Work Issuance

Mine actions are how new IUNA is minted after genesis.

A mine action is anchored to a recent chain tip and must meet the current PoW difficulty. It creates `1 IUNA` for the recipient when included in a block, and pays a fixed `1 IUNA` fee to the block finalizer.

Difficulty targets about one mine action per block:

- The retarget window is `10` blocks.
- The target is `10` mine actions per window.
- Difficulty can move by at most `2` bits per window.
- Difficulty is clamped between `10` and `32` bits in the devnet profile.
- Mine actions expire when their anchor is too old.

This keeps issuance separate from finalization. PoW miners compete to create mine actions; burn-ticket finalizers decide blocks.

A block may contain at most `2` mine actions for the same anchor. This leaves room for the difficulty retarget to move upward when PoW regularly fills both slots, while still bounding issuance from any single anchor.

## Fair Burn Inclusion

The central censorship risk is simple: what if a finalizer only includes its own burns and ignores everyone else's burns?

The blinded mempool protects transfers and third-party burns by making them indistinguishable before inclusion. It does not try to hide mine actions. Mine actions are public because they do not reveal burn intent, do not spend existing coins, and must remain available to participants with no IUNA balance.

Transfer and burn mempool traffic uses blinded transaction content. A wallet encrypts a transfer or burn payload and gossips a `BlindedTransaction` envelope. The plaintext payload is not exposed before reveal. The envelope exposes only:

- a commitment hash;
- the visible UTXO inputs that lock the fee and transaction spend;
- the declared fee;
- encrypted payload size;
- expiry height;
- nonce, ciphertext, and plaintext payload hash.

The visible inputs are signed for the blinded envelope itself and are not repeated inside the encrypted payload. The encrypted payload contains only the hidden transfer outputs or burn amount/change plus the transaction signature. When an envelope is included in a block, the visible inputs are locked immediately and cannot be spent by other pending transactions. The finalizer can rank the envelope by fee per visible envelope byte, but cannot see whether the encrypted payload is a transfer or a burn before committing it to a block. Mine actions are public and are not valid inside blinded envelopes.

Reveal is a later step. A `BlindedReveal` carries only the commitment and decryption key. Reveals are not included as loose block items. They are carried in signed reveal bundles.

Before height `1500`, nodes compute a reveal committee from the burn leader ranking. Slot `0` is assigned to the rank `0` block finalizer, so the selected finalizer can sign a reveal list for its own block. Before height `500`, the remaining slots are assigned to the two lowest-ranked eligible tickets. Starting at height `500`, the remaining slots are assigned to the next highest-ranked eligible tickets with owners that are not already in the committee, up to three unique owners total.

Starting at height `1500`, each block has one burn-selected finalizer and up to two additional independent reveal committee members. The finalizer remains reveal committee slot `0`, but does not need to include a separate reveal bundle signature: the finalizer already signs the block header, and that block signature commits to the selected reveal-bundle section. The additional reveal committee slots are selected from mature UTXO lineages, not from burn tickets, so block production remains proportional to burn while reveal witnessing is Sybil-resistant.

A UTXO lineage root is the newest mine action output in an output's ancestry. Transfer and change outputs carry that single root tag forward, even before the root is mature, so early transfers do not lose their ancestry. When outputs from multiple roots are merged and spent, descendants inherit the newest root among the spent inputs; ties are broken deterministically by root outpoint. If none of the spent inputs has a mine root, the new output has no reveal-committee lineage weight. A lineage root is eligible for reveal committee selection only when its mine action is at least `20` blocks old at the parent tip.

Nodes cache this root tag on every UTXO and maintain an incremental index from root to total unspent value. Normal block validation updates that cache only for the inputs spent and outputs created by the block. A broad ancestry search is only a rebuild or migration tool, not part of the consensus hot path.

Committee selection is root-first. For each eligible lineage root, validators sum the unspent value currently tagged with that root:

`root_value = sum(unspent_value_micro_iuna_tagged_with_root)`

The root's committee weight is:

`root_weight = floor(log2(1 + root_value))`

Splitting one large root across many addresses does not multiply committee influence, because the root is weighted once and can win at most one additional reveal slot. Merging roots deliberately collapses future descendant lineage to the newest root; lineage is a Sybil-resistance tag, not full coin-provenance accounting.

For a target height, validators derive a deterministic committee seed from the parent hash and height. Slot `0` is assigned to the block finalizer. Slots `1` and `2` are assigned without replacement by weighted deterministic draws over eligible lineage roots using `root_weight`. Lineage roots currently owned by the block finalizer are excluded from these additional draws, and after any other root wins a slot, that root is removed from the next slot draw. After a root is selected, validators choose one representative owner for that root from the unspent outputs tagged with it. The block finalizer's address and any address already selected for an earlier additional reveal slot are also skipped for additional slots. If fewer than two eligible non-finalizer lineages exist, the committee has the finalizer plus one or zero additional members. If no eligible non-finalizer lineage exists, reveal quorum falls back to `1-of-1` through the finalizer's implicit slot `0` attestation.

A committee member can sign one bundle for its slot, height, and parent hash. A bundle is at most `10,000` bytes and lists valid pending reveals ordered by visible fee rate. Starting at height `1500`, once a blinded envelope is active, committee members also gossip empty bundles when they know no valid reveal for the next height; the empty signature is an attestation that keeps the reveal layer explicit without forcing a reveal to exist.

Automatic nodes wait about `30 seconds` after seeing pending reveals for the next height before signing a reveal bundle or starting the reveal-bound VDF. Starting at height `1500`, active blinded envelopes also trigger this wait so empty attestations can be collected. This gives reveal gossip time to settle and avoids locking in an underfilled bundle from the first partial batch a node received.

A block has an envelope section and one compact reveal-bundle section. The envelope section contains the finalizer's plaintext anchor burn, public mine actions, and blinded transaction envelopes.

The compact reveal-bundle section stores:

- up to two explicit reveal committee bundle signatures for non-finalizer slots, in slot order;
- one deduplicated reveal list;
- a small bitmask per reveal saying which of the included committee bundles contained that reveal.

Validators reconstruct each signed committee bundle from this compact section before checking signatures, bundle size, slot assignment, lineage-based slot assignment, and fee ordering. The finalizer's block signature is treated as its committee attestation for slot `0`. Slot `0` does not have a separate reveal bundle payload or reveal-list-maker fee after height `1500`; it attests to the block's deduplicated reveal list as included by the finalizer. This keeps consensus bound to the independent reveal attestations without storing the same reveal payload multiple times when multiple committee members selected it.

A block may contain at most one bundle per slot. If a node sees two different signed bundles for the same height and slot before block assembly, it treats that slot as locally equivocated and does not use either bundle for that round.

Before height `1500`, reveal-list signatures are optional for chain compatibility. Starting at height `1500`, if there are no active blinded envelopes before a block, no reveal-list threshold is required. If active blinded envelopes exist, ticket blocks must carry enough reveal-list signatures for their finalizer rank:

- rank `0` needs all available reveal committee attestations (`3-of-3`, `2-of-2`, or `1-of-1`), where the finalizer's block signature counts as the slot `0` attestation;
- rank `1` needs two reveal committee attestations when possible (`2-of-3`, `2-of-2`, or `1-of-1`), where the finalizer's block signature counts as the slot `0` attestation;
- rank `2` and later ticket finalizers may publish without reveal committee signatures, but must include any valid signatures already bound into their VDF seed.

Recovery blocks do not require reveal-list signatures; their job is chain liveness after the ticket path has failed. A valid signed bundle may be empty. Honest committee policy is to sign an empty bundle only when the signer knows no valid reveal for that height, and to include every valid reveal it selects by the canonical fee ordering. The consensus rule checks committee membership, signature validity, lineage assignment, ordering, and threshold; it does not depend on a validator's local mempool contents.

The ticket-block VDF seed is bound to the reveal attestation hashes:

`seed = hash(parent hash || height || attestation_hash[0] || attestation_hash[1] || attestation_hash[2])`

Recovery blocks additionally bind the block timestamp into the VDF seed:

`seed = hash(parent hash || height || timestamp_ms || attestation_hash[0] || attestation_hash[1] || attestation_hash[2])`

Before height `1500`, each attestation hash is the signed reveal-bundle hash for that slot, or a fixed default hash when the slot has no included bundle. Starting at height `1500`, slot `0` uses a synthetic finalizer attestation hash derived from the parent, height, finalizer address, and the block's canonical deduplicated reveal list. Slot `0` therefore attests to every reveal executed by the block, independent of which explicit bundles also contained it. Slots `1` and `2` use the signed reveal-bundle hash or the fixed default hash when absent. This means the finalizer must choose the reveal-attestation set before doing the VDF work. A finalizer can still claim that a bundle arrived too late, but it cannot secretly swap or remove a timely bundle after computing the VDF without changing the seed.

When a valid bundled reveal executes, nodes decrypt the earlier payload, check the commitment and payload hash, and decode the transfer or burn. The decrypted transaction inputs must match the visible inputs locked by the envelope, and the transaction executes against that locked value. If the reveal bitmask says multiple committee bundles contained the same reveal, the reveal is still executed only once. If the decrypted transaction is a burn, it creates burn tickets at the reveal height, not the earlier envelope-commit height.

Fees are paid without inflating the reveal block reward. The decrypted transaction must pay the same fee declared by the blinded envelope. `35%` goes to the envelope committer. Up to `35%` goes to the reveal-block finalizer, scaled by included reveal attestations divided by the available committee slots for that height. Before height `1500`, signed reveal bundles define those attestations. Starting at height `1500`, the denominator is the total available committee size including implicit slot `0` (`3`, `2`, or `1`). The finalizer's implicit slot `0` attestation counts for the scaled reveal-finalizer share for every reveal in the block, so the reveal-block finalizer always receives at least one slot's share when it includes a valid reveal. Slot `0` does not earn the separate reveal-list-maker share. `10%` goes to each included explicit signed reveal-list maker for non-finalizer slots. Missing reveal-list shares, the missing reveal-finalizer share, and rounding dust are burned instead of redistributed.

Starting at height `750`, reveal fee attribution is per reveal mask. A signed reveal-list maker earns the `10%` share for a revealed payload only if that maker's signed bundle actually contained that reveal. The reveal-block finalizer's scaled share is also based on the number of attestations for that reveal, not merely the number of bundle signatures included somewhere in the block. Before height `1500`, those attestations are signed bundles. Starting at height `1500`, slot `0` is counted as attesting to every reveal in the block through the finalizer's block signature, while slots `1` and `2` count only when their signed bundle contained the reveal. Before height `750`, all included signed reveal-list makers are treated as participating in every revealed payload in that block.

Expiry is exclusive: a blinded envelope with expiry height `H` can be included only in blocks below height `H`, and revealed only while the current chain height is below `H`. The expiry height must be within `20` blocks of the node's current chain height when the envelope is accepted or selected. If an envelope expires unrevealed, its declared fee is burned and any remaining locked value returns as deterministic change to the owner of the first visible input. Expired local envelopes and reveals are dropped from local selection.

Starting at height `750`, blinded envelopes must lock at least one visible input. This rejects free, unauthenticated zero-input envelopes while preserving compatibility with historical devnet blocks before the activation height.

Starting at height `750`, every item that consumes block space must pay a fee, with one exception: a block may include one zero-fee plaintext burn from the block finalizer as its local anchor burn. Other plaintext transactions, additional finalizer burns, blinded envelopes, and revealed blinded payloads must carry a non-zero fee. This keeps historical devnet blocks valid while removing free blockspace spam after activation.

This does not make censorship impossible. A finalizer can still ignore all blinded traffic, or censor based on network metadata. But it removes the cheap strategy of inspecting plaintext mempool transactions and excluding third-party burns while including other fee-paying transactions.

## P2P Mempool Gossip

The P2P mempool gossips only:

- blinded transaction envelopes;
- public mine actions;
- blinded reveal keys;
- signed reveal bundles;
- block inventory and blocks.

It does not gossip plaintext transfers or burns. Wallet-created transfers and burns enter the network as blinded envelopes first, and are only decoded after a reveal. Mine actions are gossiped as public transactions. The one plaintext anchor burn required for every normal block is prepared locally by the finalizer and appears in the block itself.

Nodes only keep blinded reveal keys in their local mempool when the reveal references an active blinded envelope and decrypts successfully. Unknown, stale, or wrong-key reveals are rejected before they consume pending reveal capacity. Pending transaction, blinded-envelope, reveal, and orphan pools are bounded by both item count and serialized byte size.

## Block Selection

When a node builds a block, it selects transactions in this order:

1. Collect valid signed reveal bundles for the next height.
2. Reserve the local plaintext anchor burn as the first plaintext block item.
3. For recovery blocks, ensure at least one plaintext anchor burn is from the recovery finalizer.
4. Fill remaining envelope space with valid fee-paying public mine actions and blinded transaction envelopes ordered by fee rate. Public mine actions are limited to `2` actions per anchor.
5. Bind the VDF seed to the three reveal-attestation slot hashes, using default hashes for missing slots. Starting at height `1500`, slot `0` uses the synthetic finalizer attestation hash instead of a separate reveal-bundle hash.

Blocks are bounded by transaction count and serialized byte size. The devnet maximum block size is `100,000` bytes.

## Fork Choice

Nodes fully validate candidate blocks or snapshots before considering a reorg. A candidate chain must share the same genesis and cannot rewrite history deeper than the finality depth. In the devnet profile, forks whose common ancestor is below `local height - 6` are rejected.

Within that finality window, a taller valid candidate chain wins over the local chain. If the candidate and local chains have the same height but different tips, nodes compare the first divergent blocks by leader score: ticket blocks beat recovery blocks, lower finalizer rank beats higher rank, and the leader proof rank breaks remaining ties. Equal quality keeps the local chain.

## Genesis and Joining

Genesis is explicit. A normal node without a chain starts in setup mode and waits to join an existing chain from peers rather than silently creating a separate chain.

The genesis flow bootstraps the devnet with an initial burn ticket and a fixed `1 IUNA` initial reward for the genesis wallet. New nodes fetch and validate chain snapshots from peers, then continue with normal block validation.

## What This Design Is Trying to Achieve

iuna is trying to make these things true at the same time:

- Finalization should not require specialized mining hardware.
- New issuance should not require already owning a large stake.
- Burns should have real opportunity cost.
- Burn timing power should expire rather than accumulate into permanent control.
- Block timing should be hard to rush.
- Finalizers should have a consensus-level reason to include burn traffic they cannot inspect before committing.

The design is intentionally small and still evolving. The devnet exists to find out where these assumptions hold and where they break.
