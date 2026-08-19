use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use proptest::test_runner::Config;

use super::ledger_ops::block_reward;
use super::reveal::{BurnBundlePayload, burn_bundle_slot_mask};
use super::ticket::ticket_block_min_timestamp;
use super::{
    Amount, BURN_LINEAGE_MATURITY_HEIGHTS, Block, BurnBundle, BurnBundleSignature,
    BurnCommitteeMember, BurnLeaderRank, ChainSnapshot, GenesisBurn, Ledger, MICRO_IUNA,
    MaskedBurn, Transaction, VDF_TARGET_BLOCK_MS, Wallet, run_vdf,
};

const NOW_MS: u64 = 10_000_000_000;
const LEVELS: [u8; 6] = [1, 5, 10, 25, 33, 50];
const COMBINED_LEVELS: [(u8, u8); 5] = [(10, 1), (10, 10), (25, 5), (25, 25), (50, 10)];
const LINEAGE_RESOURCE_ROOTS: usize = 10;

#[derive(Clone, Copy, Debug)]
enum AdversaryStrategy {
    Honest,
    CensorBurns,
    MaximizeBurnWeight,
    MaximizeCommitteeWeight,
    WithholdBurnFromCommittee,
    MissRank0,
    ForceFallback,
    AttemptRecovery,
    AddressRotation,
    CombinedStrategy,
}

impl AdversaryStrategy {
    fn from_index(index: usize) -> Self {
        match index % 10 {
            0 => Self::Honest,
            1 => Self::CensorBurns,
            2 => Self::MaximizeBurnWeight,
            3 => Self::MaximizeCommitteeWeight,
            4 => Self::WithholdBurnFromCommittee,
            5 => Self::MissRank0,
            6 => Self::ForceFallback,
            7 => Self::AttemptRecovery,
            8 => Self::AddressRotation,
            _ => Self::CombinedStrategy,
        }
    }
}

#[derive(Clone, Debug)]
struct Actor {
    wallet: Wallet,
    burn_wallets: Vec<Wallet>,
    lineage_wallets: Vec<Wallet>,
    strategy: AdversaryStrategy,
}

#[derive(Clone, Copy, Debug)]
struct ResourceLevel {
    burn_percent: u8,
    lineage_percent: u8,
}

#[derive(Clone, Debug, Default)]
struct AdversarialMetrics {
    attacker_finalization_share: f64,
    attacker_committee_share: f64,
    third_party_burn_censorship_rate: f64,
    fallback_rate: f64,
    recovery_rate: f64,
    average_blocks_until_recovery: f64,
    attacker_burn_cost: Amount,
    attacker_net_reward: i128,
}

struct Harness {
    ledger: Ledger,
    attacker: Actor,
    honest: Vec<Wallet>,
    wallets: BTreeMap<String, Wallet>,
    seed: u64,
    resource: ResourceLevel,
}

impl Harness {
    fn new(seed: u64, burn_percent: u8, lineage_percent: u8, strategy: AdversaryStrategy) -> Self {
        let attacker_wallet = Wallet::from_seed(&format!("adv-{seed}-attacker-main"));
        let attacker_burn_wallets = (0..4)
            .map(|index| Wallet::from_seed(&format!("adv-{seed}-attacker-burn-{index}")))
            .collect::<Vec<_>>();
        let attacker_lineage_wallets = (0..4)
            .map(|index| Wallet::from_seed(&format!("adv-{seed}-attacker-lineage-{index}")))
            .collect::<Vec<_>>();
        let honest = (0..8)
            .map(|index| Wallet::from_seed(&format!("adv-{seed}-honest-{index}")))
            .collect::<Vec<_>>();
        let mut all_wallets = vec![attacker_wallet.clone()];
        all_wallets.extend(attacker_burn_wallets.clone());
        all_wallets.extend(attacker_lineage_wallets.clone());
        all_wallets.extend(honest.clone());

        let mut allocations = BTreeMap::new();
        for wallet in &all_wallets {
            allocations.insert(wallet.address().to_string(), 10_000 * MICRO_IUNA);
        }
        let attacker_burn = u64::from(burn_percent).max(1) * MICRO_IUNA;
        let honest_burn_total = u64::from(100_u8.saturating_sub(burn_percent)).max(1) * MICRO_IUNA;
        let honest_burn_each = (honest_burn_total / honest.len() as u64).max(1);
        let mut genesis_burns = vec![GenesisBurn::new(attacker_wallet.address(), attacker_burn)];
        genesis_burns.extend(
            honest
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), honest_burn_each)),
        );
        let mut ledger = Ledger::new_with_genesis_burns(allocations, genesis_burns, 1).unwrap();
        ledger.launch_profile.mine_difficulty_bits = 0;

        let wallets = all_wallets
            .into_iter()
            .map(|wallet| (wallet.address().to_string(), wallet))
            .collect::<BTreeMap<_, _>>();

        Self {
            ledger,
            attacker: Actor {
                wallet: attacker_wallet,
                burn_wallets: attacker_burn_wallets,
                lineage_wallets: attacker_lineage_wallets,
                strategy,
            },
            honest,
            wallets,
            seed,
            resource: ResourceLevel {
                burn_percent,
                lineage_percent,
            },
        }
    }

    fn wallet(&self, address: &str) -> &Wallet {
        self.wallets
            .get(address)
            .unwrap_or_else(|| panic!("seed {} missing wallet {address}", self.seed))
    }

    fn next_rank(&self, rank: usize) -> BurnLeaderRank {
        self.ledger
            .burn_leader_ranks_for_block(self.ledger.height() + 1)
            .unwrap()
            .get(rank)
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "seed {} has no rank {rank} at height {}",
                    self.seed,
                    self.ledger.height() + 1
                )
            })
    }

    fn submit_anchor_burn(&mut self, wallet: &Wallet) -> Transaction {
        let burn = self
            .ledger
            .build_burn(wallet, 1, 1)
            .unwrap_or_else(|error| {
                panic!(
                    "seed {} failed to build anchor burn for {}: {error:#}",
                    self.seed,
                    wallet.address()
                )
            });
        self.ledger.submit_transaction(burn.clone()).unwrap();
        burn
    }

    fn submit_fee_burn(&mut self, wallet: &Wallet, amount: Amount, fee: Amount) -> Transaction {
        let burn = self
            .ledger
            .build_burn(wallet, amount, fee)
            .unwrap_or_else(|error| {
                panic!(
                    "seed {} failed to build fee burn for {}: {error:#}",
                    self.seed,
                    wallet.address()
                )
            });
        self.ledger.submit_transaction(burn.clone()).unwrap();
        burn
    }

    fn committee_bundles(&self) -> Vec<BurnBundle> {
        self.ledger
            .burn_committee_for_next_block()
            .into_iter()
            .filter_map(|member| {
                self.wallets
                    .get(&member.owner)
                    .and_then(|wallet| self.ledger.build_burn_bundle(wallet).unwrap())
            })
            .collect()
    }

    fn prepared_ticket_block(&mut self, rank: usize, bundles: Vec<BurnBundle>) -> Block {
        let leader = self.next_rank(rank);
        let wallet = self.wallet(&leader.owner).clone();
        self.submit_anchor_burn(&wallet);
        self.finish_ticket_block_from_pending(rank, bundles)
    }

    fn finish_ticket_block_from_pending(&self, rank: usize, bundles: Vec<BurnBundle>) -> Block {
        let leader = self.next_rank(rank);
        let wallet = self.wallet(&leader.owner).clone();
        let timestamp = self
            .ledger
            .tip()
            .timestamp_ms
            .saturating_add(VDF_TARGET_BLOCK_MS * 2 * rank as u64)
            .saturating_add(1);
        let prepared = self
            .ledger
            .prepare_next_block_with_burn_bundles(wallet.address(), timestamp, bundles)
            .unwrap_or_else(|error| {
                panic!(
                    "seed {} failed to prepare ticket block rank {rank}: {error:#}",
                    self.seed
                )
            });
        let vdf_output = run_vdf(prepared.vdf_seed(), prepared.vdf_rounds());
        prepared.finish(&wallet, vdf_output)
    }

    fn mine_ticket_block(&mut self, rank: usize) -> Block {
        let leader = self.next_rank(rank);
        let wallet = self.wallet(&leader.owner).clone();
        self.submit_anchor_burn(&wallet);
        let bundles = self.committee_bundles();
        let block = self.finish_ticket_block_from_pending(rank, bundles);
        self.ledger
            .apply_block_at(block.clone(), NOW_MS.saturating_add(block.timestamp_ms))
            .unwrap_or_else(|error| {
                panic!(
                    "seed {} validator rejected generated ticket block: {error:#}",
                    self.seed
                )
            });
        block
    }

    fn mine_recovery_block(&mut self, wallet: &Wallet) -> Block {
        self.submit_anchor_burn(wallet);
        let timestamp = self.ledger.recovery_block_min_timestamp();
        let prepared = self
            .ledger
            .prepare_recovery_block(wallet.address(), timestamp)
            .unwrap();
        let vdf_output = run_vdf(prepared.vdf_seed(), prepared.vdf_rounds());
        let block = prepared.finish(wallet, vdf_output);
        self.ledger
            .apply_block_at(block.clone(), NOW_MS.saturating_add(block.timestamp_ms))
            .unwrap();
        block
    }

    fn mature_lineages(&mut self, attacker_roots: usize, honest_roots: usize) {
        let mut roots_remaining = attacker_roots + honest_roots;
        let mut latest_root_height = self.ledger.height();
        while roots_remaining > 0 {
            let anchor = self.ledger.tip_hash().to_string();
            for index in 0..2 {
                if roots_remaining == 0 {
                    break;
                }
                let wallet = if roots_remaining > honest_roots {
                    self.attacker.lineage_wallets[index % self.attacker.lineage_wallets.len()]
                        .clone()
                } else {
                    self.honest[index % self.honest.len()].clone()
                };
                let mine = self
                    .ledger
                    .build_mine(wallet.address())
                    .unwrap_or_else(|error| {
                        panic!(
                            "seed {} failed to build mine for anchor {anchor}: {error:#}",
                            self.seed
                        )
                    });
                self.ledger.submit_transaction(mine).unwrap();
                roots_remaining -= 1;
            }
            self.mine_ticket_block(0);
            latest_root_height = self.ledger.height();
        }
        while self.ledger.height()
            < latest_root_height
                .saturating_add(BURN_LINEAGE_MATURITY_HEIGHTS)
                .saturating_add(1)
        {
            self.mine_ticket_block(0);
        }
    }

    fn mature_resource_lineages(&mut self) {
        let (attacker_roots, honest_roots) = lineage_resource_roots(self.resource.lineage_percent);
        self.mature_lineages(attacker_roots, honest_roots);
    }

    fn attacker_addresses(&self) -> BTreeSet<String> {
        std::iter::once(self.attacker.wallet.address().to_string())
            .chain(
                self.attacker
                    .burn_wallets
                    .iter()
                    .map(|wallet| wallet.address().to_string()),
            )
            .chain(
                self.attacker
                    .lineage_wallets
                    .iter()
                    .map(|wallet| wallet.address().to_string()),
            )
            .collect()
    }

    fn run_strategy(&mut self, blocks: usize) -> AdversarialMetrics {
        self.mature_resource_lineages();
        let attacker_addresses = self.attacker_addresses();
        let mut metrics = AdversarialMetrics::default();
        let mut third_party_burns = 0usize;
        let mut censored_third_party_burns = 0usize;
        let mut finalizations = 0usize;
        let mut attacker_finalizations = 0usize;
        let mut committee_slots = 0usize;
        let mut attacker_committee_slots = 0usize;
        let mut fallback_blocks = 0usize;
        let mut recovery_blocks = 0usize;
        let mut blocks_until_recovery = Vec::new();

        for step in 0..blocks {
            let victim = self.honest[step % self.honest.len()].clone();
            let third_party_burn = if matches!(
                self.attacker.strategy,
                AdversaryStrategy::CensorBurns
                    | AdversaryStrategy::WithholdBurnFromCommittee
                    | AdversaryStrategy::CombinedStrategy
            ) {
                third_party_burns += 1;
                if matches!(
                    self.attacker.strategy,
                    AdversaryStrategy::WithholdBurnFromCommittee
                        | AdversaryStrategy::CombinedStrategy
                ) {
                    Some(self.ledger.build_burn(&victim, 1, 1).unwrap())
                } else {
                    Some(self.submit_fee_burn(&victim, 1, 1))
                }
            } else {
                None
            };

            let committee = self.ledger.burn_committee_for_next_block();
            committee_slots += committee.len();
            attacker_committee_slots += committee
                .iter()
                .filter(|member| attacker_addresses.contains(&member.owner))
                .count();

            let rank = match self.attacker.strategy {
                AdversaryStrategy::MissRank0
                | AdversaryStrategy::ForceFallback
                | AdversaryStrategy::CombinedStrategy => {
                    usize::from(self.ledger.finalizer_rank_count_for_next_block() > 1)
                }
                _ => 0,
            };
            let block = if matches!(
                self.attacker.strategy,
                AdversaryStrategy::AttemptRecovery | AdversaryStrategy::CombinedStrategy
            ) && self
                .ledger
                .recovery_block_available_at(self.ledger.recovery_block_min_timestamp())
                && step % 7 == 6
            {
                recovery_blocks += 1;
                blocks_until_recovery.push((step + 1) as f64);
                let wallet = self.attacker.wallet.clone();
                self.mine_recovery_block(&wallet)
            } else {
                if rank > 0 {
                    fallback_blocks += 1;
                }
                self.mine_ticket_block(rank)
            };

            finalizations += 1;
            if attacker_addresses.contains(&block.miner) {
                attacker_finalizations += 1;
                metrics.attacker_burn_cost = metrics.attacker_burn_cost.saturating_add(1);
                metrics.attacker_net_reward += i128::from(block.reward);
            }
            if let Some(burn) = third_party_burn {
                censored_third_party_burns += usize::from(
                    !block
                        .transactions
                        .iter()
                        .any(|tx| tx.signature() == burn.signature()),
                );
            }
        }

        metrics.attacker_finalization_share = share(attacker_finalizations, finalizations);
        metrics.attacker_committee_share = share(attacker_committee_slots, committee_slots);
        metrics.third_party_burn_censorship_rate =
            share(censored_third_party_burns, third_party_burns);
        metrics.fallback_rate = share(fallback_blocks, blocks);
        metrics.recovery_rate = share(recovery_blocks, blocks);
        metrics.average_blocks_until_recovery = if blocks_until_recovery.is_empty() {
            0.0
        } else {
            blocks_until_recovery.iter().sum::<f64>() / blocks_until_recovery.len() as f64
        };
        metrics.attacker_net_reward -= i128::from(metrics.attacker_burn_cost);
        metrics
    }
}

fn share(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn lineage_resource_roots(lineage_percent: u8) -> (usize, usize) {
    let attacker_roots = (usize::from(lineage_percent) * LINEAGE_RESOURCE_ROOTS).div_ceil(100);
    let attacker_roots = attacker_roots.clamp(1, LINEAGE_RESOURCE_ROOTS);
    (attacker_roots, LINEAGE_RESOURCE_ROOTS - attacker_roots)
}

fn rehash(block: &mut Block) {
    block.reward = block_reward(&block.transactions, 0).unwrap();
    block.hash = block.compute_hash();
}

fn assert_rejects(mut ledger: Ledger, block: Block, label: &str) {
    assert!(
        ledger.apply_block_at(block, NOW_MS).is_err(),
        "{label} was accepted by the consensus validator"
    );
}

fn harness_for_percent(seed: u64, burn_percent: u8) -> Harness {
    Harness::new(seed, burn_percent, 10, AdversaryStrategy::Honest)
}

fn mutate_signature(signature: &mut String) {
    let replacement = if signature.starts_with('0') { "1" } else { "0" };
    signature.replace_range(0..1, replacement);
}

fn committee_roots_are_unique(committee: &[BurnCommitteeMember]) -> bool {
    let mut roots = BTreeSet::new();
    committee
        .iter()
        .filter(|member| member.slot > 0)
        .all(|member| roots.insert(member.root.clone()))
}

proptest! {
    #![proptest_config(Config { cases: 32, .. Config::default() })]

    #[test]
    fn required_attested_burn_must_be_included(seed in any::<u64>(), burn_idx in 0usize..LEVELS.len()) {
        let mut harness = harness_for_percent(seed, LEVELS[burn_idx]);
        let leader = harness.next_rank(0);
        let finalizer = harness.wallet(&leader.owner).clone();
        harness.submit_anchor_burn(&finalizer);
        let victim = harness
            .honest
            .iter()
            .find(|wallet| wallet.address() != finalizer.address())
            .unwrap()
            .clone();
        let victim_burn = harness.submit_fee_burn(&victim, 1, 1);
        let bundle = finalizer.burn_bundle(BurnBundlePayload {
            height: harness.ledger.height() + 1,
            prev_hash: harness.ledger.tip_hash().to_string(),
            slot: 0,
            member: finalizer.address().to_string(),
            burns: vec![victim_burn.clone()],
        });
        let mut block = harness.finish_ticket_block_from_pending(0, vec![bundle]);
        block.transactions.retain(|tx| tx.signature() != victim_burn.signature());
        rehash(&mut block);

        prop_assert!(
            harness.ledger.apply_block_at(block, NOW_MS).is_err(),
            "seed={} burn_percent={} strategy={:?} accepted a block missing attested burn {}",
            seed,
            LEVELS[burn_idx],
            harness.attacker.strategy,
            victim_burn.signature()
        );
    }

    #[test]
    fn timing_and_vdf_mutations_are_rejected(seed in any::<u64>(), rank in 1usize..3) {
        let mut harness = harness_for_percent(seed, 25);
        let leader = harness.next_rank(rank);
        let wallet = harness.wallet(&leader.owner).clone();
        let parent = harness.ledger.tip().clone();
        let block = harness.prepared_ticket_block(rank, Vec::new());

        let mut early = block.clone();
        early.timestamp_ms = ticket_block_min_timestamp(&parent, rank as u32).unwrap() - 1;
        rehash(&mut early);
        prop_assert!(
            harness.ledger.clone().apply_block_at(early, NOW_MS).is_err(),
            "seed={} rank={} accepted early fallback timestamp for {}",
            seed,
            rank,
            wallet.address()
        );

        let mut future = block.clone();
        future.timestamp_ms = NOW_MS + super::MAX_BLOCK_TIMESTAMP_FUTURE_DRIFT_MS + 1;
        rehash(&mut future);
        prop_assert!(
            harness.ledger.clone().apply_block_at(future, NOW_MS).is_err(),
            "seed={} rank={} accepted future drift timestamp",
            seed,
            rank
        );
    }

    #[test]
    fn committee_selection_is_deterministic_and_sybil_resistant(seed in any::<u64>(), lineage_idx in 0usize..LEVELS.len()) {
        let mut harness = Harness::new(seed, 10, LEVELS[lineage_idx], AdversaryStrategy::AddressRotation);
        harness.mature_lineages(2, 4);
        let committee = harness.ledger.burn_committee_for_next_block();
        let snapshot_committee = Ledger::from_snapshot_at(harness.ledger.snapshot(), NOW_MS)
            .unwrap()
            .burn_committee_for_next_block();

        prop_assert_eq!(committee.clone(), snapshot_committee, "seed={} committee selection is not deterministic", seed);
        prop_assert!(committee_roots_are_unique(&committee), "seed={} selected one lineage more than once: {:?}", seed, committee);
        let non_finalizer_roots = committee.iter().filter(|member| member.slot > 0).count();
        prop_assert!(non_finalizer_roots < super::BURN_COMMITTEE_SIZE);
    }

    #[test]
    fn adversarial_state_machine_keeps_invalid_paths_out(
        seed in any::<u64>(),
        strategy_idx in 0usize..10,
        level_idx in 0usize..LEVELS.len(),
        blocks in 3usize..8,
    ) {
        let strategy = AdversaryStrategy::from_index(strategy_idx);
        let level = LEVELS[level_idx];
        let mut harness = Harness::new(seed, level, level, strategy);
        let metrics = harness.run_strategy(blocks);
        prop_assert!(metrics.attacker_finalization_share.is_finite());
        prop_assert!(metrics.attacker_committee_share.is_finite());
        match strategy {
            AdversaryStrategy::Honest
            | AdversaryStrategy::MaximizeBurnWeight
            | AdversaryStrategy::MaximizeCommitteeWeight
            | AdversaryStrategy::AddressRotation => {
                prop_assert_eq!(metrics.third_party_burn_censorship_rate, 0.0);
                prop_assert_eq!(metrics.fallback_rate, 0.0);
                prop_assert_eq!(metrics.recovery_rate, 0.0);
            }
            AdversaryStrategy::CensorBurns => {
                prop_assert_eq!(metrics.third_party_burn_censorship_rate, 0.0);
            }
            AdversaryStrategy::WithholdBurnFromCommittee => {
                prop_assert!(metrics.third_party_burn_censorship_rate > 0.0);
            }
            AdversaryStrategy::MissRank0 | AdversaryStrategy::ForceFallback => {
                prop_assert!(metrics.fallback_rate > 0.0);
                prop_assert_eq!(metrics.recovery_rate, 0.0);
            }
            AdversaryStrategy::AttemptRecovery => {
                if blocks >= 7 {
                    prop_assert!(metrics.recovery_rate > 0.0);
                } else {
                    prop_assert_eq!(metrics.recovery_rate, 0.0);
                }
            }
            AdversaryStrategy::CombinedStrategy => {
                prop_assert!(metrics.third_party_burn_censorship_rate > 0.0);
                prop_assert!(metrics.fallback_rate > 0.0);
            }
        }
    }
}

#[test]
fn attested_burn_is_not_selected_again_as_normal_transaction() {
    let mut harness = harness_for_percent(10, 25);
    let leader = harness.next_rank(0);
    let finalizer = harness.wallet(&leader.owner).clone();
    harness.submit_anchor_burn(&finalizer);
    let victim = harness
        .honest
        .iter()
        .find(|wallet| wallet.address() != finalizer.address())
        .unwrap()
        .clone();
    let attested_burn = harness.submit_fee_burn(&victim, 1, 1);
    let extra_burn = harness.submit_fee_burn(&victim, 1, 1);
    let bundle = finalizer.burn_bundle(BurnBundlePayload {
        height: harness.ledger.height() + 1,
        prev_hash: harness.ledger.tip_hash().to_string(),
        slot: 0,
        member: finalizer.address().to_string(),
        burns: vec![attested_burn.clone()],
    });

    let block = harness.finish_ticket_block_from_pending(0, vec![bundle]);

    assert_eq!(
        block
            .burn_bundle_section
            .burns
            .iter()
            .filter(|masked| masked.burn.signature() == attested_burn.signature())
            .count(),
        1
    );
    assert_eq!(
        block
            .transactions
            .iter()
            .filter(|tx| tx.signature() == attested_burn.signature())
            .count(),
        1
    );
    assert!(
        block
            .transactions
            .iter()
            .any(|tx| tx.signature() == extra_burn.signature())
    );
}

#[test]
fn attested_burn_block_validates_independent_of_local_mempool() {
    let mut harness = harness_for_percent(22, 25);
    let leader = harness.next_rank(0);
    let finalizer = harness.wallet(&leader.owner).clone();
    harness.submit_anchor_burn(&finalizer);
    let victim = harness
        .honest
        .iter()
        .find(|wallet| wallet.address() != finalizer.address())
        .unwrap()
        .clone();
    let attested_burn = harness.submit_fee_burn(&victim, 1, 1);
    let bundle = finalizer.burn_bundle(BurnBundlePayload {
        height: harness.ledger.height() + 1,
        prev_hash: harness.ledger.tip_hash().to_string(),
        slot: 0,
        member: finalizer.address().to_string(),
        burns: vec![attested_burn.clone()],
    });
    let block = harness.finish_ticket_block_from_pending(0, vec![bundle]);
    let parent_snapshot = harness.ledger.snapshot();

    let mut empty_mempool = Ledger::from_snapshot_at(parent_snapshot.clone(), NOW_MS).unwrap();
    empty_mempool
        .apply_block_at(block.clone(), NOW_MS.saturating_add(block.timestamp_ms))
        .unwrap();

    let mut conflicting_mempool = Ledger::from_snapshot_at(parent_snapshot, NOW_MS).unwrap();
    let conflict = conflicting_mempool
        .build_transfer(&victim, finalizer.address(), 1, 1)
        .unwrap();
    conflicting_mempool
        .submit_transaction(conflict.clone())
        .unwrap();
    conflicting_mempool
        .apply_block_at(block, NOW_MS.saturating_add(1))
        .unwrap();
    assert!(
        conflicting_mempool
            .pending()
            .iter()
            .all(|tx| tx.signature() != conflict.signature())
    );
}

#[test]
fn required_burn_cannot_be_executed_twice_in_one_block() {
    let mut harness = harness_for_percent(11, 25);
    let leader = harness.next_rank(0);
    let finalizer = harness.wallet(&leader.owner).clone();
    harness.submit_anchor_burn(&finalizer);
    let victim = harness
        .honest
        .iter()
        .find(|wallet| wallet.address() != finalizer.address())
        .unwrap()
        .clone();
    let burn = harness.submit_fee_burn(&victim, 1, 1);
    let mut block = harness.finish_ticket_block_from_pending(0, Vec::new());
    block.transactions.push(burn.clone());
    rehash(&mut block);

    assert_rejects(harness.ledger, block, "duplicate burn transaction");
}

#[test]
fn misused_or_extra_committee_bundle_is_rejected() {
    let mut harness = harness_for_percent(12, 25);
    let mut block = harness.prepared_ticket_block(0, Vec::new());
    block
        .burn_bundle_section
        .signatures
        .push(BurnBundleSignature {
            slot: 1,
            member: harness.attacker.wallet.address().to_string(),
            signature: "00".repeat(64),
        });
    rehash(&mut block);

    assert_rejects(harness.ledger, block, "extra committee signature");
}

#[test]
fn finalizer_anchor_alone_does_not_require_committee_signatures() {
    let mut harness = harness_for_percent(20, 25);
    harness.mature_lineages(1, 4);
    let block = harness.prepared_ticket_block(0, Vec::new());

    harness
        .ledger
        .apply_block_at(block, NOW_MS.saturating_add(1))
        .unwrap();
}

#[test]
fn pending_third_party_burn_does_not_affect_block_validity() {
    let mut harness = harness_for_percent(21, 25);
    harness.mature_lineages(1, 4);
    let leader = harness.next_rank(0);
    let finalizer = harness.wallet(&leader.owner).clone();
    let anchor = harness.submit_anchor_burn(&finalizer);
    let victim = harness
        .honest
        .iter()
        .find(|wallet| wallet.address() != finalizer.address())
        .unwrap()
        .clone();
    let third_party = harness.submit_fee_burn(&victim, 1, 1);
    let mut block = harness.finish_ticket_block_from_pending(0, Vec::new());
    block
        .transactions
        .retain(|tx| tx.signature() == anchor.signature());
    rehash(&mut block);

    assert!(
        harness
            .ledger
            .pending()
            .iter()
            .any(|tx| tx.signature() == third_party.signature())
    );
    harness
        .ledger
        .apply_block_at(block, NOW_MS.saturating_add(1))
        .unwrap();
}

#[test]
fn included_third_party_burn_requires_committee_signatures() {
    let mut harness = harness_for_percent(21, 25);
    harness.mature_lineages(1, 4);
    let leader = harness.next_rank(0);
    let finalizer = harness.wallet(&leader.owner).clone();
    harness.submit_anchor_burn(&finalizer);
    let victim = harness
        .honest
        .iter()
        .find(|wallet| wallet.address() != finalizer.address())
        .unwrap()
        .clone();
    harness.submit_fee_burn(&victim, 1, 1);
    let block = harness.finish_ticket_block_from_pending(0, Vec::new());

    assert_rejects(
        harness.ledger,
        block,
        "included third-party burn without committee signatures",
    );
}

#[test]
fn zero_fee_public_burn_is_rejected() {
    let wallet = Wallet::from_seed("zero-fee-public-burn");
    let mut allocations = BTreeMap::new();
    allocations.insert(wallet.address().to_string(), 10 * MICRO_IUNA);
    let ledger = Ledger::new_with_genesis_burns(
        allocations,
        vec![GenesisBurn::new(wallet.address(), MICRO_IUNA)],
        1,
    )
    .unwrap();

    let error = ledger.build_burn(&wallet, 1, 0).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("burn transaction fee must be greater than zero")
    );
}

#[test]
fn post_genesis_transactions_cannot_spend_with_genesis_input_signatures() {
    let alice = Wallet::from_seed("post-genesis-signature-alice");
    let bob = Wallet::from_seed("post-genesis-signature-bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
    allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
    let mut ledger = Ledger::new_with_genesis_burns(
        allocations,
        vec![GenesisBurn::new(alice.address(), MICRO_IUNA)],
        1,
    )
    .unwrap();
    let mut transaction = ledger.build_transfer(&alice, bob.address(), 1, 1).unwrap();
    let Transaction::Transfer {
        inputs, signature, ..
    } = &mut transaction
    else {
        panic!("test builds a transfer");
    };
    for input in inputs {
        input.signature = "genesis".to_string();
    }
    *signature = "0".repeat(128);

    let error = ledger.submit_transaction(transaction).unwrap_err();

    assert!(
        error.to_string().contains("invalid input signature")
            || error
                .to_string()
                .contains("transaction signature is invalid")
    );
}

#[test]
fn invalid_committee_signature_is_rejected() {
    let mut harness = harness_for_percent(13, 25);
    harness.mature_lineages(1, 4);
    let member = harness
        .ledger
        .burn_committee_for_next_block()
        .into_iter()
        .find(|member| member.slot > 0)
        .expect("mature lineage should provide an additional committee member");
    let wallet = harness.wallet(&member.owner).clone();
    let burn = harness.submit_fee_burn(&harness.honest[0].clone(), 1, 1);
    let mut bundle = harness.ledger.build_burn_bundle(&wallet).unwrap().unwrap();
    assert!(
        bundle
            .burns
            .iter()
            .any(|tx| tx.signature() == burn.signature())
    );
    mutate_signature(&mut bundle.signature);

    assert!(
        harness
            .ledger
            .validate_next_block_burn_bundles(vec![bundle])
            .is_err()
    );
}

#[test]
fn non_mature_lineage_cannot_join_committee() {
    let mut harness = harness_for_percent(14, 25);
    let attacker = harness.attacker.lineage_wallets[0].clone();
    let mine = harness.ledger.build_mine(attacker.address()).unwrap();
    harness.ledger.submit_transaction(mine).unwrap();
    harness.mine_ticket_block(0);

    let committee = harness.ledger.burn_committee_for_next_block();
    assert!(
        committee
            .iter()
            .all(|member| member.owner != attacker.address()),
        "non-mature lineage appeared in committee: {committee:?}"
    );
}

#[test]
fn changing_attestation_set_after_vdf_changes_seed_and_invalidates_block() {
    let mut harness = harness_for_percent(15, 25);
    let leader = harness.next_rank(0);
    let finalizer = harness.wallet(&leader.owner).clone();
    harness.submit_anchor_burn(&finalizer);
    let victim = harness
        .honest
        .iter()
        .find(|wallet| wallet.address() != finalizer.address())
        .unwrap()
        .clone();
    let victim_burn = harness.submit_fee_burn(&victim, 1, 1);
    let mut block = harness.finish_ticket_block_from_pending(0, Vec::new());
    let original_seed = block.vdf_seed();
    block.burn_bundle_section.burns.push(MaskedBurn {
        burn: victim_burn,
        bundle_mask: burn_bundle_slot_mask(0).unwrap_or(0),
    });
    let changed_seed = block.vdf_seed();
    assert_ne!(original_seed, changed_seed);
    rehash(&mut block);

    assert_rejects(harness.ledger, block, "post-VDF attestation mutation");
}

#[test]
fn fallback_block_must_match_its_rank_and_ticket() {
    let mut harness = harness_for_percent(16, 25);
    let mut block = harness.prepared_ticket_block(1, Vec::new());
    block.finalizer_rank = 0;
    rehash(&mut block);

    assert_rejects(harness.ledger, block, "fallback block with wrong rank");
}

#[test]
fn consumed_ticket_cannot_be_reused() {
    let mut harness = harness_for_percent(17, 25);
    let block = harness.mine_ticket_block(0);
    let used_ticket = block.leader_proof.as_ref().unwrap().ticket_id.clone();
    let next_ranks = harness
        .ledger
        .burn_leader_ranks_for_block(harness.ledger.height() + 1)
        .unwrap();

    assert!(
        next_ranks.iter().all(|rank| rank.ticket_id != used_ticket),
        "consumed ticket {used_ticket} was eligible again"
    );
}

#[test]
fn recovery_cannot_bypass_ticket_rules_before_threshold_and_requires_own_burn() {
    let mut harness = harness_for_percent(18, 25);
    let attacker = harness.attacker.wallet.clone();
    harness.submit_anchor_burn(&attacker);
    assert!(
        harness
            .ledger
            .prepare_recovery_block(attacker.address(), VDF_TARGET_BLOCK_MS)
            .is_err()
    );
    harness.ledger.clear_pending_transactions();

    let mut recovery = harness.mine_recovery_block(&attacker);
    recovery
        .transactions
        .retain(|tx| !(tx.is_burn() && tx.sender() == attacker.address()));
    rehash(&mut recovery);
    let parent_snapshot = ChainSnapshot {
        genesis_allocations: harness.ledger.genesis_allocations.clone(),
        vdf_rounds: harness.ledger.initial_vdf_rounds,
        launch_profile: harness.ledger.launch_profile.clone(),
        blocks: harness.ledger.chain[..harness.ledger.chain.len() - 1].to_vec(),
    };
    let parent_ledger = Ledger::from_snapshot_at(parent_snapshot, NOW_MS).unwrap();

    assert_rejects(parent_ledger, recovery, "recovery without finalizer burn");
}

#[test]
fn recovery_and_fork_choice_cannot_cross_finality_depth() {
    let mut local = harness_for_percent(19, 25);
    for _ in 0..9 {
        local.mine_ticket_block(0);
    }

    let fork_base = ChainSnapshot {
        genesis_allocations: local.ledger.genesis_allocations.clone(),
        vdf_rounds: local.ledger.initial_vdf_rounds,
        launch_profile: local.ledger.launch_profile.clone(),
        blocks: local.ledger.chain[..2].to_vec(),
    };
    let mut remote = Harness {
        ledger: Ledger::from_snapshot_at(fork_base, NOW_MS).unwrap(),
        attacker: local.attacker.clone(),
        honest: local.honest.clone(),
        wallets: local.wallets.clone(),
        seed: local.seed,
        resource: local.resource,
    };
    remote.mine_ticket_block(1);
    for _ in 0..12 {
        remote.mine_ticket_block(0);
    }

    let switched = local
        .ledger
        .extend_from_snapshot_at(remote.ledger.snapshot(), NOW_MS)
        .unwrap();
    assert!(!switched, "fork deeper than finality depth was accepted");
}

#[test]
fn adversarial_scenarios_cover_resource_matrix() {
    let strategies = [
        AdversaryStrategy::MaximizeBurnWeight,
        AdversaryStrategy::MaximizeCommitteeWeight,
        AdversaryStrategy::CensorBurns,
        AdversaryStrategy::WithholdBurnFromCommittee,
        AdversaryStrategy::ForceFallback,
        AdversaryStrategy::AttemptRecovery,
        AdversaryStrategy::AddressRotation,
        AdversaryStrategy::CombinedStrategy,
    ];

    for (case, strategy) in strategies.into_iter().enumerate() {
        let (burn, lineage) = COMBINED_LEVELS[case % COMBINED_LEVELS.len()];
        let mut harness = Harness::new(100 + case as u64, burn, lineage, strategy);
        let blocks = if matches!(
            strategy,
            AdversaryStrategy::AttemptRecovery | AdversaryStrategy::CombinedStrategy
        ) {
            7
        } else {
            4
        };
        let metrics = harness.run_strategy(blocks);
        assert!(
            metrics.attacker_finalization_share.is_finite()
                && metrics.attacker_committee_share.is_finite(),
            "strategy={strategy:?} burn={burn}% lineage={lineage}% metrics={metrics:?}",
        );
        match strategy {
            AdversaryStrategy::MaximizeBurnWeight
            | AdversaryStrategy::MaximizeCommitteeWeight
            | AdversaryStrategy::AddressRotation => {
                assert_eq!(
                    metrics.third_party_burn_censorship_rate, 0.0,
                    "strategy={strategy:?} should not censor burns: {metrics:?}"
                );
                assert_eq!(
                    metrics.fallback_rate, 0.0,
                    "strategy={strategy:?} should not force fallbacks: {metrics:?}"
                );
                assert_eq!(
                    metrics.recovery_rate, 0.0,
                    "strategy={strategy:?} should not mine recovery blocks: {metrics:?}"
                );
            }
            AdversaryStrategy::CensorBurns => {
                assert_eq!(
                    metrics.third_party_burn_censorship_rate, 0.0,
                    "visible burns should be included by consensus-valid ticket blocks: {metrics:?}"
                );
            }
            AdversaryStrategy::WithholdBurnFromCommittee => {
                assert!(
                    metrics.third_party_burn_censorship_rate > 0.0,
                    "withheld burns should be absent from produced blocks: {metrics:?}"
                );
            }
            AdversaryStrategy::ForceFallback => {
                assert!(
                    metrics.fallback_rate > 0.0,
                    "force-fallback strategy did not produce fallback blocks: {metrics:?}"
                );
                assert_eq!(
                    metrics.recovery_rate, 0.0,
                    "force-fallback strategy should not mine recovery blocks: {metrics:?}"
                );
            }
            AdversaryStrategy::AttemptRecovery => {
                assert!(
                    metrics.recovery_rate > 0.0,
                    "recovery strategy did not produce recovery blocks: {metrics:?}"
                );
            }
            AdversaryStrategy::CombinedStrategy => {
                assert!(
                    metrics.third_party_burn_censorship_rate > 0.0,
                    "combined strategy did not withhold burns: {metrics:?}"
                );
                assert!(
                    metrics.fallback_rate > 0.0,
                    "combined strategy did not produce fallback blocks: {metrics:?}"
                );
                assert!(
                    metrics.recovery_rate > 0.0,
                    "combined strategy did not produce recovery blocks: {metrics:?}"
                );
            }
            AdversaryStrategy::Honest | AdversaryStrategy::MissRank0 => unreachable!(),
        }
    }

    for burn in LEVELS {
        for lineage in LEVELS {
            let mut harness = Harness::new(
                200 + u64::from(burn) * 10 + u64::from(lineage),
                burn,
                lineage,
                AdversaryStrategy::Honest,
            );
            let metrics = harness.run_strategy(3);
            assert!(
                metrics.attacker_finalization_share.is_finite()
                    && metrics.attacker_committee_share.is_finite(),
                "burn={burn}% lineage={lineage}% configured={:?} metrics={metrics:?}",
                (
                    harness.resource.burn_percent,
                    harness.resource.lineage_percent
                ),
            );
            assert_eq!(
                metrics.third_party_burn_censorship_rate, 0.0,
                "honest resource matrix run censored third-party burns: {metrics:?}"
            );
            assert_eq!(
                metrics.fallback_rate, 0.0,
                "honest resource matrix run produced fallback blocks: {metrics:?}"
            );
            assert_eq!(
                metrics.recovery_rate, 0.0,
                "honest resource matrix run produced recovery blocks: {metrics:?}"
            );
        }
    }
}
