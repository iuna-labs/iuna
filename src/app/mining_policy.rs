//! Local automatic-mining policy. None of these choices are consensus rules.

use crate::consensus::{
    Amount, ConsensusError, ConsensusState, LEDGER_TARGET_INTERVAL_HEARTBEATS, PublicKey,
    minimum_ticket_budget, subsidy,
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

fn expected_net_value(reward: Amount, other_weight: Amount, budget: Amount, duration: u64) -> f64 {
    let quotient = budget / duration;
    let remainder = budget % duration;
    let expected_wins = [(quotient + 1, remainder), (quotient, duration - remainder)]
        .into_iter()
        .filter(|(weight, slots)| *weight != 0 && *slots != 0)
        .map(|(weight, slots)| {
            (slots as f64 * weight as f64)
                / (LEDGER_TARGET_INTERVAL_HEARTBEATS as f64 * (other_weight as f64 + weight as f64))
        })
        .sum::<f64>();
    reward as f64 * expected_wins - budget as f64
}

/// Ticket budget that maximizes subsidy-only net EV against the current weight of other miners.
///
/// The continuous optimum seeds a small exact search that applies the protocol's integer
/// quotient/remainder slot split. The inclusion fee is constant, so it cannot move the optimum.
/// The consensus-recommended budget remains the lower bound for any automatic ticket.
pub fn ev_maximizing_ticket_budget(
    state: &ConsensusState,
    mining_public_key: Option<PublicKey>,
    duration: u64,
) -> Result<Amount> {
    let minimum = recommended_ticket_budget(state, duration)?;
    let reward = subsidy(state.ledger_blocks, state.minted)?;
    let other_weight = other_active_ticket_weight(state, mining_public_key)?;
    if reward == 0 || other_weight == 0 {
        return Ok(minimum);
    }

    // With evenly spread ticket weight, EV(B) = A*B/(C+B)-B where
    // A = reward*duration/D and C = duration*other_weight.
    let continuous = duration as f64
        * ((reward as f64 * other_weight as f64 / LEDGER_TARGET_INTERVAL_HEARTBEATS as f64).sqrt()
            - other_weight as f64);
    let center = if continuous.is_finite() && continuous > minimum as f64 {
        continuous.round() as u64
    } else {
        minimum
    };
    let radius = duration
        .checked_mul(2)
        .and_then(|value| value.checked_add(2))
        .ok_or(ConsensusError::Arithmetic)?;
    let start = minimum.max(center.saturating_sub(radius));
    let end = center
        .checked_add(radius)
        .ok_or(ConsensusError::Arithmetic)?;

    let mut best_budget = minimum;
    let mut best_value = expected_net_value(reward, other_weight, minimum, duration);
    for budget in start..=end {
        let value = expected_net_value(reward, other_weight, budget, duration);
        if value > best_value || (value == best_value && budget < best_budget) {
            best_budget = budget;
            best_value = value;
        }
    }
    Ok(best_budget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::{TrackedTicket, pinned_genesis};

    #[test]
    fn no_competition_keeps_the_automatic_budget_at_the_consensus_minimum() {
        let (_, state) = pinned_genesis().unwrap();
        assert_eq!(
            ev_maximizing_ticket_budget(&state, Some([7; 32]), 10).unwrap(),
            recommended_ticket_budget(&state, 10).unwrap()
        );
    }

    #[test]
    fn competing_weight_raises_the_ev_maximizing_budget_but_own_weight_does_not() {
        let (_, mut state) = pinned_genesis().unwrap();
        state.height = 6;
        state.tickets.insert(
            [1; 32],
            TrackedTicket {
                id: [1; 32],
                included_height: 0,
                budget: 10_000_000,
                duration: 10,
                mining_public_key: [8; 32],
                lottery_seed: [9; 32],
            },
        );
        let minimum = recommended_ticket_budget(&state, 10).unwrap();
        let competing = ev_maximizing_ticket_budget(&state, Some([7; 32]), 10).unwrap();
        assert_eq!(competing, 213_606_788);
        assert!(competing > minimum);
        assert_eq!(
            ev_maximizing_ticket_budget(&state, Some([8; 32]), 10).unwrap(),
            minimum
        );
    }
}
