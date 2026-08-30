use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::{
    Amount, Block, FinalizerMode, GRINDING_RESISTANCE_ACTIVATION_HEIGHT, LaunchProfile,
    MAX_VDF_ROUNDS, Transaction, VDF_TARGET_BLOCK_MS, hex_hash,
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
    let seed = ticket_draw_seed(parent, target_height, rank);
    let digest = Sha256::digest(seed.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    u128::from_be_bytes(bytes) % total_weight
}

pub(super) fn draw_parent_randomness(parent: &Block, target_height: u64) -> String {
    if target_height >= GRINDING_RESISTANCE_ACTIVATION_HEIGHT {
        format!("{}:{}", parent.vdf_seed(), parent.vdf_output)
    } else {
        format!("{}:{}", parent.hash, parent.vdf_output)
    }
}

fn ticket_draw_seed(parent: &Block, target_height: u64, rank: u32) -> String {
    let parent_randomness = draw_parent_randomness(parent, target_height);
    if rank == 0 {
        format!("iuna-ticket-draw:{target_height}:{parent_randomness}")
    } else {
        format!("iuna-ticket-draw-rank:{target_height}:{rank}:{parent_randomness}")
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BurnBundleSection, LeaderProof, TxInput};

    fn parent(height: u64) -> Block {
        Block {
            height,
            prev_hash: "0".repeat(64),
            timestamp_ms: 1_000,
            miner: "parent".to_string(),
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: 0,
            reward: 0,
            vdf_rounds: 100,
            vdf_output: "parent-vdf-output".to_string(),
            leader_proof: None,
            burn_bundle_section: BurnBundleSection::default(),
            transactions: Vec::new(),
            hash: "1".repeat(64),
        }
    }

    #[test]
    fn ticket_draw_stops_using_grindable_parent_hash_at_height_1000() {
        let mut legacy_left = parent(GRINDING_RESISTANCE_ACTIVATION_HEIGHT - 2);
        legacy_left.hash = "1".repeat(64);
        let mut legacy_right = legacy_left.clone();
        legacy_right.hash = "2".repeat(64);

        assert_ne!(
            ticket_draw_seed(&legacy_left, GRINDING_RESISTANCE_ACTIVATION_HEIGHT - 1, 0),
            ticket_draw_seed(&legacy_right, GRINDING_RESISTANCE_ACTIVATION_HEIGHT - 1, 0)
        );

        let mut activated_left = parent(GRINDING_RESISTANCE_ACTIVATION_HEIGHT - 1);
        activated_left.hash = "1".repeat(64);
        let mut activated_right = activated_left.clone();
        activated_right.hash = "2".repeat(64);
        assert_eq!(
            ticket_draw_seed(&activated_left, GRINDING_RESISTANCE_ACTIVATION_HEIGHT, 0),
            ticket_draw_seed(&activated_right, GRINDING_RESISTANCE_ACTIVATION_HEIGHT, 0)
        );
    }

    fn ticket(id: char, owner: &str, from: u64, until: u64) -> BurnTicket {
        BurnTicket {
            id: id.to_string().repeat(64),
            owner: owner.to_string(),
            amount: 1,
            eligible_from_height: from,
            eligible_until_height: until,
        }
    }

    fn ticket_block(parent: &Block, height: u64, rank: u32, selected: &BurnTicket) -> Block {
        Block {
            height,
            prev_hash: parent.hash.clone(),
            timestamp_ms: ticket_block_min_timestamp(parent, rank).unwrap(),
            miner: selected.owner.clone(),
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: rank,
            reward: 0,
            vdf_rounds: vdf_rounds_for_finalizer_rank(100, rank).unwrap(),
            vdf_output: "child-vdf-output".to_string(),
            leader_proof: Some(LeaderProof {
                ticket_id: selected.id.clone(),
                public_key: selected.owner.clone(),
                signature: "2".repeat(128),
            }),
            burn_bundle_section: BurnBundleSection::default(),
            transactions: Vec::new(),
            hash: "3".repeat(64),
        }
    }

    #[test]
    fn burns_create_tickets_after_three_blocks_for_three_heights() {
        let burn = Transaction::Burn {
            inputs: vec![TxInput {
                outpoint: super::super::OutPoint {
                    txid: "a".repeat(64),
                    index: 0,
                },
                owner: "burner".to_string(),
                signature: "b".repeat(128),
            }],
            change: Vec::new(),
            amount: 42,
            fee: 1,
            signature: "b".repeat(128),
        };

        let tickets =
            tickets_created_by_transactions(10, &[burn], &LaunchProfile::default()).unwrap();

        assert_eq!(tickets.len(), 1);
        assert_eq!(tickets[0].amount, 42);
        assert_eq!(tickets[0].eligible_from_height, 13);
        assert_eq!(tickets[0].eligible_until_height, 15);
    }

    #[test]
    fn ticket_draw_has_a_fixed_parent_vdf_height_rank_and_weight_vector() {
        let parent = parent(41);
        let tickets = vec![
            BurnTicket {
                amount: 2,
                ..ticket('a', "alice", 42, 42)
            },
            BurnTicket {
                amount: 3,
                ..ticket('b', "bob", 42, 42)
            },
            BurnTicket {
                amount: 5,
                ..ticket('c', "carol", 42, 42)
            },
        ];

        let ranked = ranked_tickets_for_height(&parent, 42, &tickets)
            .into_iter()
            .map(|ticket| ticket.id)
            .collect::<Vec<_>>();

        assert_eq!(ranked, vec!["c".repeat(64), "a".repeat(64), "b".repeat(64)]);
    }

    #[test]
    fn fallback_consumes_missed_owners_current_tickets_but_keeps_future_tickets() {
        assert_eq!(MISSED_FALLBACK_TICKET_INVALIDATION_HEIGHT, 300);
        let parent = parent(299);
        let mut tickets = vec![
            ticket('a', "alice", 300, 302),
            ticket('b', "bob", 300, 302),
            ticket('c', "carol", 300, 302),
            ticket('d', "alice", 300, 302),
            ticket('e', "bob", 301, 303),
        ];
        let ranked = ranked_tickets_for_height(&parent, 300, &tickets);
        let selected = ranked[1].clone();
        let missed_owner = ranked[0].owner.clone();
        let selected_owner = selected.owner.clone();
        let block = ticket_block(&parent, 300, 1, &selected);

        consume_leader_ticket(&parent, &block, &mut tickets).unwrap();

        assert!(tickets.iter().all(|ticket| {
            ticket.eligible_from_height > 300
                || (ticket.owner != missed_owner && ticket.owner != selected_owner)
        }));
        assert!(
            tickets.iter().any(|ticket| ticket.id == "e".repeat(64)),
            "future tickets must remain pending"
        );
    }

    #[test]
    fn fallback_before_activation_consumes_only_the_finalizing_ticket() {
        let parent = parent(MISSED_FALLBACK_TICKET_INVALIDATION_HEIGHT - 2);
        let height = MISSED_FALLBACK_TICKET_INVALIDATION_HEIGHT - 1;
        let mut tickets = vec![
            ticket('a', "alice", height, height + 2),
            ticket('b', "bob", height, height + 2),
            ticket('c', "alice", height, height + 2),
        ];
        let ranked = ranked_tickets_for_height(&parent, height, &tickets);
        let missed_ticket_id = ranked[0].id.clone();
        let selected = ranked[1].clone();
        let block = ticket_block(&parent, height, 1, &selected);

        consume_leader_ticket(&parent, &block, &mut tickets).unwrap();

        assert_eq!(tickets.len(), 2);
        assert!(tickets.iter().all(|ticket| ticket.id != selected.id));
        assert!(
            tickets.iter().any(|ticket| ticket.id == missed_ticket_id),
            "the missed rank-0 ticket must survive before activation"
        );
    }

    #[test]
    fn rank_zero_consumes_only_its_winning_ticket() {
        let parent = parent(300);
        let mut tickets = vec![
            ticket('a', "alice", 301, 303),
            ticket('b', "alice", 301, 303),
            ticket('c', "bob", 301, 303),
        ];
        let selected = ranked_tickets_for_height(&parent, 301, &tickets)[0].clone();
        let block = ticket_block(&parent, 301, 0, &selected);

        consume_leader_ticket(&parent, &block, &mut tickets).unwrap();

        assert_eq!(tickets.len(), 2);
        assert!(tickets.iter().all(|ticket| ticket.id != selected.id));
    }

    #[test]
    fn fallback_vdf_rounds_and_time_slots_scale_with_rank() {
        let parent = parent(10);

        assert_eq!(vdf_rounds_for_finalizer_rank(100, 0).unwrap(), 100);
        assert_eq!(vdf_rounds_for_finalizer_rank(100, 1).unwrap(), 200);
        assert_eq!(vdf_rounds_for_finalizer_rank(100, 2).unwrap(), 300);
        assert_eq!(ticket_block_min_timestamp(&parent, 0).unwrap(), 1_001);
        assert_eq!(
            ticket_block_min_timestamp(&parent, 1).unwrap(),
            1_000 + 2 * VDF_TARGET_BLOCK_MS
        );
        assert_eq!(
            ticket_block_min_timestamp(&parent, 2).unwrap(),
            1_000 + 4 * VDF_TARGET_BLOCK_MS
        );
    }
}
