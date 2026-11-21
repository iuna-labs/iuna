//! Local automatic-mining policy. None of these choices are consensus rules.

use std::collections::BTreeSet;

use crate::consensus::{
    Amount, ConsensusError, ConsensusState, LEDGER_TARGET_INTERVAL_HEARTBEATS, PublicKey,
    TICKET_CONFIRMATIONS, minimum_ticket_budget, subsidy,
};

type Result<T> = std::result::Result<T, ConsensusError>;

/// A budget that remains valid whether or not the ticket's Heartbeat also includes a Ledger block.
pub fn recommended_ticket_budget(state: &ConsensusState, duration: u64) -> Result<Amount> {
    let current_reward = subsidy(state.ledger_blocks, state.minted)?;
    let current = minimum_ticket_budget(duration, current_reward)?;
    let after_ledger_blocks = state
        .ledger_blocks
        .checked_add(1)
        .ok_or(ConsensusError::Arithmetic)?;
    let after_minted = state
        .minted
        .checked_add(current_reward)
        .ok_or(ConsensusError::Arithmetic)?;
    let reward_after_same_heartbeat = subsidy(after_ledger_blocks, after_minted)?;
    let after = minimum_ticket_budget(duration, reward_after_same_heartbeat)?;
    Ok(current.max(after))
}

/// Active per-Heartbeat weight not owned by `mining_public_key` at the current tip.
pub fn other_active_ticket_weight(
    state: &ConsensusState,
    mining_public_key: Option<PublicKey>,
) -> Result<Amount> {
    state
        .tickets
        .values()
        .filter(|ticket| Some(ticket.mining_public_key) != mining_public_key)
        .filter_map(|ticket| ticket.weight_at(state.height))
        .try_fold(0_u64, |total, weight| {
            total.checked_add(weight).ok_or(ConsensusError::Arithmetic)
        })
}

/// Distinct mining keys other than `mining_public_key` that will hold ticket weight while a ticket
/// included in the next block is active, counting tickets still in the mempool.
pub fn competing_miners(
    state: &ConsensusState,
    pending_keys: impl IntoIterator<Item = PublicKey>,
    mining_public_key: Option<PublicKey>,
    duration: u64,
) -> Result<u64> {
    let first = state
        .height
        .checked_add(1 + TICKET_CONFIRMATIONS)
        .ok_or(ConsensusError::Arithmetic)?;
    let end = first
        .checked_add(duration)
        .ok_or(ConsensusError::Arithmetic)?;
    let mut keys = BTreeSet::new();
    for ticket in state.tickets.values() {
        let active_from = ticket
            .included_height
            .checked_add(TICKET_CONFIRMATIONS)
            .ok_or(ConsensusError::Arithmetic)?;
        let active_until = active_from
            .checked_add(ticket.duration)
            .ok_or(ConsensusError::Arithmetic)?;
        if active_from < end && active_until > first {
            keys.insert(ticket.mining_public_key);
        }
    }
    keys.extend(pending_keys);
    if let Some(own) = mining_public_key {
        keys.remove(&own);
    }
    Ok(keys.len() as u64)
}

/// Ticket budget for automatic Ledger mining: each miner's share of the symmetric equilibrium.
///
/// With `N` miners, including this one, competing for an expected prize `V = reward / D` per
/// Heartbeat, the equilibrium weight per miner is `V (N - 1) / N^2` per Heartbeat. The
/// consensus-recommended budget remains the lower bound.
///
/// Why not a best response to the competing weight: nodes that decide at about the same time,
/// each against weight it can already see, overreact together. Above three miners that feedback
/// is unstable, and in a simulation of nodes joining one by one, total weight swung between 0.1
/// and 160 times the prize while nodes got back as little as 2% of their spend. Counting
/// miners instead of weight keeps total weight near the prize from the first few miners on.
///
/// Why no ticket is skipped when its EV is negative: once total weight exceeds the prize, a
/// newcomer always sees a negative EV, while a miner whose own ticket is still active sees less
/// competition. In the same simulation, the first miners kept buying and every later node stayed
/// out for good. Without the skip, a full ticket set rotates over all miners at a small loss.
///
/// Counting keys has a cost: a miner that spreads tickets over many keys lowers everyone else's
/// budget, though every key still pays at least the consensus minimum.
pub fn automatic_ticket_budget(
    state: &ConsensusState,
    pending_keys: impl IntoIterator<Item = PublicKey>,
    mining_public_key: Option<PublicKey>,
    duration: u64,
) -> Result<Amount> {
    let minimum = recommended_ticket_budget(state, duration)?;
    let reward = subsidy(state.ledger_blocks, state.minted)?;
    let others = competing_miners(state, pending_keys, mining_public_key, duration)?;
    let miners = u128::from(others) + 1;
    let share = u128::from(reward) * u128::from(duration) * u128::from(others)
        / (u128::from(LEDGER_TARGET_INTERVAL_HEARTBEATS) * miners * miners);
    Ok(minimum.max(Amount::try_from(share).map_err(|_| ConsensusError::Arithmetic)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::{TrackedTicket, pinned_genesis};

    fn ticket(seed: u8, included_height: u64, budget: Amount) -> TrackedTicket {
        TrackedTicket {
            id: [seed; 32],
            included_height,
            budget,
            duration: 10,
            mining_public_key: [seed; 32],
            lottery_seed: [seed; 32],
        }
    }

    fn state_with(tickets: &[TrackedTicket]) -> ConsensusState {
        let (_, mut state) = pinned_genesis().unwrap();
        state.height = 6;
        state.tickets = tickets.iter().map(|t| (t.id, t.clone())).collect();
        state
    }

    #[test]
    fn no_competition_keeps_the_automatic_budget_at_the_consensus_minimum() {
        let (_, state) = pinned_genesis().unwrap();
        assert_eq!(
            automatic_ticket_budget(&state, [], Some([7; 32]), 10).unwrap(),
            recommended_ticket_budget(&state, 10).unwrap()
        );
    }

    #[test]
    fn one_competitor_gives_each_miner_a_quarter_of_the_prize_whatever_its_weight() {
        let reward = subsidy(0, 0).unwrap();
        for budget in [MIN_BUDGET, 10_000_000_000] {
            let state = state_with(&[ticket(8, 0, budget)]);
            assert_eq!(state.ledger_blocks, 0);
            assert_eq!(
                automatic_ticket_budget(&state, [], Some([7; 32]), 10).unwrap(),
                reward / 4
            );
        }
    }

    #[test]
    fn own_tickets_do_not_count_as_competition() {
        let state = state_with(&[ticket(8, 0, MIN_BUDGET)]);
        assert_eq!(
            automatic_ticket_budget(&state, [[8; 32]], Some([8; 32]), 10).unwrap(),
            recommended_ticket_budget(&state, 10).unwrap()
        );
    }

    #[test]
    fn competitors_count_once_by_key_including_pending_and_not_yet_active_tickets() {
        // Window of a ticket included at 7: active from 13 up to 23.
        let state = state_with(&[
            ticket(1, 0, MIN_BUDGET),  // active 6..16: overlaps
            ticket(2, 5, MIN_BUDGET),  // active 11..21: overlaps
            ticket(3, 17, MIN_BUDGET), // active 23..33: starts after the window
        ]);
        let mut expired = ticket(4, 0, MIN_BUDGET);
        expired.duration = 7; // active 6..13: ends before the window
        let mut state = state;
        state.tickets.insert(expired.id, expired);
        assert_eq!(competing_miners(&state, [], None, 10).unwrap(), 2);
        assert_eq!(
            competing_miners(&state, [[1; 32], [5; 32], [6; 32]], None, 10).unwrap(),
            4
        );
    }

    #[test]
    fn many_competitors_fall_back_to_the_consensus_minimum() {
        // The equilibrium share is about reward / N, so the minimum takes over near the miner
        // count a full ticket set holds.
        let tickets: Vec<_> = (1..=2_200u32)
            .map(|index| {
                let mut ticket = ticket(0, 0, MIN_BUDGET);
                ticket.id[..4].copy_from_slice(&index.to_le_bytes());
                ticket.mining_public_key = ticket.id;
                ticket
            })
            .collect();
        let state = state_with(&tickets);
        let minimum = recommended_ticket_budget(&state, 10).unwrap();
        assert_eq!(
            automatic_ticket_budget(&state, [], Some([0; 32]), 10).unwrap(),
            minimum
        );
        let few = state_with(&tickets[..9]);
        let ten_miners = subsidy(0, 0).unwrap() * 9 / 100;
        assert_eq!(
            automatic_ticket_budget(&few, [], Some([0; 32]), 10).unwrap(),
            ten_miners.max(minimum)
        );
    }

    const MIN_BUDGET: Amount = crate::consensus::MIN_TICKET_BUDGET;
}
