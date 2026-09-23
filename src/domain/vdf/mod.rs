use std::borrow::Cow;
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[cfg(feature = "e2e")]
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Block, FinalizerMode, MAX_VDF_ROUNDS, VDF_TARGET_BLOCK_MS, decode_hex, hex_encode};

#[cfg(test)]
mod arithmetic;
mod limb_arithmetic;
mod limbs;
mod prover;
#[cfg(test)]
mod reducer;
mod wesolowski;

#[cfg(test)]
mod reference;

const VDF_SOLUTION_PREFIX: &str = "classgroup-wesolowski-bqfc-v1:";
const MIN_VDF_ROUNDS: u64 = 1;
#[cfg(feature = "e2e")]
static E2E_VDF_ROUND_DIVISOR: AtomicU64 = AtomicU64::new(1);
pub(super) const VDF_RETARGET_WINDOW_BLOCKS: usize = 20;
pub(super) const MAX_VDF_RETARGET_STEP_PERCENT: u128 = 2;
pub(super) const VDF_RETARGET_DEADBAND_PERCENT: u128 = 10;
pub(super) const MIN_VDF_RETARGET_OBSERVED_BLOCK_MS: u64 = VDF_TARGET_BLOCK_MS / 4;
pub(super) const MAX_VDF_RETARGET_OBSERVED_BLOCK_MS: u64 = VDF_TARGET_BLOCK_MS * 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VdfProgress {
    pub completed_steps: u64,
    pub total_steps: u64,
    pub completed_phase_rounds: u64,
    pub phase_rounds: u64,
    pub phase: VdfProgressPhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VdfProgressPhase {
    Output,
    Proof,
}

/// Shortens newly produced and verified VDFs inside an isolated e2e test
/// process. Every participant in that process must use the same divisor.
#[cfg(feature = "e2e")]
pub fn configure_e2e_vdf_round_divisor_for_tests(divisor: u64) {
    E2E_VDF_ROUND_DIVISOR.store(divisor.max(1), Ordering::Relaxed);
}

pub fn run_vdf(seed: &str, rounds: u64) -> String {
    run_vdf_with_progress(seed, rounds, Duration::MAX, |_| {})
}

pub fn run_vdf_with_progress(
    seed: &str,
    rounds: u64,
    progress_interval: Duration,
    progress: impl FnMut(VdfProgress),
) -> String {
    let cancelled = AtomicBool::new(false);
    run_vdf_cancellable_with_progress(seed, rounds, progress_interval, &cancelled, progress)
        .expect("non-cancellable VDF must finish")
}

pub fn run_vdf_cancellable_with_progress(
    seed: &str,
    rounds: u64,
    progress_interval: Duration,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(VdfProgress),
) -> Option<String> {
    let (proof_seed, proof_rounds) = vdf_proof_parameters(seed, rounds);
    let total_steps = proof_rounds.saturating_mul(2);
    let mut last_progress = Instant::now();
    let solution = wesolowski::prove_cancellable(
        proof_seed.as_bytes(),
        proof_rounds,
        |phase, completed_phase_rounds| {
            let completed_steps = match phase {
                VdfProgressPhase::Output => completed_phase_rounds,
                VdfProgressPhase::Proof => proof_rounds.saturating_add(completed_phase_rounds),
            };
            maybe_report_vdf_progress(
                &mut last_progress,
                progress_interval,
                VdfProgress {
                    completed_steps,
                    total_steps,
                    completed_phase_rounds,
                    phase_rounds: proof_rounds,
                    phase,
                },
                &mut progress,
            );
        },
        cancelled,
    )
    .expect("valid IUNA VDF parameters must produce a class-group proof");
    solution.map(|solution| encode_vdf_solution(&solution))
}

pub fn verify_vdf(seed: &str, rounds: u64, solution: &str) -> bool {
    let Some(solution) = decode_vdf_solution(solution) else {
        return false;
    };
    let (proof_seed, proof_rounds) = vdf_proof_parameters(seed, rounds);
    wesolowski::verify(proof_seed.as_bytes(), proof_rounds, &solution)
}

fn vdf_proof_parameters(seed: &str, rounds: u64) -> (Cow<'_, str>, u64) {
    #[cfg(feature = "e2e")]
    {
        let divisor = E2E_VDF_ROUND_DIVISOR.load(Ordering::Relaxed);
        if divisor > 1 {
            return (
                Cow::Owned(format!("iuna-e2e-vdf:{rounds}:{seed}")),
                rounds.div_ceil(divisor).max(MIN_VDF_ROUNDS),
            );
        }
    }
    (Cow::Borrowed(seed), rounds)
}

pub(super) fn vdf_solution_placeholder() -> String {
    format!(
        "{VDF_SOLUTION_PREFIX}{}",
        "f".repeat(wesolowski::SOLUTION_BYTES * 2)
    )
}

pub(super) fn retarget_vdf_rounds(current_rounds: u64, observed_block_ms: u64) -> u64 {
    let current = u128::from(current_rounds);
    let observed = u128::from(observed_block_ms.max(1));
    let target = u128::from(VDF_TARGET_BLOCK_MS);
    let deadband = target * VDF_RETARGET_DEADBAND_PERCENT / 100;
    if observed >= target.saturating_sub(deadband) && observed <= target.saturating_add(deadband) {
        return current_rounds;
    }

    let raw_adjusted = current * target / observed;
    let max_step = (current * MAX_VDF_RETARGET_STEP_PERCENT / 100).max(1);
    let min_next = current
        .saturating_sub(max_step)
        .max(u128::from(MIN_VDF_ROUNDS));
    let max_next = current
        .saturating_add(max_step)
        .min(u128::from(MAX_VDF_ROUNDS));
    raw_adjusted.clamp(min_next, max_next) as u64
}

pub(super) fn clamped_vdf_retarget_observed_block_ms(observed_block_ms: u64) -> u64 {
    observed_block_ms.clamp(
        MIN_VDF_RETARGET_OBSERVED_BLOCK_MS,
        MAX_VDF_RETARGET_OBSERVED_BLOCK_MS,
    )
}

pub(super) fn vdf_retarget_observed_block_ms(parent: &Block, child: &Block) -> Option<u64> {
    if child.finalizer_mode != FinalizerMode::Ticket {
        return None;
    }
    if child.finalizer_rank != 0 {
        return None;
    }

    Some(clamped_vdf_retarget_observed_block_ms(
        child.timestamp_ms - parent.timestamp_ms,
    ))
}

pub(super) fn recent_vdf_retarget_average_observed_block_ms(chain: &[Block]) -> Option<u64> {
    let observations = chain
        .windows(2)
        .rev()
        .filter(|pair| pair[0].height > 0)
        .filter_map(|pair| vdf_retarget_observed_block_ms(&pair[0], &pair[1]))
        .take(VDF_RETARGET_WINDOW_BLOCKS)
        .collect::<Vec<_>>();
    if observations.is_empty() {
        return None;
    }
    let total = observations
        .iter()
        .map(|observed| u128::from(*observed))
        .sum::<u128>();
    Some((total / observations.len() as u128) as u64)
}

fn maybe_report_vdf_progress(
    last_progress: &mut Instant,
    progress_interval: Duration,
    snapshot: VdfProgress,
    progress: &mut impl FnMut(VdfProgress),
) {
    if snapshot.completed_steps == snapshot.total_steps
        || last_progress.elapsed() >= progress_interval
    {
        progress(snapshot);
        *last_progress = Instant::now();
    }
}

fn encode_vdf_solution(solution: &[u8]) -> String {
    format!("{VDF_SOLUTION_PREFIX}{}", hex_encode(solution))
}

fn decode_vdf_solution(solution: &str) -> Option<Vec<u8>> {
    let hex = solution.strip_prefix(VDF_SOLUTION_PREFIX)?;
    let solution = decode_hex(hex).ok()?;
    (solution.len() == wesolowski::SOLUTION_BYTES).then_some(solution)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };

    use super::{
        MAX_VDF_RETARGET_OBSERVED_BLOCK_MS, MAX_VDF_RETARGET_STEP_PERCENT,
        MIN_VDF_RETARGET_OBSERVED_BLOCK_MS, VDF_RETARGET_DEADBAND_PERCENT,
        VDF_RETARGET_WINDOW_BLOCKS, VDF_SOLUTION_PREFIX, VdfProgressPhase,
        clamped_vdf_retarget_observed_block_ms, recent_vdf_retarget_average_observed_block_ms,
        retarget_vdf_rounds, run_vdf, run_vdf_cancellable_with_progress, run_vdf_with_progress,
        vdf_retarget_observed_block_ms, vdf_solution_placeholder, verify_vdf, wesolowski,
    };
    use crate::domain::{Block, BurnBundleSection, FinalizerMode, VDF_TARGET_BLOCK_MS};

    fn block(height: u64, timestamp_ms: u64, mode: FinalizerMode, rank: u32) -> Block {
        Block {
            height,
            prev_hash: format!("{height:064x}"),
            timestamp_ms,
            miner: "finalizer".to_string(),
            reward_address: None,
            reward_address_signature: None,
            finalizer_mode: mode,
            finalizer_rank: rank,
            reward: 0,
            vdf_rounds: 100,
            vdf_output: format!("output-{height}"),
            leader_proof: None,
            burn_bundle_section: BurnBundleSection::default(),
            transactions: Vec::new(),
            transactions_v2: Vec::new(),
            hash: format!("{:064x}", height + 1),
        }
    }

    #[test]
    fn vdf_retarget_parameters_and_boundaries_match_the_protocol() {
        assert_eq!(VDF_RETARGET_WINDOW_BLOCKS, 20);
        assert_eq!(VDF_RETARGET_DEADBAND_PERCENT, 10);
        assert_eq!(MAX_VDF_RETARGET_STEP_PERCENT, 2);
        assert_eq!(MIN_VDF_RETARGET_OBSERVED_BLOCK_MS, VDF_TARGET_BLOCK_MS / 4);
        assert_eq!(MAX_VDF_RETARGET_OBSERVED_BLOCK_MS, VDF_TARGET_BLOCK_MS * 4);

        assert_eq!(
            retarget_vdf_rounds(1_000, VDF_TARGET_BLOCK_MS * 9 / 10),
            1_000
        );
        assert_eq!(
            retarget_vdf_rounds(1_000, VDF_TARGET_BLOCK_MS * 11 / 10),
            1_000
        );
        assert_eq!(retarget_vdf_rounds(1_000, VDF_TARGET_BLOCK_MS / 2), 1_020);
        assert_eq!(retarget_vdf_rounds(1_000, VDF_TARGET_BLOCK_MS * 2), 980);
        assert_eq!(
            clamped_vdf_retarget_observed_block_ms(1),
            VDF_TARGET_BLOCK_MS / 4
        );
        assert_eq!(
            clamped_vdf_retarget_observed_block_ms(u64::MAX),
            VDF_TARGET_BLOCK_MS * 4
        );
    }

    #[test]
    fn vdf_retarget_observes_only_rank_zero_ticket_blocks() {
        let parent = block(1, 1_000, FinalizerMode::Ticket, 0);
        let primary = block(2, 1_000 + VDF_TARGET_BLOCK_MS, FinalizerMode::Ticket, 0);
        let fallback = block(2, 1_000 + VDF_TARGET_BLOCK_MS, FinalizerMode::Ticket, 1);
        let recovery = block(2, 1_000 + VDF_TARGET_BLOCK_MS, FinalizerMode::Recovery, 0);

        assert_eq!(
            vdf_retarget_observed_block_ms(&parent, &primary),
            Some(VDF_TARGET_BLOCK_MS)
        );
        assert_eq!(vdf_retarget_observed_block_ms(&parent, &fallback), None);
        assert_eq!(vdf_retarget_observed_block_ms(&parent, &recovery), None);
    }

    #[test]
    fn fallback_blocks_do_not_consume_the_twenty_primary_observation_window() {
        let mut chain = vec![block(0, 0, FinalizerMode::Ticket, 0)];
        chain.push(block(1, VDF_TARGET_BLOCK_MS / 2, FinalizerMode::Ticket, 0));
        for height in 2..=21 {
            chain.push(block(
                height,
                height * (VDF_TARGET_BLOCK_MS / 2),
                FinalizerMode::Ticket,
                0,
            ));
        }
        for height in 22..=46 {
            chain.push(block(
                height,
                height * (VDF_TARGET_BLOCK_MS / 2),
                FinalizerMode::Ticket,
                1,
            ));
        }

        assert_eq!(
            recent_vdf_retarget_average_observed_block_ms(&chain),
            Some(VDF_TARGET_BLOCK_MS / 2)
        );
    }

    #[test]
    fn vdf_solution_verifies_and_is_bound_to_seed_and_rounds() {
        let solution = run_vdf("test-seed", 128);

        assert!(verify_vdf("test-seed", 128, &solution));
        assert!(!verify_vdf("other-seed", 128, &solution));
        assert!(!verify_vdf("test-seed", 129, &solution));
        assert!(!verify_vdf("test-seed", 128, "not-a-vdf-solution"));
    }

    #[test]
    fn vdf_progress_reports_output_and_proof_steps() {
        let mut progress = Vec::new();
        let solution = run_vdf_with_progress("progress-seed", 4, Duration::ZERO, |snapshot| {
            progress.push(snapshot);
        });

        assert!(verify_vdf("progress-seed", 4, &solution));
        assert!(
            progress
                .iter()
                .any(|snapshot| snapshot.phase == VdfProgressPhase::Output)
        );
        assert!(
            progress
                .iter()
                .any(|snapshot| snapshot.phase == VdfProgressPhase::Proof)
        );
        assert_eq!(
            progress.last().map(|snapshot| snapshot.completed_steps),
            Some(8)
        );
        assert_eq!(
            progress.last().map(|snapshot| snapshot.total_steps),
            Some(8)
        );
    }

    #[test]
    fn cancellable_vdf_stops_during_output_or_proof() {
        for phase in [VdfProgressPhase::Output, VdfProgressPhase::Proof] {
            let cancelled = AtomicBool::new(false);
            let solution = run_vdf_cancellable_with_progress(
                "cancelled-seed",
                128,
                Duration::ZERO,
                &cancelled,
                |progress| {
                    if progress.phase == phase && progress.completed_phase_rounds >= 10 {
                        cancelled.store(true, Ordering::Relaxed);
                    }
                },
            );

            assert!(solution.is_none(), "VDF did not stop during {phase:?}");
        }
    }

    #[test]
    fn vdf_solution_uses_chia_bqfc_protocol_format() {
        let solution = run_vdf("test-seed", 16);
        let encoded = solution.strip_prefix(VDF_SOLUTION_PREFIX).unwrap();

        assert_eq!(encoded.len(), wesolowski::SOLUTION_BYTES * 2);
        assert_eq!(solution.len(), vdf_solution_placeholder().len());
    }

    #[test]
    fn legacy_vdf_solution_formats_are_not_accepted() {
        let rsa = format!("{}:{}", "f".repeat(512), "f".repeat(512));
        let gmp_class_group = format!("classgroup-wesolowski-v1:{}", "f".repeat(520));

        assert!(!verify_vdf("test-seed", 16, &rsa));
        assert!(!verify_vdf("test-seed", 16, &gmp_class_group));
    }
}
