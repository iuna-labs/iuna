# iuna protocol in simple terms

iuna is an experimental devnet protocol that combines three mechanisms:

- **Burn lottery:** burning IUNA creates tickets for future block finalization.
- **VDF timing:** the selected finalizer must do sequential delay work before publishing a block.
- **Proof-of-work issuance:** new IUNA enters the chain through PoW mine actions.

The goal is to avoid relying on only one scarce resource. Proof-of-work chains tend to centralize around hardware and cheap energy. Proof-of-stake chains tend to centralize around existing wealth and staking pools. iuna tries a split design: burns choose who finalizes blocks, VDFs pace block production, and PoW keeps new issuance open to anyone who can find valid work.

This is still experimental. The rules below describe the current devnet protocol, not a proven mainnet design.

## Coins and Transactions

iuna uses a UTXO-style ledger. The main transaction types are:

1. **Transfer:** moves IUNA from one address to another and pays a sender-chosen fee.
2. **Burn:** destroys an amount of IUNA, pays a sender-chosen fee, and creates a future lottery ticket.
3. **Mine action:** proves SHA-256-style PoW against the current chain tip. A valid mine action mints a fixed `1 IUNA` reward to its recipient and pays a fixed `1 IUNA` fee to the block finalizer.
4. **Burn claim:** proves that recent finalizers saw a burn that has not been included yet.

Burn and transfer fees are chosen by the sender. Mine action reward and mine action fee are deterministic protocol values.

## Burns Become Tickets

A burn does not immediately select its own block. Instead:

1. A burn is included in a block.
2. It becomes a ticket after the maturity delay.
3. The ticket stays eligible for a short expiry window.
4. Its lottery weight is the burned amount.

On the current devnet profile, tickets mature after `3` blocks and remain eligible for `3` block heights.

The lottery draw for the next height is deterministic. Nodes rank all eligible burn tickets using the parent block hash, the parent VDF output, the target height, and the ticket amounts. More burned IUNA means more weight, but the winner is still drawn by the protocol.

## Finalizing Blocks

For each block height, eligible tickets are ranked:

- Rank `0` is the primary finalizer.
- Rank `1`, `2`, and later ranks are fallback finalizers.

The selected finalizer must prove ownership of the selected ticket and run the required VDF work. A block is valid only if the finalizer matches its ranked ticket, carries the correct leader proof, includes a valid VDF output, and follows the transaction selection rules.

Every normal block must include at least one burn transaction. The finalizer reward for the block is the sum of transaction fees in that block.

## VDF Timing

The VDF is there to make block production sequential and time-based. It cannot be parallelized in the same way as normal hashing work.

The target block time is `10 minutes`. The protocol retargets VDF rounds from recent observed block times:

- It uses a `20` block observation window.
- It ignores recovery blocks for retargeting.
- It adjusts fallback blocks by finalizer rank, so a rank `1` fallback block does not look twice as slow just because it had to wait for twice the VDF work.
- It has a `10%` deadband and a maximum `2%` retarget step per adjustment.
- Extremely fast or slow samples are clamped before they affect the next target.

Fallback finalizers use more VDF work: rank `0` uses the base rounds, rank `1` uses `2x`, rank `2` uses `3x`, and so on. This gives the primary finalizer the first chance while still allowing the network to move if the primary does not publish.

## Recovery Blocks

If selected ticket finalizers do not publish for long enough, recovery finalization becomes available. The current delay is `6` target block times.

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
- Difficulty is clamped between `1` and `32` bits on the current devnet.
- Mine actions expire when their anchor is too old.

This keeps issuance separate from finalization. PoW miners compete to create mine actions; burn-ticket finalizers decide blocks.

## Fair Burn Inclusion

The central censorship risk is simple: what if a finalizer only includes its own burns and ignores everyone else's burns?

iuna handles this with burn claims.

When recent finalizers see a valid burn in the mempool, they can sign a `BurnSeen` attestation. A burner can package the burn plus enough recent-finalizer attestations into a `BurnClaim`.

A burn claim is valid only when:

- it references a valid burn transaction;
- the burn is still unconfirmed;
- the burn can be applied to the current UTXO set;
- the attestations are from recent finalizers;
- the attestations match the finalizer's recent block height and hash;
- enough unique recent finalizers signed it.

The current quorum target is `3` recent finalizers, capped by however many unique recent finalizers exist. Attestations are taken from the last `10` blocks.

Once a valid burn claim is included, the claimed burn becomes consensus-required. Blocks after the claim must include that burn within the inclusion window. On the current devnet, that window is `3` blocks. A block that omits a due claimed burn is invalid.

This does not make censorship impossible. A fully partitioned network or a cartel that controls enough recent finalizers can still cause trouble. But it changes the normal case: if independent finalizers have seen a burn, later finalizers cannot simply ignore it without producing invalid blocks.

## Block Selection

When a node builds a block, it selects transactions in this order:

1. Include due claimed burns first.
2. Ensure the block has at least one burn.
3. For recovery blocks, ensure at least one burn is from the recovery finalizer.
4. Fill remaining space with valid transactions ordered by fee rate.

Blocks are bounded by transaction count and serialized byte size. The current devnet maximum block size is `100,000` bytes.

## Genesis and Joining

Genesis is explicit. A normal node without a chain starts in setup mode and waits to join an existing chain from peers rather than silently creating a separate chain.

The current genesis flow bootstraps the devnet with an initial burn ticket and an initial reward for the genesis wallet. New nodes fetch and validate chain snapshots from peers, then continue with normal block validation.

## What This Design Is Trying to Achieve

iuna is trying to make these things true at the same time:

- Finalization should not require specialized mining hardware.
- New issuance should not require already owning a large stake.
- Burns should have real opportunity cost.
- Block timing should be hard to rush.
- Finalizers should have a consensus-level reason to include burns they did not create.

The design is intentionally small and still evolving. The devnet exists to find out where these assumptions hold and where they break.
