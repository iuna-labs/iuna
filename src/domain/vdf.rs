use sha2::{Digest, Sha256};

use super::{
    Block, FALLBACK_VDF_RETARGET_ACTIVATION_HEIGHT, FALLBACK_VDF_RETARGET_DEACTIVATION_HEIGHT,
    FinalizerMode, MAX_VDF_ROUNDS, VDF_TARGET_BLOCK_MS,
};

const VDF_MODULUS: u128 = 4_611_685_975_477_714_963;
const VDF_CHALLENGE_MIN: u64 = 1_073_741_827;
const MIN_VDF_ROUNDS: u64 = 1;
pub(super) const VDF_RETARGET_WINDOW_BLOCKS: usize = 20;
pub(super) const MAX_VDF_RETARGET_STEP_PERCENT: u128 = 2;
pub(super) const VDF_RETARGET_DEADBAND_PERCENT: u128 = 10;
pub(super) const MIN_VDF_RETARGET_OBSERVED_BLOCK_MS: u64 = VDF_TARGET_BLOCK_MS / 4;
pub(super) const MAX_VDF_RETARGET_OBSERVED_BLOCK_MS: u64 = VDF_TARGET_BLOCK_MS * 4;

pub fn run_vdf(seed: &str, rounds: u64) -> String {
    let x = vdf_seed_element(seed);
    let mut y = x;
    for _ in 0..rounds {
        y = mul_mod(y, y);
    }

    let challenge = vdf_challenge_prime(seed, rounds, y);
    let proof = vdf_proof(x, rounds, challenge);
    encode_vdf_solution(y, proof)
}

pub fn verify_vdf(seed: &str, rounds: u64, solution: &str) -> bool {
    let Some((y, proof)) = decode_vdf_solution(solution) else {
        return false;
    };
    if y == 0 || y >= VDF_MODULUS || proof >= VDF_MODULUS {
        return false;
    }

    let x = vdf_seed_element(seed);
    let challenge = vdf_challenge_prime(seed, rounds, y);
    let remainder = pow_mod_small(2, rounds, challenge) as u128;
    let verified = mul_mod(mod_pow(proof, challenge as u128), mod_pow(x, remainder));
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
    if child.finalizer_rank != 0
        && (child.height < FALLBACK_VDF_RETARGET_ACTIVATION_HEIGHT
            || child.height >= FALLBACK_VDF_RETARGET_DEACTIVATION_HEIGHT)
    {
        return None;
    }

    Some(clamped_vdf_retarget_observed_block_ms(
        child.timestamp_ms - parent.timestamp_ms,
    ))
}

fn vdf_seed_element(seed: &str) -> u128 {
    let digest = Sha256::digest(format!("iuna-vdf-seed:{seed}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    2 + (u128::from_be_bytes(bytes) % (VDF_MODULUS - 3))
}

fn vdf_challenge_prime(seed: &str, rounds: u64, output: u128) -> u64 {
    let digest = Sha256::digest(format!("iuna-vdf-challenge:{seed}:{rounds}:{output:x}"));
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    let candidate = VDF_CHALLENGE_MIN + (u64::from_be_bytes(bytes) % VDF_CHALLENGE_MIN);
    next_odd_prime(candidate | 1)
}

fn vdf_proof(x: u128, rounds: u64, challenge: u64) -> u128 {
    let mut proof = 1_u128;
    let mut remainder = 1_u64 % challenge;
    for _ in 0..rounds {
        let doubled = remainder * 2;
        let carry = doubled >= challenge;
        proof = mul_mod(proof, proof);
        if carry {
            proof = mul_mod(proof, x);
        }
        remainder = doubled % challenge;
    }
    proof
}

fn encode_vdf_solution(output: u128, proof: u128) -> String {
    format!("{output:032x}:{proof:032x}")
}

fn decode_vdf_solution(solution: &str) -> Option<(u128, u128)> {
    let (output, proof) = solution.split_once(':')?;
    if output.len() != 32 || proof.len() != 32 {
        return None;
    }
    Some((
        u128::from_str_radix(output, 16).ok()?,
        u128::from_str_radix(proof, 16).ok()?,
    ))
}

fn mul_mod(left: u128, right: u128) -> u128 {
    (left * right) % VDF_MODULUS
}

fn mod_pow(mut base: u128, mut exponent: u128) -> u128 {
    let mut result = 1_u128;
    while exponent > 0 {
        if exponent & 1 == 1 {
            result = mul_mod(result, base);
        }
        base = mul_mod(base, base);
        exponent >>= 1;
    }
    result
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
    use super::{run_vdf, verify_vdf};

    #[test]
    fn vdf_solution_verifies_and_is_bound_to_seed_and_rounds() {
        let solution = run_vdf("test-seed", 128);

        assert!(verify_vdf("test-seed", 128, &solution));
        assert!(!verify_vdf("other-seed", 128, &solution));
        assert!(!verify_vdf("test-seed", 129, &solution));
        assert!(!verify_vdf("test-seed", 128, "not-a-vdf-solution"));
    }
}
