# iuna protocol in simple terms

iuna is an experimental protocol built from two chains that every full node runs together:

- **Heartbeat Chain:** a SHA-256 Proof of Work chain with a 60-second target block time. It decides fork choice, carries the shared state, and its block hashes are the "heartbeats" that supply randomness.
- **Ledger Chain:** a linear UTXO transaction chain with a target block time of about 10 minutes. Its blocks are picked by a public lottery and carried inside Heartbeat blocks.
- **Proof of Expenditure:** anyone can buy temporary Ledger mining weight by spending IUNA on a mining ticket. The spent amount is not destroyed; it is paid out to Heartbeat miners over the ticket's lifetime.

The goal is that the same expenditure always buys the same mining weight, and that this weight is rented rather than owned.

Spending more IUNA deliberately gives more relative influence over the Ledger lottery. The difference from stake is that ticket weight is paid for again and again: the budget is locked when the ticket is included, is never refunded, and the ticket disappears after at most about a day. The same is true of the old burn idea, but expenditure is a transfer rather than destruction. That keeps the money supply stable without a reissue pool, recovery blocks, claim accumulators, separate state rent or special recovery rewards.

This is still experimental. The rules below describe candidate protocol, not a proven mainnet design. Version 3 starts from a clean genesis; earlier candidate coins do not carry over. The chain is intended to carry forward if the candidate is promoted, but that is not guaranteed.

## Candidate Parameters

- network name: `iuna-mainnet-candidate`;
- protocol version: `3`;
- Heartbeat target block time: `60 seconds`;
- difficulty adjustment: relative ASERT with a half-life of `60` Heartbeats (`1 hour`);
- median-time-past window: `11` blocks;
- maximum future timestamp drift: `10 minutes`;
- proof-of-work limit and genesis target: `0000003fffffffffffffffffffffffffffffffffffffffffffffffffffffffff`;
- genesis nonce: `44762545`;
- genesis hash: `00000015ff94624a51c05e7bf53e0b837baf105d9cf28d0af49955f1a8b24fdb`;
- genesis timestamp: `1794077841`;
- genesis miner key: `d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a`;
- smallest unit: `1 IUNA = 100_000_000` units;
- `MAX_MONEY`: `4_207_680_100_000_000` units (`42,076,801 IUNA`);
- genesis reward: `1 IUNA`;
- initial Ledger subsidy: `50 IUNA`;
- Ledger halving interval: `420,768` included Ledger blocks;
- Ledger target interval `D`: `10` Heartbeats;
- maximum Heartbeat block data: `16 KiB`;
- maximum Ledger sidecar: `512 KiB`;
- maximum tracked tickets `M`: `4,096`;
- ticket duration `K`: `1` to `1,440` Heartbeats;
- ticket confirmations before activation: `6` Heartbeats;
- minimum ticket budget: `1,000,000` units (`0.01 IUNA`);
- winner inclusion window: `3` Heartbeats.

The genesis miner key is the public RFC 8032 test key. Its secret is public too, so anyone can spend the genesis output. The first miner's node does exactly that to fund the first ticket. A promoted mainnet may need a controlled key, which would be a new genesis.

Unit tests use regtest-like parameters (a 12-bit proof-of-work limit) so they can mine many blocks quickly.

## Amounts

All amounts are unsigned 64-bit integers in the smallest unit. Every addition and subtraction is checked. A state transition that overflows or exceeds `MAX_MONEY` is invalid.

## Addresses and Spending

Every output owner is a 32-byte domain-separated commitment to an Ed25519 public key:

`address = SHA256(length("IUNA-ADDRESS-v1") || "IUNA-ADDRESS-v1" || length(public_key) || public_key)`

The output publishes only this address. A spending input supplies the 32-byte public key and an Ed25519 signature. Validation recomputes the address, requires it to equal the spent output owner, and then verifies the signature. The public key is therefore revealed only when that output is spent.

Wallet recovery entropy deterministically derives separate receive and change branches using `IUNA-WALLET-CHILD-v1`, a one-byte branch identifier and a big-endian child index. Receive addresses rotate after use and every transaction sends change to the next unused change address. Recovery scans both branches using address-use history and a gap limit of 20. Mining identity remains a separate stable key; rewards are paid to its address commitment.

## Architecture

- Every full node validates and stores both logical chains and one shared UTXO state.
- Every Heartbeat block hash is a heartbeat and provides randomness for the Ledger lottery.
- A Heartbeat block contains zero or one Ledger candidate hash. If it names one, the full valid Ledger sidecar must be available.
- The canonical Ledger Chain follows entirely from the Heartbeat Chain. The Ledger has no fork choice of its own.
- A Heartbeat reorg rolls back Ledger sidecars, tickets, payouts and UTXO changes atomically.
- There is no delay function, fallback consensus, reissue pool or recovery mechanism.

## Genesis and Chain Identity

- The genesis block is an ordinary mined Heartbeat block with no parent.
- The genesis reward of `1 IUNA` goes to the address commitment of the miner public key in the header.
- The genesis block is pinned in the code, like Bitcoin's genesis block. Nodes never mine genesis at startup. Any other root is a different network.
- In practice the genesis miner holds the first distribution role. That is an explicit launch assumption.

## Heartbeat Blocks and Proof of Work

A Heartbeat header contains at least:

- protocol and network version;
- height and parent hash;
- timestamp;
- ASERT target;
- nonce;
- miner public key;
- Merkle root of the Heartbeat ticket transactions;
- an optional Ledger candidate hash;
- the shared post-state root;
- a commitment to the synthetic payouts.

The block hash is `SHA256("IUNA-HEARTBEAT-v1" || canonical header)`, and a block is valid only if that hash is at most the header target.

The target is not chosen by the miner. Validators recompute it from the parent chain and require the header to match exactly. A block's own timestamp is checked separately and only affects the target of the next block. Consensus uses integer arithmetic only.

### Difficulty: Relative ASERT

Each block scales its parent's target by how long the parent took to find:

$$
\text{target}_{n+1} = \min\left(\text{MAX\_TARGET},\ \text{target}_n \cdot 2^{(t_n - t_{n-1} - 60)/\tau}\right),\qquad \tau = 60 \cdot 60\text{ s}
$$

In plain terms: if the parent came faster than 60 seconds, the next block gets harder; if it came slower, it gets easier. A full half-life of extra time halves the difficulty.

The exact integer rules are:

- the exponent is computed with `16` fractional bits and rounded to nearest (always rounding down would push the average Heartbeat above 60 seconds);
- the fractional power of two uses the same cubic integer polynomial as Bitcoin Cash's aserti3-2d;
- the result is clamped at the proof-of-work limit and is never zero;
- block 1 has no parent solvetime and keeps the genesis target.

The half-life is `60` Heartbeats (`1 hour`). A young network sees large hashrate swings: one extra machine can multiply it. With this half-life, after a tenfold hashrate jump the difficulty is within a factor two of the right value in under an hour, and within ten percent in about three hours. A half-life of `288` Heartbeats would take four and fifteen hours. One lucky or unlucky block still moves the target by only about one percent. The price is a little more noise: the difficulty wanders around its right value by about eight percent, compared to three percent with the longer half-life.

Why relative instead of absolute ASERT? Without the clamp the two are the same. But absolute ASERT measures against a schedule that starts at the genesis timestamp. A network that sat idle for days would then build up "schedule debt" and burst-mine a run of easy blocks to catch up. Relative ASERT only looks at the last solvetime, so there is no debt to repay.

Miners do not wait for the 60-second interval. They use `max(now, median-time-past + 1)` as their timestamp. The block pace comes only from proof of work and ASERT.

### Proof-of-Work Limit

Like Bitcoin's `powLimit` (difficulty 1), the genesis target is also the easiest target allowed: 26 leading zero bits. That is roughly one 60-second Heartbeat for a single CPU thread doing about 1 MH/s. With less hashrate, Heartbeats are simply slower. ASERT raises difficulty from there.

### Timestamp Checks

- a timestamp must be greater than the median of the last `11` block timestamps;
- it may be at most `10 minutes` ahead of the validating node's clock.

Abusing that future drift can move one target by at most `2^(600 / τ)`, about `12%`, and the next solvetime reverses it.

### Fork Choice

The chain with the most cumulative work wins:

$$
\text{chainwork} = \sum \left( \left\lfloor \frac{2^{256}-1}{\text{target}+1} \right\rfloor + 1 \right)
$$

Block work and chainwork are exact, unbounded integers. The target is widened before computing `target + 1`, so nothing can overflow or wrap around.

- A node switches only to **strictly more** chainwork.
- On equal chainwork it keeps its current valid tip.
- A block that forks off below the recent state window first gets only the checks that need no state: Proof of Work, its target, its timestamp limit and whether its body matches the header. The node validates that branch in full once its chainwork exceeds the active tip. This changes when a block is validated, never which chain wins.
- The next block with extra work settles a temporary equal-work split.

### Limits and Data Availability

- A Heartbeat block may hold at most `16 KiB` of canonical data.
- A Ledger sidecar may be at most `512 KiB`.
- A named sidecar must be fully available and valid; otherwise the Heartbeat block is invalid.
- `MAX_TRACKED_TICKETS = 4_096` caps all tickets that are confirming, active or in their inclusion grace period combined. A Heartbeat block whose post-state exceeds this cap is invalid.

## Mining Tickets

### The Ticket Transaction

A ticket contains exactly:

- UTXO inputs;
- an expenditure budget `B` in units;
- an active duration `K` in heartbeats, with `1 <= K <= 1,440` (at most about one day);
- the mining public key that receives Ledger rewards;
- a public 32-byte `lottery_seed`;
- an optional change output;
- protocol version and signature.

The ticket ID is `SHA256("IUNA-TICKET-v1" || canonical fully signed transaction data)`.

### Minimum Budget

A ticket's budget must cover its share of the expected issuance while it occupies a slot in the ticket cap. Let `R_next` be the subsidy of the next Ledger inclusion (computed on the state after any sidecar in the same Heartbeat block), `D = 10` and `M = 4_096`:

$$
B \ge \max\left(1\,000\,000,\ \left\lceil \frac{R_{next}\cdot(K+9)}{D\cdot M} \right\rceil\right)
$$

The absolute minimum is `0.01 IUNA`. If all `M` ticket slots are filled, together they cost at least the expected bootstrap issuance during the time they occupy those slots. This makes the hard cap a priced safety limit rather than a free first-come reservation.

### Inclusion Fee

The inclusion fee is never stated directly; it follows from the amounts. Let `I` be the checked sum of the ticket's input values in the post-sidecar snapshot, and `C` the change output value (exactly zero if there is no change output). A change output exists exactly when `C > 0`; a present zero output is invalid. Then:

`inclusion fee = I - C - B`

The ticket is valid only if this subtraction does not underflow and the fee is at least one unit. So `I = B + C + inclusion fee` always holds, and a ticket can neither create value nor silently destroy it.

### Paying Out the Budget

A ticket included in Heartbeat block `H` is tracked for exactly `L = K + 9` blocks, from `H` through `H + K + 8`. Its whole budget `B` is paid to the miners of those blocks, one slice per block.

The slices are as equal as integers allow. Write `B = q_e·L + r_e`. Lifecycle offsets `0` through `r_e - 1` each pay `q_e + 1`; the remaining offsets pay `q_e`. Offset `0` is the inclusion block `H`.

The locked balance is not stored as a separate counter. After `n` offsets have been processed (in the post-state of height `T`, `n = T - H + 1` with `1 <= n < L`), it is:

$$
\text{remaining}(n) = B - \left(n\,q_e + \min(n, r_e)\right)
$$

Before the payout at offset `i` the same formula holds with `n = i`, and the payout is deducted atomically. After offset `L - 1`, `remaining(L) = 0` and the ticket record is removed. The budget is fully locked on inclusion, cannot be spent twice, and is never refunded to the buyer.

### Active Lottery Slots

- A ticket in `H` waits for six full Heartbeat descendants before it becomes active.
- It is active in heartbeats `H + 6` through `H + K + 5`.
- In heartbeat `T` its slot index is `s = T - (H + 6)`, with `0 <= s < K`.

The same budget is also split exactly over the `K` slots as mining weight. Write `B = q_w·K + r_w`: slots `s < r_w` get weight `q_w + 1`, the rest get `q_w`. The slot weights of one ticket sum to exactly `B`.

So one locked budget buys two things at once: a fixed payout schedule to Heartbeat miners over `K + 9` blocks, and `K` weighted lottery tickets for the Ledger.

## The Public Ledger Lottery

### Drawing

When `Heartbeat_T` is the current tip:

- eligibility follows from the canonical ticket history;
- the randomness is the fresh hash `H_Heartbeat,T`;
- the Ledger parent anchor is the current Ledger parent after processing `Heartbeat_T`: that is `H_Heartbeat,T` itself if the block contained a sidecar, otherwise the existing anchor. Before the first sidecar it is the hard-coded `LEDGER_GENESIS_ANCHOR`.

The lottery for heartbeat `T` is computed only after `Heartbeat_T` is fully validated, so `Heartbeat_T` never commits to its own outcome and there is no hash cycle.

For every active ticket, every node computes:

`y = SHA256("IUNA-LEDGER-LOT-v1" || H_Heartbeat,T || Ledger parent anchor || ticket ID || s || lottery_seed)`

Because the seed is public, any node can find the winners and build a candidate. The ticket buyer does not need to be online, and cannot hold back a winning ticket.

### Weighted Threshold

Let `w` be the ticket's slot weight and `W` the sum of all active slot weights. A ticket wins if:

$$
y < \left\lfloor \frac{2^{256}\cdot w}{D\cdot W} \right\rfloor
$$

`y`, `w` and `W` are widened to unsigned 512-bit integers first, so the multiplication and division cannot wrap around.

- If `W = 0` there are no winners.
- On average about `1/D` winners appear per heartbeat.
- `D = 10` is an initial calibration value, not a guarantee of exactly ten minutes between included Ledger blocks. Rounding, simultaneous winners and inclusion delay still need to be covered by launch simulations.

### Withholding

Every active ticket pays the miner of heartbeat `T` unconditionally. The smallest possible slice per active ticket is `floor(B / (K + 9)) >= 690` units.

Still, publishing a valid Heartbeat block is not strictly the best move under protocol rewards alone. A miner who finds a valid header can look at the lottery outcome first and decide to throw the block away. That costs the whole Heartbeat payout of the block and risks another miner taking the height, but gives the miner another shot at a better lottery outcome while it keeps working on the same height. That can be rational when the miner's own expected Ledger subsidy, fees, censorship value or an outside bribe outweighs those costs.

The expenditure payout makes selective withholding more expensive; it does not remove it. The protocol does not claim cryptographically unbiased randomness or that publishing is always the dominant strategy. Launch simulations need to model this for different concentrations of Heartbeat hashpower and Ledger ticket weight. A stronger randomness construction would be a separate protocol change; this base protocol adds no commit-reveal, delay function or extra consensus path for it.

## Ledger Candidates and Inclusion

### Candidates

The owner of a publicly winning ticket may build a non-empty body and authorize its exact contents with the ticket's mining key. A candidate header contains:

- protocol version;
- lottery heartbeat hash and height;
- Ledger parent anchor;
- ticket ID and slot index;
- lottery output `y`;
- the transactions Merkle root;
- an Ed25519 signature by the winning ticket's mining key.

The signature digest is a domain-separated hash of the network ID, genesis hash and unsigned candidate header. The candidate hash is `H_C = SHA256("IUNA-LEDGER-CANDIDATE-v2" || canonical signed candidate header)`.

Ordinary transactions keep their own signatures. A valid candidate signature authorizes selection and ordering only; it cannot change those transactions. The subsidy and the winner's fee share always go to the address commitment of the same mining public key. Heartbeat miners can include a signed candidate unchanged, but cannot construct or edit a non-empty one for somebody else's ticket.

### A Linear Chain

- The Ledger parent anchor is the Heartbeat block that included the previous Ledger sidecar.
- Once included, the hash of the including Heartbeat block becomes that Ledger block's canonical ID and the parent anchor for the next candidate.

That way a Ledger ID automatically identifies the full Heartbeat context and post-state, without a second derived ID or a hash cycle.

### Inclusion Window and Mandatory Fallback

- A winner from heartbeat `T` may publish signed non-empty candidates for inclusion in `T + 1` or `T + 2`, as long as the Ledger parent anchor does not change.
- Nodes gossip complete signed candidates. A node keeps at most one candidate per winning ticket slot: the candidate with the highest valid transaction-fee total, then the lowest candidate hash.
- A Heartbeat miner may include an available signed candidate unchanged, or include no sidecar while no winner is at its deadline.
- Before processing the sidecar of Heartbeat block `U`, validators determine `E_U`: all winners from lottery heartbeat `T = U - 3` whose parent anchor is still the current Ledger parent.
- If `E_U` is not empty, a sidecar is mandatory and must use the winner in `E_U` with the lowest normalized score `y / w`. That deadline sidecar is canonical, empty and unsigned. A signed or non-empty sidecar and a younger winner may not take its place.
- Scores are compared without floats: `a` beats `b` if `y_a · w_b < y_b · w_a`, with the lowest ticket ID winning a tie. The values are widened to unsigned 512-bit integers and the products are exact.
- Only when `E_U` is empty may the miner choose among signed non-empty candidates for the other winners still inside their first two inclusion slots, or include no sidecar at all.

### Compact Block Relay

After finding a Heartbeat block with a sidecar, the miner relays the header, the lottery context and short transaction IDs. Peers rebuild the sidecar from their own mempool and only ask for missing transactions. A peer accepts the Heartbeat block only after the full sidecar is available and validated.

## Rewards and Fees

### Bootstrap Subsidy

- target supply: `42,076,801 IUNA`;
- genesis reward: `1 IUNA`;
- initial Ledger subsidy: `50 IUNA`;
- halving every `420,768` included Ledger blocks, expected to be about four years.

Let `N` be the number of Ledger blocks already included before the current one, and `minted` the total genesis plus Ledger issuance in that parent state. In units:

- epoch `e = floor(N / 420,768)`;
- remaining room `R = MAX_MONEY - minted`;
- scheduled amount `S = 5,000,000,000 >> e` if `e < 33`, otherwise `S = 0` (the shift is never performed with `e >= 33`, which avoids implementation-defined behavior);
- subsidy = `0` if `R = 0`; `min(S, R)` if `S > 0`; `R` if `S = 0` and `R > 0`.

`N` and `minted` are updated after inclusion. So exactly the first `420,768` Ledger blocks receive the initial subsidy, block index `420,768` starts the first halving, and at most one last reward fills the remaining rounding gap. `R_next` elsewhere in this document is this function applied to the state in which the next Ledger block would be included.

The full subsidy goes to the address commitment of the mining public key of the winning ticket.

### Transaction Fees

For each ordinary Ledger transaction, all input values are looked up in the pre-sidecar UTXO snapshot. Let `I` be the checked sum of inputs and `O` the checked sum of new outputs. Every output must be strictly positive and the transaction is valid only if `O <= I`. The fee is exactly `f = I - O`. There is no implicit burn: the whole difference is fee.

For a sidecar, `F_tx` is the checked sum of all individual fees. An overflow, a missing input, an input spent twice within the batch, a zero output or `O > I` makes the whole Heartbeat block invalid.

The fees are split in half:

- `F_Heartbeat = floor(F_tx / 2)` goes to the miner of the Heartbeat block that includes the sidecar;
- `F_winner = F_tx - F_Heartbeat` goes to the address commitment of the mining public key of the winning ticket, so an odd remainder goes to the winner.

On top of that, Heartbeat miners receive all ticket expenditure slices and ticket inclusion fees. There are no separate block reward streams or recovery fees.

### A Stable Money Supply

- Bootstrap subsidies grow the money supply up to at most the target supply.
- Ticket expenditure and all fees are transfers within the existing supply.
- After the bootstrap the supply stays stable; mining neither destroys coins nor mints replacements.

Every full post-state must satisfy:

`sum(spendable UTXO values) + sum(remaining of tracked tickets) = minted`

Here `minted` is only genesis plus Ledger subsidy issuance. A block that breaks this equality (computed checked, without overflow) is invalid.

## State Transition Order

For every non-genesis Heartbeat block:

1. Start from the final state of the Heartbeat parent.
2. Decide whether a sidecar is optional, mandatory or absent. Validate every Ledger transaction in a present sidecar against the same UTXO snapshot from step 1. A Ledger transaction may not spend an output of another transaction in the same sidecar. If two transactions use the same input, the whole Heartbeat block is invalid.
3. Apply the valid Ledger transactions as one batch, create the non-zero subsidy and winner-fee outputs for the ticket winner, and set `F_Heartbeat` aside for step 5.
4. Validate all Heartbeat ticket transactions against the resulting snapshot from step 3. They may spend outputs from the sidecar just included, but not outputs of another ticket transaction in the same block. If two ticket transactions use the same input, the whole block is invalid. Then apply the valid ticket transactions as one batch.
5. Pay the current Heartbeat miner:
   - the lifecycle slice of every tracked ticket for this height;
   - ticket inclusion fees;
   - `F_Heartbeat` from an included sidecar.

   Each lifecycle slice is deducted from that ticket's remaining balance in the same atomic step.
6. Remove only tickets whose height `H + K + 8` has been fully processed and whose `remaining(L) = 0`.
7. Outputs created in step 5 can only be spent in a later block.
8. Commit the full final state in the Heartbeat post-state root.

Transaction lists have a canonical order for serialization and Merkle roots, but that order does not change the snapshots above. There are no dependencies inside a sidecar or inside a ticket batch.

### Synthetic Payout IDs

Rewards and miner payouts use synthetic IDs that can be computed in advance. They may not depend on the current Heartbeat block hash, because that hash covers the post-state root.

For an included candidate the settlement ID is:

`S = SHA256("IUNA-LEDGER-SETTLEMENT-v1" || H_Heartbeat,parent || new height || H_C || ticket ID || Heartbeat miner public key || canonical amounts)`

The canonical amounts in `S` are the breakdown at settlement, before the ticket batch: the inclusion fees and expenditure in it are still zero. Fixed output indexes under `S` identify the subsidy and the winner's fee share. The combined payout to the Heartbeat miner uses a separate ID over the parent, height, miner public key, candidate hash (or zero) and the canonical payout breakdown. All of these inputs are known before the new post-state root is computed.

A synthetic output with amount zero is never created and cannot be referenced as an input. Its fixed index stays reserved, so other outputs do not shift. This also applies to a combined Heartbeat miner payout of zero. A settlement with no subsidy and no winner fees therefore creates no UTXOs under `S`.

## Confirmations and Finality

- Tickets become active after six full Heartbeat descendants.
- An included Ledger block is immediately provisionally canonical through its Heartbeat block.
- Wallets show Ledger payments as confirmed after six further Heartbeat blocks by default.
- Outputs from the sidecar and its settlement may be spent by the ticket batch in the same Heartbeat block. Outputs newly created within a batch may not be spent within that same batch; all other outputs become spendable from a later Heartbeat block.
- Six confirmations reduce risk; they are not absolute finality.

## Encoding and State Root

Canonical encoding is big-endian and length-prefixed. Merkle trees are domain-separated and duplicate the last node on odd levels. Outputs encode a 32-byte address commitment; ordinary and ticket inputs reveal the matching public key. Inputs use Ed25519 only and sign a domain-separated SHA-256 digest of the network ID, the genesis hash, the unsigned transaction, the value of every spent output in input order, and their input index. Signed Ledger candidates use a separate domain and sign the network ID, genesis hash and unsigned candidate header with the winning ticket's mining key. The network scope stops signatures from being replayed on a fork or relaunch. The input values stop a node from misreporting a balance so that the wallet pays the difference as a fee: the signature would then be invalid. Synthetic Ledger output index `0` is the subsidy and index `1` the winner fees; the combined Heartbeat payout uses index `0`. Zero outputs are not created.

The state root commits the height, minted supply, Ledger block count, UTXOs and tracked tickets. The tip hash, timestamps, chainwork, heartbeat history and Ledger parent anchor are derived chain indexes. That is why there is no hash cycle when the hash of the including Heartbeat block becomes the next Ledger parent anchor.

## Security Claims and Limits

- The same expenditure gives the same total mining weight under the same `B/K` rules.
- Faster hardware does not create extra Ledger tickets; every node evaluates one hash per active ticket per heartbeat.
- More expenditure deliberately gives more relative mining influence.
- The Heartbeat Chain uses SHA-256 PoW and is not ASIC-resistant. ASIC hashpower gives sequencing, censorship and reorg power over both layers.
- Fresh Heartbeat hashes are economically, not cryptographically, hard to bias. A miner can discard a found block after seeing the lottery outcome. Expenditure payouts make that more expensive but do not guarantee that publishing is rational; Ledger rewards, ticket-weight concentration, censorship value and outside bribes remain part of the PoW security model.
- Ledger winners select and sign non-empty transaction bodies. Heartbeat miners still choose which available signed candidate to include and may censor all of them until the mandatory empty deadline fallback. The fallback guarantees progress, not transaction neutrality or throughput.

## Still to Validate for a Mainnet Profile

Before a mainnet profile, at least these need to be reviewed again and frozen:

- ASERT half-life, starting target, maximum target, timestamp rules and median-time window;
- exact canonical binary encoding, endianness, domain strings, Merkle rules and synthetic output indexes;
- exact transaction signature policy and protocol versions;
- limits on transaction counts and signature checks alongside the byte limits;
- benchmarks for `4,096` tracked tickets, the public lottery scan and worst-case state root updates within ten seconds;
- calibration of `D`, sidecar propagation and real Ledger block intervals;
- economic simulations of minimum expenditure, cap squatting, expenditure and hashpower concentration, fee income and selective heartbeat withholding, including miners that also control a large share of Ledger ticket weight;
- a P2P specification for compact relay, initial sync, snapshots, pruning and historical sidecar availability;
- property tests for quotient and remainder splitting, the ticket lifecycle, mandatory fallbacks, reorgs, equal-work forks, the supply cap and cross-layer UTXO conflicts.

## What This Design Is Trying to Achieve

iuna is trying to make these things true at the same time:

- The Heartbeat Chain decides PoW fork choice, timing, ticket state, Ledger inclusion and the shared state.
- One locked budget buys both a fixed payout schedule for Heartbeat miners and `K` weighted, public Ledger lottery tickets.
- Mining weight should be rented, not owned: after `K + 9` blocks the whole budget has gone to Heartbeat miners and the ticket record is gone.
- Faster Ledger hardware should not buy extra lottery chances.
- The money supply should stay stable after the bootstrap, because expenditure moves coins instead of destroying them.
- There should be one reward system: no principal refund, reissue pool, recovery path or parallel reward bookkeeping.

The design is intentionally small and still evolving. The candidate exists to find out whether these assumptions hold in practice and where they break.
