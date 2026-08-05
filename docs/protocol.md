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

Every normal block must include at least one plaintext burn. A blinded transaction envelope does not satisfy that rule, because the finalizer and validators cannot know whether the encrypted payload is a burn until reveal. Block-producing nodes create this plaintext burn locally from the finalizer wallet during block construction; it is not part of the gossiped mempool.

This mandatory burn is a liveness rule for the ticket pool, not a fairness rule for ticket distribution. It guarantees that normal block production keeps creating future tickets. Fairness against self-serving finalizers comes from blinded third-party burns.

Plaintext transaction fees go to the block finalizer immediately. Blinded transaction fees are paid when the payload is revealed and executed. Half goes to the finalizer that originally committed the envelope. The other half is the executor share and depends on reveal-bundle participation: with `0`, `1`, `2`, or `3` included reveal bundles, the reveal-block finalizer receives `0/3`, `1/3`, `2/3`, or `3/3` of that executor share. Any missing executor share is burned.

## VDF Timing

The VDF is there to make block production sequential and time-based. It cannot be parallelized in the same way as normal hashing work.

The target block time is `5 minutes`. The protocol retargets VDF rounds from recent observed block times:

- It uses a `20` block observation window.
- It uses rank `0` ticket blocks for retargeting.
- It ignores fallback and recovery blocks for retargeting because their timestamps include intentional waiting.
- It has a `10%` deadband and a maximum `2%` retarget step per adjustment.
- Extremely fast or slow samples are clamped before they affect the next target.

Fallback finalizers use more VDF work: rank `0` uses the base rounds, rank `1` uses `2x`, rank `2` uses `3x`, and so on. This gives the primary finalizer the first chance while still allowing the network to move if the primary does not publish.

VDF rounds alone are not the fallback gate. Faster hardware could otherwise finish a lower-ranked VDF before a slower primary finalizer. iuna therefore also uses rank time slots:

- rank `0` blocks are valid as soon as their timestamp is greater than the parent timestamp;
- rank `1` blocks are valid from `parent timestamp + 1 * target block time`;
- rank `2` blocks are valid from `parent timestamp + 2 * target block time`;
- and so on.

If a fallback finalizer finishes the VDF early, it must wait until its slot opens before publishing. Rank `0` does not wait on a rank slot; that keeps the primary path useful as the clean VDF-speed signal for retargeting. If a rank `0` finalizer finishes late, the block timestamp should reflect that later completion/publication time so VDF retargeting can observe slow rounds. Other nodes reject fallback blocks whose timestamp is before their rank slot.

## Timestamp Checks

Rank slots depend on block timestamps, so timestamps are constrained by consensus:

- a block timestamp must be greater than its parent timestamp;
- it must exceed median-time-past;
- it must not be too far in the future relative to the validating node's network-adjusted clock;
- for fallback ticket blocks, it must be at or after the finalizer rank slot.

The future drift limit is `2 minutes`. A finalizer can lie within that small margin, but cannot skip an entire `5 minute` rank slot by claiming a far-future timestamp. P2P treats too-early future/slot blocks as temporal errors rather than peer-banning evidence.

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
- Difficulty is clamped between `1` and `32` bits in the devnet profile.
- Mine actions expire when their anchor is too old.

This keeps issuance separate from finalization. PoW miners compete to create mine actions; burn-ticket finalizers decide blocks.

## Fair Burn Inclusion

The central censorship risk is simple: what if a finalizer only includes its own burns and ignores everyone else's burns?

Normal mempool traffic uses blinded transaction content. A wallet encrypts a normal transaction payload and gossips a `BlindedTransaction` envelope. The plaintext payload is not exposed before reveal. The envelope exposes only:

- a commitment hash;
- the declared fee;
- encrypted payload size;
- expiry height;
- nonce, ciphertext, and plaintext payload hash.

The finalizer can rank the envelope by fee per visible envelope byte, but cannot see whether the encrypted payload is a transfer or a burn before committing it to a block.

Reveal is a later step. A `BlindedReveal` carries only the commitment and decryption key. Reveals are not included as loose block items. They are carried in signed reveal bundles.

For each next block height, nodes compute a reveal committee from the burn leader ranking. The last three ranked eligible tickets form the three reveal-bundle slots. A committee member can sign one bundle for its slot, height, and parent hash. A bundle is at most `10,000` bytes and lists valid pending reveals ordered by visible fee rate. Empty bundles are not gossiped.

A block has an envelope section and one compact reveal-bundle section. The envelope section contains the finalizer's plaintext burn, other plaintext block items, and blinded transaction envelopes.

The compact reveal-bundle section stores:

- up to three bundle signatures, one per committee slot, in slot order;
- one deduplicated reveal list;
- a small bitmask per reveal saying which of the included committee bundles contained that reveal.

Validators reconstruct each signed committee bundle from this compact section before checking signatures, bundle size, slot assignment, and fee ordering. This keeps consensus bound to the three independent signed reveal lists without storing the same reveal payload multiple times when several committee members selected it.

A block may contain at most one bundle per slot. If a node sees two different signed bundles for the same height and slot before block assembly, it treats that slot as locally equivocated and does not use either bundle for that round.

The block VDF seed is bound to the reveal bundle hashes:

`seed = hash(parent hash || height || bundle_hash[0] || bundle_hash[1] || bundle_hash[2])`

If a slot has no included bundle, it contributes a fixed default hash for that slot. This means the finalizer must choose the reveal-bundle set before doing the VDF work. A finalizer can still claim that a bundle arrived too late, but it cannot secretly swap or remove a timely bundle after computing the VDF without changing the seed.

When a valid bundled reveal executes, nodes decrypt the earlier payload, check the commitment and payload hash, decode the normal transaction, validate it against the current UTXO set, and execute it once. If the reveal bitmask says multiple committee bundles contained the same reveal, the reveal is still executed only once. If the decrypted transaction is a burn, it creates burn tickets at the reveal height, not the earlier envelope-commit height.

Fees are paid without inflating the reveal block reward. The decrypted transaction must pay the same fee declared by the blinded envelope. `floor(fee / 2)` goes to the envelope committer. The reveal-block finalizer can receive up to the remaining executor share, scaled by the number of included reveal bundles. Missing executor share is burned instead of redistributed.

Expiry is exclusive: a blinded envelope with expiry height `H` can be included only in blocks below height `H`, and revealed only while the current chain height is below `H`. The expiry height must be within `20` blocks of the node's current chain height when the envelope is accepted or selected. Expired envelopes and reveals are dropped from local selection.

This does not make censorship impossible. A finalizer can still ignore all blinded traffic, or censor based on network metadata. But it removes the cheap strategy of inspecting plaintext mempool transactions and excluding third-party burns while including other fee-paying transactions.

## P2P Mempool Gossip

The P2P mempool gossips only:

- blinded transaction envelopes;
- blinded reveal keys;
- signed reveal bundles;
- block inventory and blocks.

It does not gossip plaintext transfers, burns, or mine actions. Wallet-created transfers, burns, and mine actions enter the network as blinded envelopes first, and are only decoded after a reveal. The one plaintext burn required for every normal block is produced locally by the finalizer and appears in the block itself.

## Block Selection

When a node builds a block, it selects transactions in this order:

1. Collect valid signed reveal bundles for the next height.
2. Ensure the envelope has at least one plaintext burn from local block construction.
3. For recovery blocks, ensure at least one plaintext burn is from the recovery finalizer.
4. Fill remaining envelope space with valid blinded transaction envelopes ordered by fee rate.
5. Bind the VDF seed to the three reveal-bundle slot hashes, using default hashes for missing slots.

Blocks are bounded by transaction count and serialized byte size. The devnet maximum block size is `100,000` bytes.

## Genesis and Joining

Genesis is explicit. A normal node without a chain starts in setup mode and waits to join an existing chain from peers rather than silently creating a separate chain.

The genesis flow bootstraps the devnet with an initial burn ticket and an initial reward for the genesis wallet. New nodes fetch and validate chain snapshots from peers, then continue with normal block validation.

## What This Design Is Trying to Achieve

iuna is trying to make these things true at the same time:

- Finalization should not require specialized mining hardware.
- New issuance should not require already owning a large stake.
- Burns should have real opportunity cost.
- Burn timing power should expire rather than accumulate into permanent control.
- Block timing should be hard to rush.
- Finalizers should have a consensus-level reason to include burn traffic they cannot inspect before committing.

The design is intentionally small and still evolving. The devnet exists to find out where these assumptions hold and where they break.
