use std::time::{Duration, Instant};

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

pub fn run_vdf(seed: &str, rounds: u64) -> String {
    run_vdf_with_progress(seed, rounds, Duration::MAX, |_| {})
}

pub fn run_vdf_with_progress(
    seed: &str,
    rounds: u64,
    progress_interval: Duration,
    mut progress: impl FnMut(VdfProgress),
) -> String {
    let total_steps = rounds.saturating_mul(2);
    let mut last_progress = Instant::now();
    let solution = wesolowski::prove(seed.as_bytes(), rounds, |phase, completed_phase_rounds| {
        let completed_steps = match phase {
            VdfProgressPhase::Output => completed_phase_rounds,
            VdfProgressPhase::Proof => rounds.saturating_add(completed_phase_rounds),
        };
        maybe_report_vdf_progress(
            &mut last_progress,
            progress_interval,
            VdfProgress {
                completed_steps,
                total_steps,
                completed_phase_rounds,
                phase_rounds: rounds,
                phase,
            },
            &mut progress,
        );
    })
    .expect("valid IUNA VDF parameters must produce a class-group proof");
    encode_vdf_solution(&solution)
}

pub fn verify_vdf(seed: &str, rounds: u64, solution: &str) -> bool {
    let Some(solution) = decode_vdf_solution(solution) else {
        return false;
    };
    wesolowski::verify(seed.as_bytes(), rounds, &solution)
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
    use std::time::Duration;

    use super::{
        VDF_SOLUTION_PREFIX, VdfProgressPhase, run_vdf, run_vdf_with_progress,
        vdf_solution_placeholder, verify_vdf, wesolowski,
    };

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
