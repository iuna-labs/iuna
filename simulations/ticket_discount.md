# Miner ticket discount

Ticket expenditure is paid to Heartbeat miners. A miner with hash share `h` therefore gets back, on average, a share `h` of every ticket budget, its own included. Its marginal cost of Ledger weight is `1 - h` instead of `1`. This simulation asks whether that discount concentrates Ledger weight, and whether it makes two miner strategies pay: block withholding and selective inclusion.

Script: [`ticket_discount_sim.py`](ticket_discount_sim.py). It uses only the Python standard library and fixed seeds, so every run prints the numbers below.

```sh
python3 simulations/ticket_discount_sim.py          # full run, under a minute
python3 simulations/ticket_discount_sim.py --quick  # a few seconds
```

## Summary

1. When all hashpower buys tickets, each miner's Ledger weight equals its hash share and buyers without hashpower get no weight at all. Profit stays proportional to hash share, so the discount does not compound, but the Ledger lottery copies the hash distribution instead of adding independent block producers.
2. When part of the hashpower stays out of the ticket market and few other buyers take part, the miners that do buy get more weight than their hash share. A pool with 30% of the hash and one outside competitor holds 59% of the weight and earns 1.4 times its hash share. With ten outside buyers this is gone.
3. Block withholding loses money in the healthy market, but pays once ticket weight is concentrated and the ticket market is small relative to the Ledger prize. In the thin market of point 2 it raises the pool's profit by 17%.
4. Selective inclusion costs nothing while fees are zero. It takes 2 to 6% of the other miners' profit and turns 3 to 10% of Ledger blocks into empty deadline blocks. Fees equal to the subsidy make it cost the includer 16%.

Points 2 and 3 are launch risks: they need a small ticket market, which is most likely early on.

## Model

Amounts are in IUNA per Heartbeat. `V` is the expected Ledger prize per Heartbeat: the inclusion rate `λ = 1 - e^(-1/D) ≈ 0.095` times the subsidy plus the winner's half of the fees. `X` is the total ticket spend per Heartbeat, which in steady state is also what Heartbeat miners receive as expenditure slices.

**Part A** solves the steady-state ticket market as an asymmetric Tullock contest. Buyer `i` chooses a spend `x_i` and wins a share `x_i / X` of `V`:

```
profit_i = (x_i / X) · V - x_i + h_i · (X + λ · fees / 2)
```

Its marginal cost is `c_i = 1 - h_i`. With `m` active buyers the equilibrium is `X = V (m - 1) / Σc` and `x_i = X - c_i X² / V`, and a buyer is active only while `c_i < Σc / (m - 1)`. Hash shares are fixed, and outside buyers have `h = 0`. An optional capital cost adds the same amount to every `c_i` for locking a budget during `K + 9` Heartbeats.

**Part B** replays the Ledger rules per Heartbeat with the equilibrium weights from part A: the threshold `y < w / (D · W)`, the score `y / w`, the inclusion window `T + 1` and `T + 2`, the mandatory empty deadline sidecar at `T + 3` for the lowest score, and anchor invalidation when a sidecar is included. An honest includer takes the lowest score among the eligible winners. The strategies tested for the largest miner are:

- **block withholding:** discard a found block when its own lottery has no winning ticket of the miner, and let the height be found again;
- **selective inclusion:** as includer, include only the miner's own candidate and leave other winners to the deadline.

Profit is counted per committed Heartbeat, as if ASERT had absorbed the discarded blocks.

## Results

### Healthy market: weight follows hash share

All hashpower buys tickets, no fees. `w*` is the largest miner's weight in the same market without the rebate.

| Hash distribution | Largest `h` | Its weight | `w*`, 100 outsiders | Outsider weight | Profit / `h` |
|---|---|---|---|---|---|
| pools 30/25/15/10/10/5/5 | 0.30 | 0.300 | 0.009 | 0 | 1.00 |
| flat, 10 × 10% | 0.10 | 0.100 | 0.009 | 0 | 1.00 |
| whale 45 + 11 × 5% | 0.45 | 0.450 | 0.009 | 0 | 1.00 |

The result does not depend on the number of outside buyers (1 to 1,000 tested). The miners together get back all expenditure, so they bid until `X = V`, the point where an outside buyer only breaks even. Fees change nothing. A capital cost of 2% leaves 1.8% of the weight to outsiders, 10% leaves them 8.6%.

### Thin market: weight above hash share

Only the largest `k` pools of the pools distribution buy tickets. The rest of the hash collects payouts but buys nothing.

| Buyers | Outsiders | Top pool `h` | Its weight | Profit / `h` |
|---|---|---|---|---|
| top 1 | 1 | 0.30 | 0.588 | 1.40 |
| top 1 | 3 | 0.30 | 0.432 | 1.08 |
| top 1 | 10 | 0.30 | 0.346 | 1.01 |
| top 2 | 1 | 0.30 | 0.429 | 1.08 |
| top 3 | 1 | 0.30 | 0.364 | 1.02 |

### Strategies

Profit change of the largest miner against honest play, 200,000 Heartbeats × 4 seeds. "Empty" is the share of Ledger blocks that were deadline fallbacks.

| Market | Strategy | Ledger blocks / Heartbeat | Empty | Profit change | Others' profit |
|---|---|---|---|---|---|
| pools, all buy, no fees | honest | 0.096 | 0% | — | 3.34 |
| | selective inclusion | 0.094 | 5.9% | −0.1% | 3.23 |
| | block withholding | 0.108 | 0% | −52.6% | 4.69 |
| pools, top pool only, 1 outsider | honest | 0.098 | 0% | — | 2.81 |
| | selective inclusion | 0.096 | 3.4% | −0.3% | 2.74 |
| | block withholding | 0.120 | 0% | **+17.3%** | 3.56 |
| pools, all buy, fees = subsidy | selective inclusion | 0.094 | 5.9% | −15.6% | 6.92 |
| | block withholding | 0.108 | 0% | −63.2% | 9.70 |
| whale, all buy, no fees | selective inclusion | 0.093 | 10.2% | −0.1% | 2.48 |
| | block withholding | 0.128 | 0% | −14.3% | 4.55 |

Every discarded block is a new draw of the lottery, so withholding also raises the number of Ledger blocks per Heartbeat, by up to a third in these runs. Issuance then runs ahead of its schedule in time, though not per Ledger block.

### When withholding pays

Profit change from withholding for one miner with hash share `h` and ticket weight `w`, against honest play with the same `h` and `w`. Each cell is the lowest tested weight, in steps of 0.1, at which withholding pays; the full grids are in the script output.

| `X / V` | `h = 0.1` | `h = 0.2` | `h = 0.3` | `h = 0.4` |
|---|---|---|---|---|
| 1.0 | above 0.7 | above 0.7 | 0.7 | 0.6 |
| 0.8 | 0.7 | 0.7 | 0.6 | 0.5 |
| 0.6 | 0.6 | 0.5 | 0.5 | 0.4 |
| 0.4 | 0.4 | 0.4 | 0.3 | 0.3 |

The cost of a discarded block is its Heartbeat payout, which is about `X`. The gain is another chance at the Ledger prize `V`. A small ticket market relative to the prize therefore makes withholding cheaper. In the healthy market `w = h` and `X = V`, and no tested miner with `w = h` up to 0.4 gains. The thin market above has `X / V = 0.59` and `w = 0.59` at `h = 0.3`, which this grid puts at about +18%, in line with the +17.3% of the replay.

## Limitations

- The market is static. Hash shares are fixed, and the other buyers do not react when one miner withholds or includes selectively, so the thresholds can move once they do.
- Each player's tickets are one aggregate ticket. Simultaneous winners within one player are counted once.
- There is no ASERT, network latency or stale block in the replay. Discarded blocks only cost time.
- Fees and capital cost are inputs, not outcomes, and the model has no risk aversion.
- The withholding policy is the simple one: discard every block without a winning ticket of the miner. Mixed or conditional policies were not tested.

Treat the percentages and thresholds as indications of where the risks are, not as calibrated values.
