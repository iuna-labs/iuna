use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

use num_bigint::BigUint;
use num_traits::{One, Zero};
use sha2::{Digest, Sha256};

use super::{Block, FinalizerMode, MAX_VDF_ROUNDS, VDF_TARGET_BLOCK_MS};

const VDF_RSA_2048_MODULUS_DECIMAL: &str = concat!(
    "2519590847565789349402718324004839857142928212620403202777713783604366202070",
    "7595556264018525880784406918290641249515082189298559149176184502808489120072",
    "8449926873928072877767359714183472702618963750149718246911650776133798590957",
    "0009733045974880842840179742910064245869181719511874612151517265463228221686",
    "9987549182422433637259085141865462043576798423387184774447920739934236584823",
    "8242811981638150106748104516603773060562016196762561338441436038339044149526",
    "3443219011465754445417842402092461651572335077870774981712577246796292638635",
    "6373289912154831438167899885040445364023527381951378636564391212010397122822",
    "120720357",
);
const VDF_ELEMENT_HEX_LEN: usize = 512;
const VDF_CHALLENGE_MIN: u64 = 1_073_741_827;
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
    let x = vdf_seed_element(seed);
    let mut y = x.clone();
    let total_steps = rounds.saturating_mul(2);
    let mut last_progress = Instant::now();
    for completed_rounds in 0..rounds {
        y = square_mod(&y);
        maybe_report_vdf_progress(
            &mut last_progress,
            progress_interval,
            VdfProgress {
                completed_steps: completed_rounds + 1,
                total_steps,
                completed_phase_rounds: completed_rounds + 1,
                phase_rounds: rounds,
                phase: VdfProgressPhase::Output,
            },
            &mut progress,
        );
    }

    let challenge = vdf_challenge_prime(seed, rounds, &y);
    let proof = vdf_proof_with_progress(
        &x,
        rounds,
        challenge,
        total_steps,
        &mut last_progress,
        progress_interval,
        &mut progress,
    );
    encode_vdf_solution(y, proof)
}

pub fn verify_vdf(seed: &str, rounds: u64, solution: &str) -> bool {
    let Some((y, proof)) = decode_vdf_solution(solution) else {
        return false;
    };
    if y.is_zero() || y >= *vdf_modulus() || proof >= *vdf_modulus() {
        return false;
    }

    let x = vdf_seed_element(seed);
    let challenge = vdf_challenge_prime(seed, rounds, &y);
    let remainder = BigUint::from(pow_mod_small(2, rounds, challenge));
    let verified = mul_mod(
        &proof.modpow(&BigUint::from(challenge), vdf_modulus()),
        &x.modpow(&remainder, vdf_modulus()),
    );
    verified == y
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

fn vdf_modulus() -> &'static BigUint {
    static MODULUS: OnceLock<BigUint> = OnceLock::new();
    MODULUS.get_or_init(|| {
        BigUint::parse_bytes(VDF_RSA_2048_MODULUS_DECIMAL.as_bytes(), 10)
            .expect("VDF RSA-2048 modulus must parse")
    })
}

fn vdf_seed_element(seed: &str) -> BigUint {
    let one = BigUint::one();
    let two = BigUint::from(2_u32);
    for attempt in 0_u32.. {
        let candidate = hash_to_modulus("iuna-vdf-seed-v2", seed, attempt);
        if candidate <= one {
            continue;
        }
        let element = candidate.modpow(&two, vdf_modulus());
        if element > one {
            return element;
        }
    }
    unreachable!("VDF seed hashing must eventually produce a usable element")
}

fn hash_to_modulus(domain: &str, seed: &str, attempt: u32) -> BigUint {
    let byte_len = vdf_modulus().bits().div_ceil(8) as usize;
    let mut bytes = Vec::with_capacity(byte_len);
    let mut counter = 0_u32;
    while bytes.len() < byte_len {
        let digest = Sha256::digest(format!("{domain}:{seed}:{attempt}:{counter}").as_bytes());
        bytes.extend_from_slice(&digest);
        counter = counter.saturating_add(1);
    }
    bytes.truncate(byte_len);
    BigUint::from_bytes_be(&bytes) % vdf_modulus()
}

fn vdf_challenge_prime(seed: &str, rounds: u64, output: &BigUint) -> u64 {
    let digest = Sha256::digest(format!("iuna-vdf-challenge:{seed}:{rounds}:{output:x}"));
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    let candidate = VDF_CHALLENGE_MIN + (u64::from_be_bytes(bytes) % VDF_CHALLENGE_MIN);
    next_odd_prime(candidate | 1)
}

fn vdf_proof_with_progress(
    x: &BigUint,
    rounds: u64,
    challenge: u64,
    total_steps: u64,
    last_progress: &mut Instant,
    progress_interval: Duration,
    progress: &mut impl FnMut(VdfProgress),
) -> BigUint {
    let mut proof = BigUint::one();
    let mut remainder = 1_u64 % challenge;
    for completed_rounds in 0..rounds {
        let doubled = remainder * 2;
        let carry = doubled >= challenge;
        proof = square_mod(&proof);
        if carry {
            proof = mul_mod(&proof, x);
        }
        remainder = doubled % challenge;
        maybe_report_vdf_progress(
            last_progress,
            progress_interval,
            VdfProgress {
                completed_steps: rounds.saturating_add(completed_rounds + 1),
                total_steps,
                completed_phase_rounds: completed_rounds + 1,
                phase_rounds: rounds,
                phase: VdfProgressPhase::Proof,
            },
            progress,
        );
    }
    proof
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

fn encode_vdf_solution(output: BigUint, proof: BigUint) -> String {
    format!(
        "{output:0>width$x}:{proof:0>width$x}",
        width = VDF_ELEMENT_HEX_LEN
    )
}

fn decode_vdf_solution(solution: &str) -> Option<(BigUint, BigUint)> {
    let (output, proof) = solution.split_once(':')?;
    if output.len() != VDF_ELEMENT_HEX_LEN || proof.len() != VDF_ELEMENT_HEX_LEN {
        return None;
    }
    Some((
        BigUint::parse_bytes(output.as_bytes(), 16)?,
        BigUint::parse_bytes(proof.as_bytes(), 16)?,
    ))
}

fn square_mod(value: &BigUint) -> BigUint {
    mul_mod(value, value)
}

fn mul_mod(left: &BigUint, right: &BigUint) -> BigUint {
    (left * right) % vdf_modulus()
}

fn pow_mod_small(base: u64, exponent: u64, modulus: u64) -> u64 {
    let mut result = 1_u128;
    let mut base = u128::from(base % modulus);
    let mut exponent = exponent;
    let modulus = u128::from(modulus);
    while exponent > 0 {
        if exponent & 1 == 1 {
            result = (result * base) % modulus;
        }
        base = (base * base) % modulus;
        exponent >>= 1;
    }
    result as u64
}

fn next_odd_prime(mut candidate: u64) -> u64 {
    while !is_odd_prime(candidate) {
        candidate = candidate.saturating_add(2);
    }
    candidate
}

fn is_odd_prime(candidate: u64) -> bool {
    if candidate < 3 || candidate % 2 == 0 {
        return false;
    }
    let mut divisor = 3_u64;
    while divisor * divisor <= candidate {
        if candidate % divisor == 0 {
            return false;
        }
        divisor += 2;
    }
    true
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        VDF_ELEMENT_HEX_LEN, VdfProgressPhase, pow_mod_small, run_vdf, run_vdf_with_progress,
        vdf_modulus, verify_vdf,
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
    fn vdf_solution_uses_2048_bit_elements() {
        let solution = run_vdf("test-seed", 16);
        let (output, proof) = solution.split_once(':').unwrap();

        assert_eq!(output.len(), VDF_ELEMENT_HEX_LEN);
        assert_eq!(proof.len(), VDF_ELEMENT_HEX_LEN);
        assert!(vdf_modulus().bits() >= 2048);
    }

    #[test]
    fn legacy_factorable_modulus_attack_is_not_the_active_modulus() {
        const LEGACY_MODULUS: u128 = 4_611_685_975_477_714_963;
        const LEGACY_P: u128 = 2_147_483_629;
        const LEGACY_Q: u128 = 2_147_483_647;
        assert_eq!(LEGACY_P * LEGACY_Q, LEGACY_MODULUS);
        assert_ne!(vdf_modulus().to_str_radix(10), LEGACY_MODULUS.to_string());

        let phi = (LEGACY_P - 1) * (LEGACY_Q - 1);
        let seed = 42_u128;
        let rounds = 10_000_u64;
        let sequential = legacy_repeated_squaring(seed, rounds, LEGACY_MODULUS);
        let shortcut_exponent = pow_mod_small(2, rounds, phi as u64) as u128;
        let shortcut = legacy_mod_pow(seed, shortcut_exponent, LEGACY_MODULUS);

        assert_eq!(shortcut, sequential);
    }

    fn legacy_repeated_squaring(mut value: u128, rounds: u64, modulus: u128) -> u128 {
        for _ in 0..rounds {
            value = (value * value) % modulus;
        }
        value
    }

    fn legacy_mod_pow(mut base: u128, mut exponent: u128, modulus: u128) -> u128 {
        let mut result = 1_u128;
        while exponent > 0 {
            if exponent & 1 == 1 {
                result = (result * base) % modulus;
            }
            base = (base * base) % modulus;
            exponent >>= 1;
        }
        result
    }
}
