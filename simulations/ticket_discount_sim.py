"""Economic simulation of the miner ticket discount in iuna protocol v3.

Ticket expenditure is paid to Heartbeat miners, so a miner with hash share h gets back h of every
ticket budget, its own included. Its marginal cost of Ledger weight is (1 - h) instead of 1.

Part A solves the steady-state ticket market as an asymmetric Tullock contest with that rebate.
Part B replays the Ledger lottery and inclusion rules per Heartbeat, with honest miners and with
two strategies a miner can combine with the discount: block withholding and selective inclusion.

Units: IUNA per Heartbeat. Hash shares are exogenous (hardware is a sunk choice here).

Findings and limitations: simulations/ticket_discount.md. Standard library only; seeded, so every
run prints the same numbers. A full run takes under a minute, `--quick` a few seconds:

    python3 simulations/ticket_discount_sim.py [--quick]
"""

import math
import random
import sys

S = 50.0  # Ledger subsidy per included Ledger block
D = 10  # Ledger target interval in Heartbeats
LAMBDA = 1 - math.exp(-1 / D)  # included Ledger blocks per Heartbeat, honest inclusion


# ---------------------------------------------------------------- Part A: equilibrium


def tullock(costs, value):
    """Equilibrium spend per player for prize `value` and marginal costs `costs`.

    Player i maximises (x_i / X) * value - c_i * x_i. With m active players the total is
    X = value * (m - 1) / sum(c_active), and x_i = X - c_i * X^2 / value, active iff positive.
    """
    order = sorted(range(len(costs)), key=lambda i: costs[i])
    active = []
    for i in order:
        trial = active + [i]
        if len(trial) < 2:
            active = trial
            continue
        total_cost = sum(costs[j] for j in trial)
        if costs[i] < total_cost / (len(trial) - 1):
            active = trial
        else:
            break
    spend = [0.0] * len(costs)
    if len(active) < 2:
        return spend  # a lone buyer pays the minimum budget; not modelled
    X = value * (len(active) - 1) / sum(costs[j] for j in active)
    for i in active:
        spend[i] = max(0.0, X - costs[i] * X * X / value)
    return spend


def equilibrium(hash_shares, outsiders, fees, rebate=True, capital_cost=0.0, buyers=None):
    """Ticket market with miners (hash_shares) and `outsiders` buyers without hash power.

    `buyers` marks which miners buy tickets at all; a passive miner only collects payouts.

    `fees` are the transaction fees per Ledger block; the winner gets half, the includer half.
    `capital_cost` is the extra cost per unit of budget for locking it for K + 9 Heartbeats.
    """
    value = LAMBDA * (S + fees / 2)
    hs = list(hash_shares) + [0.0] * outsiders
    active = list(buyers or [True] * len(hash_shares)) + [True] * outsiders
    costs = [1 + capital_cost - (h if rebate else 0.0) if a else math.inf for h, a in zip(hs, active)]
    spend = tullock(costs, value)
    X = sum(spend)
    rows = []
    for h, x in zip(hs, spend):
        weight = x / X if X else 0.0
        heartbeat_income = h * ((X if rebate else 0.0) + LAMBDA * fees / 2)
        ledger_income = weight * value
        profit = heartbeat_income + ledger_income - x * (1 + capital_cost)
        rows.append({"h": h, "x": x, "weight": weight, "profit": profit})
    return X, value, rows


def hhi(shares):
    return sum(s * s for s in shares)


def report_equilibria():
    distributions = {
        "pools (30/25/15/10/10/5/5)": [0.30, 0.25, 0.15, 0.10, 0.10, 0.05, 0.05],
        "flat (10 x 10%)": [0.10] * 10,
        "whale (45 + 11 x 5%)": [0.45] + [0.05] * 11,
    }
    print("PART A: steady-state ticket market (fees = 0, bootstrap)")
    print("weight = share of Ledger ticket weight; profit share = share of all mining profit")
    for name, hs in distributions.items():
        print(f"\n== {name}")
        print(f"{'outsiders':>9} | {'X/V':>5} | {'top h':>5} {'top w':>6} {'top w*':>6} | "
              f"{'miners w':>8} | {'HHI hash':>8} {'HHI w':>6} | {'top profit':>10}")
        for outsiders in (1, 3, 10, 100, 1000):
            X, V, rows = equilibrium(hs, outsiders, fees=0.0)
            _, _, base = equilibrium(hs, outsiders, fees=0.0, rebate=False)
            miners = rows[: len(hs)]
            profits = sum(max(r["profit"], 0) for r in rows)
            top = miners[0]
            print(
                f"{outsiders:>9} | {X / V:5.2f} | {top['h']:5.2f} {top['weight']:6.3f} "
                f"{base[0]['weight']:6.3f} | {sum(r['weight'] for r in miners):8.3f} | "
                f"{hhi(hs):8.3f} {hhi([r['weight'] for r in rows]):6.3f} | "
                f"{top['profit'] / profits if profits else 0:10.3f}"
            )
    print("\n(top w* = the top miner's weight without the rebate, for comparison)")

    print("\nThin market: only the top k pools buy tickets, the rest of the hash is passive")
    hs = distributions["pools (30/25/15/10/10/5/5)"]
    print(f"{'buyers':>12} {'outsiders':>9} | {'top h':>5} {'top w':>6} | {'buying w':>8} {'buying h':>8} | top profit / top h")
    for k in (1, 2, 3):
        for outsiders in (1, 3, 10, 100):
            buyers = [i < k for i in range(len(hs))]
            X, V, rows = equilibrium(hs, outsiders, fees=0.0, buyers=buyers)
            profits = sum(max(r["profit"], 0) for r in rows)
            print(f"{'top ' + str(k):>12} {outsiders:>9} | {hs[0]:5.2f} {rows[0]['weight']:6.3f} | "
                  f"{sum(r['weight'] for r in rows[:k]):8.3f} {sum(hs[:k]):8.2f} | "
                  f"{rows[0]['profit'] / profits / hs[0]:.2f}")

    print("\nSensitivity: pools, 100 outsiders, fees and capital cost")
    hs = distributions["pools (30/25/15/10/10/5/5)"]
    print(f"{'fees/S':>6} {'capital':>7} | {'top w':>6} {'miners w':>8} | top profit / top h")
    for fees in (0.0, 0.5 * S, 2 * S):
        for capital in (0.0, 0.02, 0.10):
            X, V, rows = equilibrium(hs, 100, fees=fees, capital_cost=capital)
            profits = sum(max(r["profit"], 0) for r in rows)
            miners = rows[: len(hs)]
            print(f"{fees / S:6.1f} {capital:7.2f} | {miners[0]['weight']:6.3f} "
                  f"{sum(r['weight'] for r in miners):8.3f} | "
                  f"{miners[0]['profit'] / profits / hs[0]:.2f}")


# ---------------------------------------------------------------- Part B: Monte Carlo


class Player:
    def __init__(self, name, h, weight, withhold=False, selective=False):
        self.name, self.h, self.weight = name, h, weight
        self.withhold, self.selective = withhold, selective
        self.income = 0.0
        self.ledger_wins = 0
        self.blocks = 0


def simulate(players, X, fees, heartbeats, seed):
    """Replays Heartbeats. Each player's tickets are one aggregate ticket of its weight.

    Pending winners carry (player, lottery height, score, anchor). A block at height U first
    decides its sidecar: the lowest score among winners from U - 3 is mandatory; otherwise an
    honest includer takes the lowest score among winners from U - 1 and U - 2, while a selective
    one includes only its own. Then the block's own lottery runs on the resulting anchor. A
    withholding finder discards its block when it does not win that lottery, and the height is
    found again.
    """
    rng = random.Random(seed)
    hashers = [p for p in players if p.h > 0]
    cumulative, total = [], 0.0
    for p in hashers:
        total += p.h
        cumulative.append(total)

    def finder():
        r = rng.random() * total
        for p, c in zip(hashers, cumulative):
            if r < c:
                return p
        return hashers[-1]

    pending = []  # (player, lottery height, score, anchor)
    anchor = 0
    ledger_blocks = discarded = deadline = 0
    for height in range(heartbeats):
        while True:
            miner = finder()
            live = [w for w in pending if w[3] == anchor and height - 3 <= w[1] <= height - 1]
            due = [w for w in live if w[1] == height - 3]
            forced = bool(due)
            if due:
                sidecar = min(due, key=lambda w: w[2])
            elif miner.selective:
                own = [w for w in live if w[0] is miner]
                sidecar = min(own, key=lambda w: w[2]) if own else None
            else:
                sidecar = min(live, key=lambda w: w[2]) if live else None
            next_anchor = height + 1 if sidecar else anchor
            winners = []
            for p in players:
                if p.weight <= 0:
                    continue
                y = rng.random()
                if y < p.weight / D:
                    winners.append((p, height, y / p.weight, next_anchor))
            if miner.withhold and not any(w[0] is miner for w in winners):
                discarded += 1
                continue
            break
        miner.blocks += 1
        miner.income += X
        if sidecar:
            winner = sidecar[0]
            winner.income += S + fees / 2
            winner.ledger_wins += 1
            miner.income += fees / 2
            ledger_blocks += 1
            deadline += forced
            anchor = next_anchor
        pending = [w for w in pending if w[3] == anchor and w[1] >= height - 2] + winners
    return ledger_blocks / heartbeats, discarded / heartbeats, deadline / max(ledger_blocks, 1)


def market(hs, outsiders, fees, buyers=None):
    X, V, rows = equilibrium(hs, outsiders, fees, buyers=buyers)
    miners = rows[: len(hs)]
    outsider_weight = 1 - sum(r["weight"] for r in miners)
    spend = {f"m{i}": r["x"] for i, r in enumerate(miners)}
    spend["outsiders"] = X - sum(spend.values())
    return X, miners, outsider_weight, spend


def run_strategies(hs, outsiders, fees, heartbeats, seeds, buyers=None):
    X, miners, outsider_weight, spend = market(hs, outsiders, fees, buyers)
    passive = "" if buyers is None else f", only {sum(buyers)} pool(s) buy"
    print(f"\nPART B: Monte Carlo, hash {hs}{passive}, {outsiders} outsiders, fees/S = {fees / S:.1f}, "
          f"{heartbeats:,} Heartbeats x {seeds} seeds")
    print(f"equilibrium weights: top miner {miners[0]['weight']:.3f}, "
          f"outsiders {outsider_weight:.3f}, X = {X:.3f} IUNA/Heartbeat")
    print(f"{'top miner strategy':<26} | {'Ledger/HB':>9} {'empty':>6} {'discard/HB':>10} | "
          f"{'top blocks':>10} {'top Ledger':>10} | {'top profit':>10} {'vs honest':>9} | "
          f"{'others profit':>13}")
    honest_profit = None
    for label, withhold, selective in (
        ("honest", False, False),
        ("selective inclusion", False, True),
        ("block withholding", True, False),
        ("both", True, True),
    ):
        totals = {"rate": 0.0, "empty": 0.0, "discard": 0.0, "blocks": 0.0, "wins": 0.0, "profit": 0.0, "others": 0.0}
        for seed in range(seeds):
            players = [Player(f"m{i}", r["h"], r["weight"]) for i, r in enumerate(miners)]
            players.append(Player("outsiders", 0.0, outsider_weight))
            players[0].withhold, players[0].selective = withhold, selective
            rate, discard, empty = simulate(players, X, fees, heartbeats, seed)
            totals["empty"] += empty / seeds
            top = players[0]
            totals["rate"] += rate / seeds
            totals["discard"] += discard / seeds
            totals["blocks"] += top.blocks / heartbeats / seeds
            wins = sum(p.ledger_wins for p in players)
            totals["wins"] += top.ledger_wins / wins / seeds
            totals["profit"] += (top.income / heartbeats - spend["m0"]) / seeds
            totals["others"] += sum(p.income / heartbeats - spend[p.name] for p in players[1:]) / seeds
        if honest_profit is None:
            honest_profit = totals["profit"]
        change = totals["profit"] / honest_profit - 1
        print(f"{label:<26} | {totals['rate']:9.4f} {totals['empty']:6.1%} {totals['discard']:10.4f} | "
              f"{totals['blocks']:10.3f} {totals['wins']:10.3f} | {totals['profit']:10.3f} "
              f"{change:+9.1%} | {totals['others']:13.3f}")


def withholding_threshold(heartbeats, seeds, market_size=1.0):
    """For which (h, w) does block withholding pay, against honest play at the same (h, w)?

    `market_size` is X / V: total ticket spend relative to the Ledger prize per Heartbeat.
    """
    print(f"\nPART B2: block withholding gain for one miner (rest honest, X/V = {market_size}, fees = 0)")
    X = market_size * LAMBDA * S
    weights = (0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7)
    print("rows: hash share h, columns: ticket weight w; profit change vs honest")
    print(f"{'h':>5} | " + " ".join(f"{w:>7}" for w in weights))
    for h in (0.1, 0.2, 0.3, 0.4):
        cells = []
        for w in weights:
            result = []
            for withhold in (False, True):
                profit = 0.0
                for seed in range(seeds):
                    players = [
                        Player("m0", h, w, withhold=withhold),
                        Player("others", 1 - h, 1 - w),
                    ]
                    simulate(players, X, 0.0, heartbeats, seed)
                    # the miner's spend on weight w is w * X in a market of size X
                    profit += (players[0].income / heartbeats - w * X) / seeds
                result.append(profit)
            honest, withheld = result
            cells.append(f"{withheld / honest - 1:+7.1%}" if honest > 0 else f"{'n/a':>7}")
        print(f"{h:5.2f} | " + " ".join(cells))


if __name__ == "__main__":
    quick = "--quick" in sys.argv
    report_equilibria()
    pools = [0.30, 0.25, 0.15, 0.10, 0.10, 0.05, 0.05]
    heartbeats, seeds = (40_000, 2) if quick else (200_000, 4)
    run_strategies(pools, 100, 0.0, heartbeats, seeds)
    run_strategies(pools, 1, 0.0, heartbeats, seeds, buyers=[True] + [False] * 6)
    run_strategies(pools, 100, S, heartbeats, seeds)
    run_strategies([0.45] + [0.05] * 11, 100, 0.0, heartbeats, seeds)
    for market_size in (1.0, 0.8, 0.6, 0.4):
        withholding_threshold(heartbeats // 4, seeds, market_size)
