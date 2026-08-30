use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::genesis::balances_from_utxos;
use super::mine_policy::mine_anchor;
use super::ticket::{
    BurnTicket, apply_finalizer_ticket_effects, draw_parent_randomness, genesis_tickets,
    ranked_tickets_for_height, tickets_created_by_block,
};
use super::{
    Amount, Block, BurnCommitteeMember, BurnLeaderRank, ChainSnapshot, ChainStatus, LaunchProfile,
    Ledger, OutPoint, Transaction, TxOutput, UtxoLineageRoot,
};

fn apply_historical_ticket_block(
    parent: &Block,
    block: &Block,
    launch_profile: &super::LaunchProfile,
    tickets: &mut Vec<BurnTicket>,
) -> Result<()> {
    apply_finalizer_ticket_effects(parent, block, tickets)?;
    tickets.extend(tickets_created_by_block(block, launch_profile)?);
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LineageCommitteeCandidate {
    pub(super) root: UtxoLineageRoot,
    pub(super) value: Amount,
    pub(super) weight: u64,
    pub(super) owner: String,
}

pub(super) fn lineage_committee_weight(value: Amount) -> u64 {
    let one_plus_value = u128::from(value) + 1;
    u128::BITS as u64 - one_plus_value.leading_zeros() as u64 - 1
}

pub(super) fn select_weighted_lineage_index(
    parent: &Block,
    target_height: u64,
    slot: u8,
    candidates: &[LineageCommitteeCandidate],
) -> Option<usize> {
    let total_weight = candidates.iter().try_fold(0_u128, |total, candidate| {
        total.checked_add(u128::from(candidate.weight))
    })?;
    if total_weight == 0 {
        return None;
    }
    let seed = lineage_committee_draw_seed(parent, target_height, slot);
    let digest = Sha256::digest(seed.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    let draw = u128::from_be_bytes(bytes) % total_weight;
    let mut cumulative = 0_u128;
    for (index, candidate) in candidates.iter().enumerate() {
        cumulative = cumulative.checked_add(u128::from(candidate.weight))?;
        if draw < cumulative {
            return Some(index);
        }
    }
    None
}

fn lineage_committee_draw_seed(parent: &Block, target_height: u64, slot: u8) -> String {
    let parent_randomness = draw_parent_randomness(parent, target_height);
    format!("iuna-burn-lineage-draw-v1:{target_height}:{parent_randomness}:{slot}")
}

impl Ledger {
    pub fn snapshot(&self) -> ChainSnapshot {
        ChainSnapshot {
            genesis_allocations: self.genesis_allocations.clone(),
            vdf_rounds: self.initial_vdf_rounds,
            launch_profile: self.launch_profile.clone(),
            blocks: self.chain.clone(),
        }
    }

    pub(crate) fn genesis_snapshot(&self) -> ChainSnapshot {
        ChainSnapshot {
            genesis_allocations: self.genesis_allocations.clone(),
            vdf_rounds: self.initial_vdf_rounds,
            launch_profile: self.launch_profile.clone(),
            blocks: vec![self.chain[0].clone()],
        }
    }

    pub fn status(&self) -> ChainStatus {
        self.status_with_balances(true)
    }

    pub fn light_status(&self) -> ChainStatus {
        self.status_with_balances(false)
    }

    fn status_with_balances(&self, include_balances: bool) -> ChainStatus {
        ChainStatus {
            height: self.tip().height,
            tip_hash: self.tip().hash.clone(),
            finalized_height: self
                .objective_finality_checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.height),
            finalized_hash: self
                .objective_finality_checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.hash.clone()),
            next_leader: self.expected_leader_for_next_block(),
            launch_profile_hash: self.launch_profile.hash(),
            mine_reward: self.mine_reward,
            current_mine_difficulty_bits: self.current_mine_difficulty_bits(),
            balances: if include_balances {
                balances_from_utxos(&self.utxos)
            } else {
                Default::default()
            },
            pending_transactions: self.pending.len(),
        }
    }

    pub fn tip_hash(&self) -> &str {
        &self.tip().hash
    }

    pub fn objective_finality_checkpoint(&self) -> Option<(u64, &str)> {
        self.objective_finality_checkpoint
            .as_ref()
            .map(|checkpoint| (checkpoint.height, checkpoint.hash.as_str()))
    }

    pub fn chain(&self) -> &[Block] {
        &self.chain
    }

    pub fn burn_leader_ranks_for_block(&self, height: u64) -> Result<Vec<BurnLeaderRank>> {
        Ok(self
            .burn_leader_ranks_for_blocks([height])?
            .remove(&height)
            .unwrap_or_default())
    }

    pub fn burn_leader_ranks_for_blocks<I>(
        &self,
        heights: I,
    ) -> Result<BTreeMap<u64, Vec<BurnLeaderRank>>>
    where
        I: IntoIterator<Item = u64>,
    {
        let mut requested = heights.into_iter().collect::<BTreeSet<_>>();
        let mut ranks_by_height = BTreeMap::new();
        if requested.remove(&0) {
            ranks_by_height.insert(0, Vec::new());
        }
        if requested.is_empty() {
            return Ok(ranks_by_height);
        }

        let mut tickets = genesis_tickets(
            &self.genesis_allocations,
            &self.chain[0],
            &self.launch_profile,
        )?;
        let mut next_block_index = 1;

        for height in requested {
            let parent_index = height.checked_sub(1).context("block height underflows")? as usize;
            let parent = self
                .chain
                .get(parent_index)
                .with_context(|| format!("missing parent block for height {height}"))?;
            while let Some(block) = self.chain.get(next_block_index) {
                if block.height >= height {
                    break;
                }
                let block_parent = self
                    .chain
                    .get(next_block_index - 1)
                    .with_context(|| format!("missing parent block for height {}", block.height))?;
                apply_historical_ticket_block(
                    block_parent,
                    block,
                    &self.launch_profile,
                    &mut tickets,
                )?;
                next_block_index += 1;
            }

            ranks_by_height.insert(
                height,
                ranked_tickets_for_height(parent, height, &tickets)
                    .into_iter()
                    .enumerate()
                    .map(|(rank, ticket)| BurnLeaderRank {
                        rank: rank as u32,
                        ticket_id: ticket.id,
                        owner: ticket.owner,
                        amount: ticket.amount,
                        eligible_from_height: ticket.eligible_from_height,
                        eligible_until_height: ticket.eligible_until_height,
                    })
                    .collect(),
            );
        }

        Ok(ranks_by_height)
    }

    pub fn burn_committee_for_next_block(&self) -> Vec<BurnCommitteeMember> {
        self.burn_committee_for_next_ticket_block(0)
    }

    pub fn burn_committee_for_next_ticket_block(
        &self,
        finalizer_rank: u32,
    ) -> Vec<BurnCommitteeMember> {
        self.burn_committee_for_ticket_block(self.tip().height + 1, finalizer_rank)
    }

    pub fn burn_committee_for_height(&self, height: u64) -> Vec<BurnCommitteeMember> {
        self.burn_committee_for_ticket_block(height, 0)
    }

    pub fn burn_committee_for_block(&self, block: &Block) -> Vec<BurnCommitteeMember> {
        match block.finalizer_mode {
            super::FinalizerMode::Ticket => {
                self.burn_committee_for_ticket_block(block.height, block.finalizer_rank)
            }
            super::FinalizerMode::Recovery => vec![BurnCommitteeMember {
                slot: 0,
                root: block.hash.clone(),
                owner: block.miner.clone(),
                weight: 0,
            }],
        }
    }

    fn burn_committee_for_ticket_block(
        &self,
        height: u64,
        finalizer_rank: u32,
    ) -> Vec<BurnCommitteeMember> {
        let ranked = ranked_tickets_for_height(self.tip(), height, &self.tickets);
        self.lineage_burn_committee_for_height(height, ranked, finalizer_rank)
    }

    pub fn burn_committee_memberships_for_next_block(
        &self,
        owner: &str,
    ) -> Vec<BurnCommitteeMember> {
        let max_rank = self
            .finalizer_rank_count_for_next_block()
            .min(super::BURN_COMMITTEE_SIZE);
        let mut memberships = Vec::new();
        let mut seen_slots = BTreeSet::new();
        for rank in 0..max_rank {
            for member in self.burn_committee_for_next_ticket_block(rank as u32) {
                if member.owner != owner || !seen_slots.insert(member.slot) {
                    continue;
                }
                memberships.push(member);
            }
        }
        memberships.sort_by_key(|member| member.slot);
        memberships
    }

    fn lineage_burn_committee_for_height(
        &self,
        height: u64,
        ranked: Vec<BurnTicket>,
        finalizer_rank: u32,
    ) -> Vec<BurnCommitteeMember> {
        let Some(finalizer) = ranked.get(finalizer_rank as usize) else {
            return Vec::new();
        };
        let mut committee = vec![BurnCommitteeMember {
            slot: 0,
            root: finalizer.id.clone(),
            owner: finalizer.owner.clone(),
            weight: finalizer.amount,
        }];
        let max_committee_size = super::BURN_COMMITTEE_SIZE;
        let eligible_ticket_owners = ranked
            .iter()
            .map(|ticket| ticket.owner.clone())
            .collect::<BTreeSet<_>>();
        let mut skipped_owners = ranked
            .iter()
            .take(finalizer_rank as usize)
            .map(|ticket| ticket.owner.clone())
            .collect::<BTreeSet<_>>();
        skipped_owners.insert(finalizer.owner.clone());
        let mut remaining = self
            .eligible_lineage_candidates(&skipped_owners)
            .into_iter()
            .filter_map(|candidate| {
                let owner = self.representative_owner_for_lineage_root(
                    &candidate.root,
                    &skipped_owners,
                    &eligible_ticket_owners,
                )?;
                Some(LineageCommitteeCandidate {
                    root: candidate.root,
                    value: candidate.value,
                    weight: candidate.weight,
                    owner,
                })
            })
            .collect::<Vec<_>>();

        for slot in 1..max_committee_size {
            let Some(index) =
                select_weighted_lineage_index(self.tip(), height, slot as u8, &remaining)
            else {
                break;
            };
            let selected = remaining.remove(index);
            skipped_owners.insert(selected.owner.clone());
            committee.push(BurnCommitteeMember {
                slot: slot as u8,
                root: selected.root.outpoint.id(),
                owner: selected.owner,
                weight: selected.value,
            });
            remaining.retain(|candidate| {
                candidate.root != selected.root
                    && self
                        .representative_owner_for_lineage_root(
                            &candidate.root,
                            &skipped_owners,
                            &eligible_ticket_owners,
                        )
                        .is_some()
            });
            for candidate in &mut remaining {
                candidate.owner = self
                    .representative_owner_for_lineage_root(
                        &candidate.root,
                        &skipped_owners,
                        &eligible_ticket_owners,
                    )
                    .expect("retained lineage candidate has representative owner");
            }
        }

        committee
    }

    fn eligible_lineage_candidates(
        &self,
        skipped_owners: &BTreeSet<String>,
    ) -> Vec<LineageCommitteeCandidate> {
        let parent_height = self.tip().height;
        self.lineage_values
            .iter()
            .filter(|(root, value)| {
                **value > 0
                    && root
                        .height
                        .saturating_add(self.launch_profile.burn_lineage_maturity_heights)
                        <= parent_height
                    && !skipped_owners
                        .iter()
                        .any(|owner| self.lineage_root_has_owner(root, owner))
            })
            .filter_map(|(root, value)| {
                let weight = lineage_committee_weight(*value);
                (weight > 0).then(|| LineageCommitteeCandidate {
                    root: root.clone(),
                    value: *value,
                    weight,
                    owner: String::new(),
                })
            })
            .collect()
    }

    fn lineage_root_has_owner(&self, root: &UtxoLineageRoot, owner: &str) -> bool {
        self.lineage_owners
            .get(root)
            .and_then(|owners| owners.get(owner))
            .is_some_and(|outputs| !outputs.is_empty())
    }

    fn representative_owner_for_lineage_root(
        &self,
        root: &UtxoLineageRoot,
        skipped_owners: &BTreeSet<String>,
        eligible_ticket_owners: &BTreeSet<String>,
    ) -> Option<String> {
        self.lineage_owners.get(root).and_then(|owners| {
            owners
                .iter()
                .filter(|(owner, outputs)| {
                    eligible_ticket_owners.contains(*owner)
                        && !skipped_owners.contains(*owner)
                        && !outputs.is_empty()
                })
                .filter_map(|(owner, outputs)| {
                    let (outpoint, amount) = outputs.iter().max_by(|left, right| {
                        left.1.cmp(right.1).then_with(|| right.0.cmp(left.0))
                    })?;
                    Some((owner.clone(), *amount, outpoint.clone()))
                })
                .max_by(|left, right| {
                    left.1
                        .cmp(&right.1)
                        .then_with(|| right.2.cmp(&left.2))
                        .then_with(|| right.0.cmp(&left.0))
                })
                .map(|(owner, _, _)| owner)
        })
    }

    pub fn genesis_hash(&self) -> &str {
        &self.chain[0].hash
    }

    pub(super) fn transaction_signing_domain(&self) -> super::TransactionSigningDomain {
        self.transaction_signing_domain_at(self.height().saturating_add(1))
    }

    pub(super) fn transaction_signing_domain_at(
        &self,
        height: u64,
    ) -> super::TransactionSigningDomain {
        super::TransactionSigningDomain::for_height(
            self.launch_profile.profile_id.clone(),
            self.genesis_hash().to_string(),
            height,
        )
    }

    pub fn is_setup_placeholder(&self) -> bool {
        self.height() == 0
            && self.genesis_allocations.is_empty()
            && self.chain[0].transactions.is_empty()
            && self.pending.is_empty()
    }

    pub fn height(&self) -> u64 {
        self.tip().height
    }

    pub fn recent_blocks(&self, limit: usize) -> Vec<Block> {
        self.chain.iter().rev().take(limit).cloned().collect()
    }

    pub fn blocks_before(&self, before_height: u64, limit: usize) -> Vec<Block> {
        self.chain
            .iter()
            .rev()
            .filter(|block| block.height < before_height)
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn blocks_from(&self, from_height: u64, limit: usize) -> Vec<Block> {
        if limit == 0 {
            return Vec::new();
        }
        let Ok(start) = usize::try_from(from_height) else {
            return Vec::new();
        };
        self.chain
            .get(start..)
            .unwrap_or_default()
            .iter()
            .take(limit)
            .cloned()
            .collect()
    }

    pub(crate) fn contains_block_sequence(&self, blocks: &[Block]) -> bool {
        blocks.iter().all(|block| {
            usize::try_from(block.height)
                .ok()
                .and_then(|height| self.chain.get(height))
                .is_some_and(|known| known.hash == block.hash)
        })
    }

    pub(crate) fn block_locator(&self) -> Vec<String> {
        let mut locator = Vec::new();
        let mut index = self.chain.len().saturating_sub(1);
        let mut step = 1_usize;
        loop {
            locator.push(self.chain[index].hash.clone());
            if index == 0 {
                break;
            }
            index = index.saturating_sub(step);
            if locator.len() > 10 {
                step = step.saturating_mul(2);
            }
        }
        locator
    }

    pub(crate) fn blocks_after_locator(&self, locator: &[String], limit: usize) -> Vec<Block> {
        let common_height = locator.iter().find_map(|hash| {
            self.chain
                .iter()
                .find(|block| block.hash == *hash)
                .map(|block| block.height)
        });
        common_height
            .map(|height| self.blocks_from(height.saturating_add(1), limit))
            .unwrap_or_default()
    }

    pub fn block_by_hash(&self, hash: &str) -> Option<Block> {
        self.chain.iter().find(|block| block.hash == hash).cloned()
    }

    pub fn has_block(&self, hash: &str) -> bool {
        self.chain.iter().any(|block| block.hash == hash)
    }

    pub fn pending(&self) -> &[Transaction] {
        &self.pending
    }

    pub(crate) fn clear_pending_transactions(&mut self) {
        self.pending.clear();
        self.pending_bytes = 0;
    }

    pub fn orphan_transactions(&self) -> &[Transaction] {
        &self.orphans
    }

    pub fn transaction_by_signature(&self, signature: &str) -> Option<Transaction> {
        self.pending
            .iter()
            .chain(self.orphans.iter())
            .chain(
                self.chain
                    .iter()
                    .flat_map(|block| block.transactions.iter()),
            )
            .find(|tx| tx.signature() == signature)
            .cloned()
    }

    pub fn has_transaction(&self, signature: &str) -> bool {
        self.mined_transaction_ids.contains(signature)
            || self
                .pending
                .iter()
                .chain(self.orphans.iter())
                .any(|transaction| transaction.signature() == signature)
    }

    pub fn pending_mine_count_for_anchor(&self, anchor: &str) -> usize {
        self.pending
            .iter()
            .filter(|tx| mine_anchor(tx) == Some(anchor))
            .count()
    }

    pub fn vdf_rounds(&self) -> u64 {
        self.vdf_rounds
    }

    pub fn launch_profile(&self) -> &LaunchProfile {
        &self.launch_profile
    }

    pub fn current_mine_difficulty_bits(&self) -> u32 {
        self.mine_difficulty_bits_for_anchor_height(self.tip().height)
    }

    pub fn mine_difficulty_bits_at_height(&self, height: u64) -> u32 {
        self.mine_difficulty_bits_for_anchor_height(height.min(self.tip().height))
    }

    pub fn balance_of(&self, address: &str) -> Amount {
        self.utxos
            .values()
            .filter(|output| output.address == address)
            .map(|output| output.amount)
            .sum()
    }

    pub fn utxos_for_address(&self, address: &str) -> Vec<(OutPoint, TxOutput)> {
        self.utxos
            .iter()
            .filter(|(_, output)| output.address == address)
            .map(|(outpoint, output)| (outpoint.clone(), output.clone()))
            .collect()
    }

    pub fn all_utxos(&self) -> Vec<(OutPoint, TxOutput)> {
        self.utxos
            .iter()
            .map(|(outpoint, output)| (outpoint.clone(), output.clone()))
            .collect()
    }

    pub fn available_utxos_for_address(&self, address: &str) -> Result<Vec<(OutPoint, TxOutput)>> {
        Ok(self
            .utxos_after_spendable_pending()?
            .into_iter()
            .filter(|(_, output)| output.address == address)
            .collect())
    }

    pub fn next_nonce(&self, address: &str) -> u64 {
        self.utxos
            .keys()
            .chain(
                self.pending
                    .iter()
                    .flat_map(|tx| tx.inputs().iter().map(|input| &input.outpoint)),
            )
            .filter(|outpoint| outpoint.txid.contains(address))
            .count() as u64
            + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        BURN_COMMITTEE_SIZE, GRINDING_RESISTANCE_ACTIVATION_HEIGHT, GenesisBurn, MICRO_IUNA, Wallet,
    };
    use proptest::prelude::*;
    use proptest::test_runner::Config;

    fn synthetic_committee_ledger(seed: u64) -> Ledger {
        let mut ledger = Ledger::new(BTreeMap::new(), 1);
        ledger.launch_profile.burn_lineage_maturity_heights = 0;
        let owners = (0..10)
            .map(|index| format!("owner-{seed}-{index}"))
            .collect::<Vec<_>>();

        ledger.tickets = owners
            .iter()
            .enumerate()
            .map(|(index, owner)| {
                let valid = index == 0 || seed.rotate_right(index as u32) & 1 == 1;
                let (eligible_from_height, eligible_until_height) = if valid {
                    (1, 1)
                } else if index % 2 == 0 {
                    (0, 0)
                } else {
                    (2, 3)
                };
                BurnTicket {
                    id: format!("ticket-{seed}-{index}"),
                    owner: owner.clone(),
                    amount: 1 + seed.rotate_left(index as u32) % 100,
                    eligible_from_height,
                    eligible_until_height,
                }
            })
            .collect();

        for root_index in 0_u32..8 {
            let root = UtxoLineageRoot {
                outpoint: OutPoint {
                    txid: format!("{:064x}", u128::from(seed) << 8 | u128::from(root_index)),
                    index: root_index,
                },
                height: u64::from((seed.rotate_right(root_index) & 0b11) == 0),
            };
            let first_owner = (root_index as usize + seed as usize) % owners.len();
            let second_owner = (first_owner + 1 + root_index as usize) % owners.len();
            let first_value = 1 + seed.rotate_left(root_index) % 1_000;
            let second_value = 1 + seed.rotate_right(root_index) % 1_000;
            ledger
                .lineage_values
                .insert(root.clone(), first_value + second_value);
            ledger.lineage_owners.insert(
                root,
                BTreeMap::from([
                    (
                        owners[first_owner].clone(),
                        BTreeMap::from([(
                            OutPoint {
                                txid: format!("{:064x}", 0x100_u128 + root_index as u128),
                                index: 0,
                            },
                            first_value,
                        )]),
                    ),
                    (
                        owners[second_owner].clone(),
                        BTreeMap::from([(
                            OutPoint {
                                txid: format!("{:064x}", 0x200_u128 + root_index as u128),
                                index: 0,
                            },
                            second_value,
                        )]),
                    ),
                ]),
            );
        }
        ledger
    }

    #[test]
    fn lineage_committee_selects_a_ticket_owner_within_the_selected_root() {
        let finalizer = Wallet::from_seed("lineage-ticket-finalizer");
        let ticket_owner = Wallet::from_seed("lineage-ticket-owner");
        let non_ticket_owner = Wallet::from_seed("lineage-non-ticket-owner");
        let mut ledger = Ledger::new_with_genesis_burns(
            BTreeMap::from([(finalizer.address().to_string(), 10 * MICRO_IUNA)]),
            vec![GenesisBurn::new(finalizer.address(), MICRO_IUNA)],
            1,
        )
        .unwrap();
        ledger.launch_profile.burn_lineage_maturity_heights = 0;

        let ticket_root = UtxoLineageRoot {
            outpoint: OutPoint {
                txid: "1".repeat(64),
                index: 0,
            },
            height: 0,
        };
        ledger.lineage_values.insert(ticket_root.clone(), 110);
        ledger.lineage_owners.insert(
            ticket_root.clone(),
            BTreeMap::from([
                (
                    non_ticket_owner.address().to_string(),
                    BTreeMap::from([(
                        OutPoint {
                            txid: "2".repeat(64),
                            index: 0,
                        },
                        100,
                    )]),
                ),
                (
                    ticket_owner.address().to_string(),
                    BTreeMap::from([(
                        OutPoint {
                            txid: "3".repeat(64),
                            index: 0,
                        },
                        10,
                    )]),
                ),
            ]),
        );

        let non_ticket_root = UtxoLineageRoot {
            outpoint: OutPoint {
                txid: "4".repeat(64),
                index: 0,
            },
            height: 0,
        };
        ledger.lineage_values.insert(non_ticket_root.clone(), 1_000);
        ledger.lineage_owners.insert(
            non_ticket_root,
            BTreeMap::from([(
                non_ticket_owner.address().to_string(),
                BTreeMap::from([(
                    OutPoint {
                        txid: "5".repeat(64),
                        index: 0,
                    },
                    1_000,
                )]),
            )]),
        );

        let ranked = vec![
            BurnTicket {
                id: "finalizer-ticket".to_string(),
                owner: finalizer.address().to_string(),
                amount: 1,
                eligible_from_height: 1,
                eligible_until_height: 1,
            },
            BurnTicket {
                id: "committee-ticket".to_string(),
                owner: ticket_owner.address().to_string(),
                amount: 1,
                eligible_from_height: 1,
                eligible_until_height: 1,
            },
        ];

        let committee = ledger.lineage_burn_committee_for_height(1, ranked, 0);

        assert_eq!(committee.len(), 2);
        assert_eq!(committee[1].root, ticket_root.outpoint.id());
        assert_eq!(committee[1].owner, ticket_owner.address());
        assert_ne!(committee[1].owner, non_ticket_owner.address());
    }

    #[test]
    fn lineage_weight_is_floor_log2_of_one_plus_root_value() {
        assert_eq!(lineage_committee_weight(0), 0);
        assert_eq!(lineage_committee_weight(1), 1);
        assert_eq!(lineage_committee_weight(2), 1);
        assert_eq!(lineage_committee_weight(3), 2);
        assert_eq!(lineage_committee_weight(7), 3);
        assert_eq!(lineage_committee_weight(8), 3);
        assert_eq!(lineage_committee_weight(u64::MAX), 64);
    }

    #[test]
    fn committee_draw_seed_has_a_fixed_legacy_parent_hash_height_and_slot_vector() {
        let mut parent = Ledger::new(BTreeMap::new(), 1).tip().clone();
        parent.hash = "a".repeat(64);
        parent.vdf_output = "parent-vdf".to_string();
        let candidates = [1_u64, 2, 4]
            .into_iter()
            .enumerate()
            .map(|(index, weight)| LineageCommitteeCandidate {
                root: UtxoLineageRoot {
                    outpoint: OutPoint {
                        txid: format!("{:064x}", index + 1),
                        index: 0,
                    },
                    height: 1,
                },
                value: weight,
                weight,
                owner: format!("owner-{index}"),
            })
            .collect::<Vec<_>>();

        assert_eq!(
            select_weighted_lineage_index(&parent, 42, 1, &candidates),
            Some(2)
        );
    }

    #[test]
    fn committee_draw_stops_using_grindable_parent_hash_at_height_1000() {
        let mut parent = Ledger::new(BTreeMap::new(), 1).tip().clone();
        parent.height = GRINDING_RESISTANCE_ACTIVATION_HEIGHT - 1;
        parent.hash = "1".repeat(64);
        parent.vdf_output = "parent-vdf".to_string();
        let mut alternate_hash = parent.clone();
        alternate_hash.hash = "2".repeat(64);

        assert_ne!(
            lineage_committee_draw_seed(&parent, GRINDING_RESISTANCE_ACTIVATION_HEIGHT - 1, 1,),
            lineage_committee_draw_seed(
                &alternate_hash,
                GRINDING_RESISTANCE_ACTIVATION_HEIGHT - 1,
                1,
            )
        );
        assert_eq!(
            lineage_committee_draw_seed(&parent, GRINDING_RESISTANCE_ACTIVATION_HEIGHT, 1),
            lineage_committee_draw_seed(&alternate_hash, GRINDING_RESISTANCE_ACTIVATION_HEIGHT, 1,)
        );

        let mut alternate_output = parent;
        alternate_output.vdf_output = "different-vdf".to_string();
        assert_ne!(
            lineage_committee_draw_seed(
                &alternate_output,
                GRINDING_RESISTANCE_ACTIVATION_HEIGHT,
                1,
            ),
            lineage_committee_draw_seed(&alternate_hash, GRINDING_RESISTANCE_ACTIVATION_HEIGHT, 1,)
        );
    }

    proptest! {
        #![proptest_config(Config { cases: 128, .. Config::default() })]

        #[test]
        fn committee_members_are_eligible_ticket_owners_in_distinct_mature_roots(
            seed in any::<u64>(),
            requested_rank in 0usize..10,
        ) {
            let ledger = synthetic_committee_ledger(seed);
            let target_height = ledger.height() + 1;
            let ranked = ranked_tickets_for_height(ledger.tip(), target_height, &ledger.tickets);
            let rank = requested_rank % ranked.len();
            let committee = ledger.burn_committee_for_next_ticket_block(rank as u32);
            let repeated = ledger.burn_committee_for_next_ticket_block(rank as u32);
            let excluded_owners = ranked
                .iter()
                .take(rank + 1)
                .map(|ticket| ticket.owner.as_str())
                .collect::<BTreeSet<_>>();

            prop_assert_eq!(&committee, &repeated, "committee selection must be deterministic");
            prop_assert!(!committee.is_empty());
            prop_assert!(committee.len() <= BURN_COMMITTEE_SIZE);

            let mut owners = BTreeSet::new();
            let mut roots = BTreeSet::new();
            for (index, member) in committee.iter().enumerate() {
                prop_assert_eq!(usize::from(member.slot), index, "committee slots must be contiguous");
                prop_assert!(owners.insert(member.owner.as_str()), "committee owners must be unique");
                prop_assert!(
                    ledger.tickets.iter().any(|ticket| {
                        ticket.owner == member.owner
                            && ticket.eligible_from_height <= target_height
                            && target_height <= ticket.eligible_until_height
                    }),
                    "committee member {} has no ticket for height {target_height}",
                    member.owner,
                );

                if member.slot == 0 {
                    prop_assert_eq!(&member.owner, &ranked[rank].owner);
                    prop_assert_eq!(&member.root, &ranked[rank].id);
                    prop_assert_eq!(member.weight, ranked[rank].amount);
                    continue;
                }

                prop_assert!(!excluded_owners.contains(member.owner.as_str()));
                let (root, root_owners) = ledger
                    .lineage_owners
                    .iter()
                    .find(|(root, _)| root.outpoint.id() == member.root)
                    .expect("selected committee root must exist");
                prop_assert!(roots.insert(root.clone()), "lineage roots must be unique");
                prop_assert_eq!(member.weight, ledger.lineage_values[root]);
                prop_assert!(
                    root.height
                        .saturating_add(ledger.launch_profile.burn_lineage_maturity_heights)
                        <= ledger.tip().height,
                    "committee root must be mature",
                );
                prop_assert!(
                    root_owners
                        .get(&member.owner)
                        .is_some_and(|outputs| !outputs.is_empty()),
                    "committee member must currently belong to the selected lineage root",
                );
                prop_assert!(
                    root_owners
                        .keys()
                        .all(|owner| !excluded_owners.contains(owner.as_str())),
                    "a root containing a missed owner or finalizer must be excluded",
                );
            }
        }
    }
}
