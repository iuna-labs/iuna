use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::{
    Amount, Block, FinalizerMode, LaunchProfile, MAX_VDF_ROUNDS, Transaction, VDF_TARGET_BLOCK_MS,
    hex_hash,
};

pub(super) const MISSED_FALLBACK_TICKET_INVALIDATION_HEIGHT: u64 = 300;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BurnTicket {
    pub(super) id: String,
    pub(super) owner: String,
    pub(super) amount: Amount,
    pub(super) eligible_from_height: u64,
    pub(super) eligible_until_height: u64,
}

pub(super) fn ranked_tickets_for_height(
    parent: &Block,
    target_height: u64,
    tickets: &[BurnTicket],
) -> Vec<BurnTicket> {
    let mut remaining = tickets
        .iter()
        .filter(|ticket| ticket_is_eligible_for_height(ticket, target_height))
        .cloned()
        .collect::<Vec<_>>();
    let mut ranked = Vec::with_capacity(remaining.len());

    for rank in 0.. {
        let Some(selected_index) =
            select_weighted_ticket_index(parent, target_height, rank, &remaining)
        else {
            break;
        };
        ranked.push(remaining.remove(selected_index));
    }

    ranked
}

fn select_weighted_ticket_index(
    parent: &Block,
    target_height: u64,
    rank: u32,
    tickets: &[BurnTicket],
) -> Option<usize> {
    let total_weight = tickets.iter().try_fold(0_u128, |total, ticket| {
        total.checked_add(u128::from(ticket.amount))
    })?;
    if total_weight == 0 {
        return None;
    }

    let draw = weighted_ticket_draw(parent, target_height, rank, total_weight);
    let mut cumulative = 0_u128;
    for (index, ticket) in tickets.iter().enumerate() {
        cumulative = cumulative.checked_add(u128::from(ticket.amount))?;
        if draw < cumulative {
            return Some(index);
        }
    }
    None
}

fn weighted_ticket_draw(parent: &Block, target_height: u64, rank: u32, total_weight: u128) -> u128 {
    let seed = if rank == 0 {
        format!(
            "iuna-ticket-draw:{}:{}:{}",
            target_height, parent.hash, parent.vdf_output
        )
    } else {
        format!(
            "iuna-ticket-draw-rank:{}:{}:{}:{}",
            target_height, rank, parent.hash, parent.vdf_output
        )
    };
    let digest = Sha256::digest(seed.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    u128::from_be_bytes(bytes) % total_weight
}

pub(super) fn vdf_rounds_for_finalizer_rank(base_rounds: u64, rank: u32) -> Result<u64> {
    let rounds = base_rounds
        .checked_mul(u64::from(
            rank.checked_add(1).context("finalizer rank overflows")?,
        ))
        .context("finalizer rank VDF rounds overflow")?;
    if rounds > MAX_VDF_ROUNDS {
        bail!("finalizer rank VDF rounds exceed maximum");
    }
    Ok(rounds)
}

fn finalizer_rank_slot_delay_ms(rank: u32) -> Result<u64> {
    VDF_TARGET_BLOCK_MS
        .checked_mul(2)
        .context("finalizer rank time slot overflow")?
        .checked_mul(u64::from(rank))
        .context("finalizer rank time slot overflow")
}

pub(super) fn ticket_block_min_timestamp(parent: &Block, rank: u32) -> Result<u64> {
    if rank == 0 {
        return parent
            .timestamp_ms
            .checked_add(1)
            .context("finalizer rank minimum timestamp overflow");
    }

    parent
        .timestamp_ms
        .checked_add(finalizer_rank_slot_delay_ms(rank)?)
        .context("finalizer rank minimum timestamp overflow")
}

pub(super) fn base_vdf_rounds_for_finalizer_rank(vdf_rounds: u64, rank: u32) -> u64 {
    vdf_rounds / u64::from(rank.saturating_add(1).max(1))
}

pub(super) fn tickets_created_by_block(
    block: &Block,
    profile: &LaunchProfile,
) -> Result<Vec<BurnTicket>> {
    tickets_created_by_transactions(block.height, &block.transactions, profile)
}

pub(super) fn tickets_created_by_transactions(
    block_height: u64,
    transactions: &[Transaction],
    profile: &LaunchProfile,
) -> Result<Vec<BurnTicket>> {
    if profile.ticket_expiry_window_heights == 0 {
        bail!("ticket expiry window must be at least one height");
    }
    let mut tickets = Vec::new();
    for tx in transactions {
        let Transaction::Burn {
            inputs,
            amount,
            signature,
            ..
        } = tx
        else {
            continue;
        };
        let Some(owner) = inputs.first().map(|input| input.owner.clone()) else {
            continue;
        };
        if *amount == 0 {
            continue;
        }
        let target_height = block_height
            .checked_add(profile.ticket_maturity_delay_heights)
            .with_context(|| format!("ticket target height overflow at block {block_height}"))?;
        let eligible_until_height = target_height
            .checked_add(profile.ticket_expiry_window_heights - 1)
            .with_context(|| format!("ticket expiry height overflow at block {block_height}"))?;
        tickets.push(BurnTicket {
            id: signature.clone(),
            owner,
            amount: *amount,
            eligible_from_height: target_height,
            eligible_until_height,
        });
    }
    Ok(tickets)
}

pub(super) fn genesis_tickets(
    genesis_allocations: &BTreeMap<String, Amount>,
    genesis: &Block,
    profile: &LaunchProfile,
) -> Result<Vec<BurnTicket>> {
    if profile.ticket_maturity_delay_heights == 0 {
        return tickets_created_by_block(genesis, profile);
    }

    let burn_tickets = genesis
        .transactions
        .iter()
        .filter_map(|tx| {
            let Transaction::Burn {
                inputs,
                amount,
                signature,
                ..
            } = tx
            else {
                return None;
            };
            let owner = inputs.first()?.owner.clone();
            (*amount > 0).then(|| (owner, *amount, signature.clone()))
        })
        .collect::<Vec<_>>();

    if !burn_tickets.is_empty() {
        return genesis_bootstrap_tickets(burn_tickets, profile, genesis);
    }

    let Some((owner, amount)) = genesis_allocations
        .iter()
        .rev()
        .find(|(_, amount)| **amount > 0)
    else {
        return Ok(Vec::new());
    };
    genesis_bootstrap_tickets(
        vec![(
            owner.clone(),
            1,
            hex_hash(format!(
                "iuna-genesis-ticket:{owner}:{amount}:{}",
                genesis.hash
            )),
        )],
        profile,
        genesis,
    )
}

fn genesis_bootstrap_tickets(
    source_tickets: Vec<(String, Amount, String)>,
    profile: &LaunchProfile,
    genesis: &Block,
) -> Result<Vec<BurnTicket>> {
    let mut tickets = Vec::new();
    for height in 1..=profile.ticket_maturity_delay_heights {
        for (owner, amount, source_id) in &source_tickets {
            tickets.push(BurnTicket {
                id: hex_hash(format!(
                    "iuna-genesis-bootstrap-ticket:{}:{source_id}:{height}",
                    genesis.hash
                )),
                owner: owner.clone(),
                amount: *amount,
                eligible_from_height: height,
                eligible_until_height: height,
            });
        }
    }
    Ok(tickets)
}

pub(super) fn apply_finalizer_ticket_effects(
    parent: &Block,
    block: &Block,
    tickets: &mut Vec<BurnTicket>,
) -> Result<()> {
    match block.finalizer_mode {
        FinalizerMode::Ticket => consume_leader_ticket(parent, block, tickets),
        FinalizerMode::Recovery => {
            tickets.retain(|ticket| {
                !ticket_is_eligible_for_height(ticket, block.height)
                    && ticket.eligible_until_height > block.height
            });
            Ok(())
        }
    }
}

pub(super) fn consume_leader_ticket(
    parent: &Block,
    block: &Block,
    tickets: &mut Vec<BurnTicket>,
) -> Result<()> {
    let Some(proof) = &block.leader_proof else {
        bail!("block is missing leader proof");
    };
    if !tickets.iter().any(|ticket| {
        ticket.id == proof.ticket_id && ticket_is_eligible_for_height(ticket, block.height)
    }) {
        bail!("leader ticket is not pending for block {}", block.height);
    };
    let invalidated = invalidated_ticket_ids(parent, block, tickets, &proof.ticket_id);
    tickets.retain(|ticket| {
        !invalidated.contains(&ticket.id) && ticket.eligible_until_height > block.height
    });
    Ok(())
}

fn invalidated_ticket_ids(
    parent: &Block,
    block: &Block,
    tickets: &[BurnTicket],
    leader_ticket_id: &str,
) -> std::collections::BTreeSet<String> {
    let ranked_tickets = ranked_tickets_for_height(parent, block.height, tickets);
    let Some(finalizer_index) = ranked_tickets
        .iter()
        .position(|ticket| ticket.id == leader_ticket_id)
    else {
        return [leader_ticket_id.to_string()].into();
    };
    if block.height < MISSED_FALLBACK_TICKET_INVALIDATION_HEIGHT || finalizer_index == 0 {
        return [leader_ticket_id.to_string()].into();
    }

    let missed_and_finalizer_owners = ranked_tickets
        .iter()
        .take(finalizer_index + 1)
        .map(|ticket| ticket.owner.clone())
        .collect::<std::collections::BTreeSet<_>>();

    tickets
        .iter()
        .filter(|ticket| {
            ticket_is_eligible_for_height(ticket, block.height)
                && missed_and_finalizer_owners.contains(&ticket.owner)
        })
        .map(|ticket| ticket.id.clone())
        .collect()
}

pub(super) fn ticket_is_eligible_for_height(ticket: &BurnTicket, height: u64) -> bool {
    ticket.eligible_from_height <= height && height <= ticket.eligible_until_height
}

pub(super) fn mine_action_count(block: &Block) -> u64 {
    block
        .transactions
        .iter()
        .filter(|transaction| matches!(transaction, Transaction::Mine { .. }))
        .count() as u64
}
