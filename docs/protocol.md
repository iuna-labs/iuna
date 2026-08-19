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

Fallback finalization invalidates missed ticket opportunities. If a ticket block is finalized by rank `1` or higher, nodes invalidate all tickets ranked from `0` through the finalizing rank for that height. They also invalidate any other currently eligible tickets owned by those same addresses. Future tickets from those addresses that are not yet eligible remain pending. Rank `0` ticket blocks continue to consume only the winning ticket.

Every normal block must include at least one burn. This keeps the future ticket pool alive even during quiet periods. A node that may finalize prepares a local anchor burn for the next block from the finalizer wallet, and that anchor burn appears directly in the block.

The anchor burn is not a fairness mechanism. By itself, it would mostly help the current finalizer keep creating future tickets. Fairness against self-serving finalizers comes from the burn inclusion committee described below.

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

If the ticket path does not produce a valid block for long enough, recovery finalization becomes available. This can happen because selected ticket finalizers do not publish, or because they cannot satisfy the rules needed for a valid ticket block. The recovery delay is `6` target block times.

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
- Difficulty has a minimum of `10` bits in the devnet profile.
- Mine actions expire when their anchor is too old.

This keeps issuance separate from finalization. PoW miners compete to create mine actions; burn-ticket finalizers decide blocks.

A block may contain at most `2` mine actions for the same anchor. This leaves room for the difficulty retarget to move upward when PoW regularly fills both slots, while still bounding issuance from any single anchor.

## Fair Burn Inclusion

The central censorship risk is simple: what if a finalizer only includes its own burns and ignores everyone else's burns?

Burn fairness is enforced by a burn inclusion committee. The committee gives independently observed burns a path into the next block, even when the block finalizer would rather ignore them.

The idea is:

1. Burns are ordinary mempool transactions.
2. Committee members look at the valid burns they have seen.
3. Each committee member signs a burn bundle for the next block height.
4. The finalizer must include enough committee attestations for its rank.
5. Every burn in the required burn list must appear in the block.

So a censoring finalizer cannot simply leave out third-party burns that the committee witnessed. To keep censoring, it must either keep those burns away from committee members, control enough committee influence, or disrupt the normal ticket path until weaker liveness rules take over.

Each block has one burn-selected finalizer and up to two additional independent burn committee members. The finalizer is committee slot `0`; the finalizer's block signature counts as its slot `0` burn-list attestation. The additional committee slots are selected from mature UTXO lineages, not from burn tickets, so committee capture requires a different resource from block production.

Why not choose the whole committee from burn tickets? Because then a large burner could buy both block production and the inclusion watchdog. Instead, the extra committee members are selected from UTXO lineages that originate in PoW mine actions. Burn weight chooses who can finalize blocks; mature mined coin lineages help choose who witnesses burn inclusion.

A UTXO lineage is a lightweight ancestry tag:

1. A mine action output starts a lineage root.
2. Transfer and change outputs carry that root forward.
3. If a transaction spends outputs from multiple roots, descendants inherit the newest root.
4. Deterministic tie-breaks handle roots with the same age.
5. Outputs with no mine root have no burn-committee lineage weight.

A lineage root is eligible for burn committee selection only when its mine action is at least `20` blocks old at the parent tip.

Node implementation note: nodes cache this root tag on every UTXO and maintain an index from root to total unspent value, so normal validation does not need to search full ancestry.

Committee selection is root-first. For each eligible lineage root, validators sum the unspent value currently tagged with that root:

`root_value = sum(unspent_value_micro_iuna_tagged_with_root)`

The root's committee weight is:

`root_weight = floor(log2(1 + root_value))`

Splitting one large root across many addresses does not multiply committee influence, because the root is weighted once and can win at most one additional burn committee slot. Merging roots deliberately collapses future descendant lineage to the newest root; lineage is a Sybil-resistance tag, not full coin-provenance accounting.

The lineage weight is logarithmic. A larger root has more chance to be selected, but doubling value does not double influence forever. This keeps committee selection from becoming a simple rich-get-richer vote while still giving larger, older mined lineages some weight.

For a target height, validators derive a deterministic committee seed from the parent hash and height. Slot `0` is assigned to the block finalizer. Slots `1` and `2` are assigned without replacement by weighted deterministic draws over eligible lineage roots using `root_weight`.

After a lineage root wins, validators deterministically choose one representative owner from the unspent outputs tagged with that root. The additional slots are meant to be independent from the finalizer, so the finalizer's address and any address already selected for an earlier additional burn committee slot are skipped when choosing representatives. If no eligible non-finalizer representative remains for a winning root, that root is skipped and the draw continues to the next eligible root.

The protocol can detect addresses and lineage roots, not hidden common control, so a finalizer using unrelated addresses is still a social and economic risk rather than something this rule can perfectly identify.

If fewer than two eligible non-finalizer lineages exist, the committee has the finalizer plus one or zero additional members. If no eligible non-finalizer lineage exists, burn inclusion quorum falls back to `1-of-1` through the finalizer's implicit slot `0` attestation.

A committee member can sign one burn bundle for its slot, height, and parent hash. A bundle is at most `10,000` bytes and lists valid fee-paying pending burns ordered by absolute fee, with signature as the deterministic tie-breaker. Honest committee policy is to include every valid burn it selects by that canonical ordering, or to sign an empty bundle only when the signer knows no valid burn for that height. Empty bundles are an honest-policy signal, not something validators can prove from their own mempools. Consensus checks committee membership, signature validity, lineage assignment, ordering, and threshold.

Automatic nodes wait about `30 seconds` after seeing pending burns for the next height before signing a burn bundle or starting the burn-list-bound VDF. This gives burn gossip time to settle and avoids locking in an underfilled bundle from the first partial batch a node received.

A block contains transfers, burns, mine actions, and one compact burn-bundle section. The finalizer's local anchor burn is still reserved as the first block item.

The compact burn-bundle section stores:

- up to two explicit burn committee bundle signatures for non-finalizer slots, in slot order;
- one deduplicated required burn list;
- a small bitmask per burn saying which of the included committee bundles contained that burn.

The required burn list is the union of fee-paying burns contained in the attestations the block uses. If one committee member signs an empty bundle and another signs a bundle with burns, the required list still includes the burns from the non-empty bundle.

Required burns must fit in the block. Nodes reject burn bundles or attestation sets whose deduplicated required burn list cannot fit within the block size and transaction count limits.

Validators reconstruct each signed committee bundle from this compact section before checking signatures, bundle size, slot assignment, lineage-based slot assignment, and fee ordering. The finalizer's block signature is treated as its committee attestation for slot `0`. Slot `0` does not have a separate burn bundle signature; it attests to the block's deduplicated required burn list as included by the finalizer. This keeps consensus bound to the independent burn attestations without storing the same burn payload multiple times when multiple committee members selected it.

Block validity is not allowed to depend on a validator's local mempool. Validators decide the required burn-list threshold from deterministic chain and block data only. Pending burns can affect local relay, bundle-signing, and block-building policy, but they cannot make the same block valid on one node and invalid on another.

A block may contain at most one bundle per slot. If a block includes one valid bundle for a slot, validators check that included bundle and do not need to know whether another bundle for the same slot existed elsewhere. If a block builder sees two different signed bundles for the same height and slot before block assembly, it ignores that slot's bundles for the round as local safety policy. The current protocol does not have a separate slashing rule for this.

Ticket blocks must carry enough burn-list attestations for their finalizer rank. Rank `0` has the strictest rule because it is the preferred path. Fallback ranks relax the threshold so the chain can keep moving if the primary finalizer or some committee members are unavailable. If the primary cannot gather the required attestations in time, a lower-ranked finalizer eventually gets a looser threshold.

That is a deliberate liveness tradeoff. A lower-ranked finalizer can use fewer attestations, so its required burn list may omit burns that appeared only in committee bundles it did not use. This is weaker for fairness than the rank `0` path, but stronger for liveness when the strict path is stuck.

The available committee size is the finalizer plus the selected non-finalizer committee members for that height:

- rank `0` needs all available burn committee attestations (`3-of-3`, `2-of-2`, or `1-of-1`), where the finalizer's block signature counts as the slot `0` attestation;
- rank `1` needs one fewer than all available attestations, with at least one attestation required (`2-of-3`, `1-of-2`, or `1-of-1`);
- rank `2` and later ticket finalizers need one valid burn-list attestation from the available committee (`1-of-3`, `1-of-2`, or `1-of-1`), where the finalizer's block signature still counts if no non-finalizer bundle is included.

Recovery blocks do not require burn-list signatures. They are the last liveness escape hatch after the ticket path has failed, so committee failure must not be able to stop the chain forever. Recovery is weaker for fairness and is not meant to be the normal block path.

The ticket-block VDF seed is bound to the burn-list attestation hashes:

`seed = hash(parent hash || height || attestation_hash[0] || attestation_hash[1] || attestation_hash[2])`

Recovery blocks additionally bind the block timestamp into the VDF seed:

`seed = hash(parent hash || height || timestamp_ms || attestation_hash[0] || attestation_hash[1] || attestation_hash[2])`

The burn-list attestation hashes are part of the VDF seed. This forces the finalizer to choose the included burn-attestation set before doing the delay work. After the VDF is computed, changing that attestation set changes the seed and invalidates the work.

Slot `0` uses a synthetic finalizer attestation hash derived from the parent, height, finalizer address, and the block's canonical deduplicated required burn list. Slots `1` and `2` use the signed burn-bundle hash or the fixed default hash when absent.

For recovery blocks, missing burn-list attestations use the fixed default hashes. A recovery block may include available burn bundles, but it does not need burn bundles in order to be valid.

Every burn in the deduplicated required burn list must appear in the block. A required burn is executed only once even if multiple committee bundles contained it. The block may also include additional valid burns not present in the required list, but those additional burns do not count toward the committee attestation threshold.

Every transaction that consumes normal block space must pay a fee. Transfers and burns with zero fee are invalid; the finalizer's local anchor burn is an ordinary fee-paying burn. Mine actions pay the deterministic mine action fee. Genesis burns are the only zero-fee burn transactions, and they are valid only inside the genesis block.

This does not make censorship impossible. A finalizer can still censor burns that no committee member has seen, and a well-funded attacker can try to control enough UTXO lineage weight to dominate the burn committee. But the attack is no longer just "win the block and leave out everyone else." The attacker also has to beat the independent inclusion layer, or force the chain onto weaker liveness paths.

## P2P Mempool Gossip

The P2P mempool gossips:

- transfers;
- burns;
- mine actions;
- signed burn bundles;
- block inventory and blocks.

Anchor burns are prepared locally by the finalizer and are not normal wallet traffic.

Nodes only keep transactions in their local mempool when they are valid, fee-paying, and unexpired. Pending transaction, burn-bundle, and orphan pools are bounded by both item count and serialized byte size.

## Block Selection

When a node builds a block, the flow is:

1. Collect valid signed burn bundles for the next height.
2. Reserve the local anchor burn as the first block item.
3. For recovery blocks, ensure at least one anchor burn is from the recovery finalizer.
4. Include every burn required by the selected burn-bundle attestations.
5. Fill remaining block space with valid fee-paying transfers, additional burns, and mine actions ordered by fee rate. Mine actions are limited to `2` actions per anchor.
6. Bind the VDF seed to the three burn-attestation slot hashes, using default hashes for missing slots. Slot `0` uses the synthetic finalizer attestation hash instead of a separate burn-bundle signature.

Blocks are bounded by transaction count and serialized byte size. The devnet maximum block size is `100,000` bytes.

## Fork Choice

Nodes fully validate candidate blocks or snapshots before considering a reorg. A candidate chain must share the same genesis and cannot rewrite history deeper than the finality depth. In the devnet profile, forks whose common ancestor is below `local height - 6` are rejected.

Burn inclusion is part of block validity. If a ticket block carries burn-list attestations but omits a burn required by those attestations, nodes reject the block before fork choice. The fork choice rule only compares chains made of valid blocks.

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
- Finalizers should have a consensus-level reason to include independently witnessed burn traffic.

The design is intentionally small and still evolving. The devnet exists to find out where these assumptions hold and where they break.
