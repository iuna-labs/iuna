use kyn_vdf::{Form, KynVdfError, get_b};
use num_bigint::{BigInt, BigUint};
use num_traits::{One, ToPrimitive};

use super::{VdfProgressPhase, limb_arithmetic};

// Keep peak prover memory bounded. Larger parameter sets retain the old
// constant-memory algorithm instead of attempting an attacker-sized allocation.
const MAX_CHECKPOINTS: u64 = 262_144;
const MAX_BUCKETS: u64 = 65_536;
const INVALID_BUCKET: usize = usize::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProofParameters {
    k: u32,
    l: u64,
    checkpoint_count: u64,
    bucket_count: u64,
}

#[derive(Clone, Copy)]
struct ClassGroup<'a> {
    discriminant: &'a BigInt,
    threshold: &'a BigInt,
}

pub(super) fn prove(
    discriminant: &BigInt,
    generator: &Form,
    threshold: &BigInt,
    rounds: u64,
    progress: impl FnMut(VdfProgressPhase, u64),
) -> Result<(Form, Form), KynVdfError> {
    let group = ClassGroup {
        discriminant,
        threshold,
    };
    let parameters = ProofParameters::for_rounds(rounds);
    if parameters.checkpoint_count > MAX_CHECKPOINTS || parameters.bucket_count > MAX_BUCKETS {
        return prove_constant_memory(discriminant, generator, threshold, rounds, progress);
    }

    prove_checkpointed(group, generator, rounds, parameters, progress)
}

fn prove_checkpointed(
    group: ClassGroup<'_>,
    generator: &Form,
    rounds: u64,
    parameters: ProofParameters,
    mut progress: impl FnMut(VdfProgressPhase, u64),
) -> Result<(Form, Form), KynVdfError> {
    let checkpoint_capacity = usize::try_from(parameters.checkpoint_count)
        .map_err(|_| arithmetic_error("checkpoint count does not fit in memory"))?;
    let checkpoint_stride = u64::from(parameters.k)
        .checked_mul(parameters.l)
        .ok_or_else(|| arithmetic_error("checkpoint stride overflow"))?;
    let limb_discriminant = limb_arithmetic::to_limb(group.discriminant);
    let limb_threshold = limb_arithmetic::to_limb(group.threshold);
    let mut checkpoints = Vec::with_capacity(checkpoint_capacity);
    let mut output = limb_arithmetic::LimbForm::from_form(generator);
    let mut output_scratch = limb_arithmetic::LimbFormScratch::default();
    for completed_rounds in 1..=rounds {
        if (completed_rounds - 1) % checkpoint_stride == 0 {
            checkpoints.push(output.clone());
        }
        output = output.nudupl_reduce_with_scratch(
            &limb_discriminant,
            &limb_threshold,
            &mut output_scratch,
        );
        progress(VdfProgressPhase::Output, completed_rounds);
    }

    let output = output.into_form();
    debug_assert_eq!(checkpoints.len(), checkpoint_capacity);
    let proof = generate_checkpoint_proof(
        group,
        generator,
        &output,
        &checkpoints,
        rounds,
        parameters,
        &mut progress,
    )?;
    Ok((output, proof))
}

fn generate_checkpoint_proof(
    group: ClassGroup<'_>,
    generator: &Form,
    output: &Form,
    checkpoints: &[limb_arithmetic::LimbForm],
    rounds: u64,
    parameters: ProofParameters,
    progress: &mut impl FnMut(VdfProgressPhase, u64),
) -> Result<Form, KynVdfError> {
    let challenge = get_b(group.discriminant, generator, output)?;
    let bucket_count = usize::try_from(parameters.bucket_count)
        .map_err(|_| arithmetic_error("bucket count does not fit in memory"))?;
    let k0 = parameters.k - parameters.k / 2;
    let k1 = parameters.k / 2;
    let row_count = 1_u64 << k1;
    let column_count = 1_u64 << k0;
    let work_per_pass = parameters
        .checkpoint_count
        .checked_add(row_count)
        .and_then(|value| value.checked_add(column_count))
        .ok_or_else(|| arithmetic_error("proof progress calculation overflow"))?;
    let total_work = parameters
        .l
        .checked_mul(work_per_pass)
        .ok_or_else(|| arithmetic_error("proof progress calculation overflow"))?;
    let mut completed_work = 0_u64;
    let limb_discriminant = limb_arithmetic::to_limb(group.discriminant);
    let limb_threshold = limb_arithmetic::to_limb(group.threshold);
    let mut proof = limb_arithmetic::LimbForm::identity(&limb_discriminant);
    let mut proof_scratch = limb_arithmetic::LimbFormScratch::default();
    let block_step_exponent = u64::from(parameters.k)
        .checked_mul(parameters.l)
        .ok_or_else(|| arithmetic_error("proof block step overflow"))?;
    let block_step = BigUint::from(2_u8).modpow(&BigUint::from(block_step_exponent), &challenge);

    for j in (0..parameters.l).rev() {
        proof = proof.fast_pow_u64_with_scratch(
            1_u64 << parameters.k,
            &limb_discriminant,
            &limb_threshold,
            &mut proof_scratch,
        );

        let checkpoint_blocks =
            get_blocks_for_pass(j, parameters, rounds, &challenge, &block_step)?;
        let mut buckets: Vec<Option<limb_arithmetic::LimbForm>> = vec![None; bucket_count];
        for (i, checkpoint) in checkpoints.iter().enumerate() {
            let bucket = checkpoint_blocks[i];
            if bucket != INVALID_BUCKET {
                let bucket_form = buckets[bucket].take();
                buckets[bucket] = Some(match bucket_form {
                    Some(bucket_form) => bucket_form.compose_unreduced(
                        checkpoint,
                        &limb_discriminant,
                        &limb_threshold,
                        &mut proof_scratch,
                    ),
                    None => checkpoint.clone(),
                });
            }

            completed_work += 1;
            report_proof_progress(progress, rounds, completed_work, total_work);
        }

        for b1 in 0..row_count {
            let row_start = b1 << k0;
            let mut aggregate: Option<limb_arithmetic::LimbForm> = None;
            for b0 in 0..column_count {
                if let Some(bucket) = &buckets[(row_start + b0) as usize] {
                    aggregate = Some(match aggregate {
                        Some(aggregate) => aggregate.compose_unreduced(
                            bucket,
                            &limb_discriminant,
                            &limb_threshold,
                            &mut proof_scratch,
                        ),
                        None => bucket.clone(),
                    });
                }
            }
            if row_start != 0 {
                if let Some(aggregate) = aggregate {
                    let aggregate = aggregate.fast_pow_u64_with_scratch(
                        row_start,
                        &limb_discriminant,
                        &limb_threshold,
                        &mut proof_scratch,
                    );
                    proof = proof.compose_unreduced(
                        &aggregate,
                        &limb_discriminant,
                        &limb_threshold,
                        &mut proof_scratch,
                    );
                }
            }
            completed_work += 1;
            report_proof_progress(progress, rounds, completed_work, total_work);
        }

        for b0 in 0..column_count {
            let mut aggregate: Option<limb_arithmetic::LimbForm> = None;
            for b1 in 0..row_count {
                if let Some(bucket) = &buckets[((b1 << k0) + b0) as usize] {
                    aggregate = Some(match aggregate {
                        Some(aggregate) => aggregate.compose_unreduced(
                            bucket,
                            &limb_discriminant,
                            &limb_threshold,
                            &mut proof_scratch,
                        ),
                        None => bucket.clone(),
                    });
                }
            }
            if b0 != 0 {
                if let Some(aggregate) = aggregate {
                    let aggregate = aggregate.fast_pow_u64_with_scratch(
                        b0,
                        &limb_discriminant,
                        &limb_threshold,
                        &mut proof_scratch,
                    );
                    proof = proof.compose_unreduced(
                        &aggregate,
                        &limb_discriminant,
                        &limb_threshold,
                        &mut proof_scratch,
                    );
                }
            }
            completed_work += 1;
            report_proof_progress(progress, rounds, completed_work, total_work);
        }
    }

    proof.reduce();
    progress(VdfProgressPhase::Proof, rounds);
    Ok(proof.into_form())
}

fn get_blocks_for_pass(
    j: u64,
    parameters: ProofParameters,
    rounds: u64,
    challenge: &BigUint,
    step: &BigUint,
) -> Result<Vec<usize>, KynVdfError> {
    let checkpoint_count = usize::try_from(parameters.checkpoint_count)
        .map_err(|_| arithmetic_error("checkpoint count does not fit in memory"))?;
    let mut blocks = vec![INVALID_BUCKET; checkpoint_count];
    if checkpoint_count == 0 {
        return Ok(blocks);
    }

    let mut index = checkpoint_count - 1;
    loop {
        let position = checkpoint_position(index, j, parameters.l)?;
        if rounds >= block_end(position, parameters.k)? {
            break;
        }
        if index == 0 {
            return Ok(blocks);
        }
        index -= 1;
    }

    let position = checkpoint_position(index, j, parameters.l)?;
    let exponent = rounds
        .checked_sub(block_end(position, parameters.k)?)
        .ok_or_else(|| arithmetic_error("proof block exceeds iteration count"))?;
    let mut residue = BigUint::from(2_u8).modpow(&BigUint::from(exponent), challenge);
    loop {
        blocks[index] = block_from_residue(&residue, parameters.k, challenge)?;
        if index == 0 {
            break;
        }
        index -= 1;
        residue = (residue * step) % challenge;
    }

    Ok(blocks)
}

fn report_proof_progress(
    progress: &mut impl FnMut(VdfProgressPhase, u64),
    rounds: u64,
    completed: u64,
    total: u64,
) {
    let proof_rounds = completed.saturating_mul(rounds) / total.max(1);
    progress(VdfProgressPhase::Proof, proof_rounds);
}

fn checkpoint_position(index: usize, j: u64, l: u64) -> Result<u64, KynVdfError> {
    u64::try_from(index)
        .map_err(|_| arithmetic_error("checkpoint index does not fit in u64"))?
        .checked_mul(l)
        .and_then(|value| value.checked_add(j))
        .ok_or_else(|| arithmetic_error("checkpoint position overflow"))
}

fn block_end(position: u64, k: u32) -> Result<u64, KynVdfError> {
    u64::from(k)
        .checked_mul(
            position
                .checked_add(1)
                .ok_or_else(|| arithmetic_error("checkpoint position overflow"))?,
        )
        .ok_or_else(|| arithmetic_error("checkpoint block overflow"))
}

fn block_from_residue(
    residue: &BigUint,
    k: u32,
    challenge: &BigUint,
) -> Result<usize, KynVdfError> {
    let block = (residue << k) / challenge;
    block
        .to_usize()
        .filter(|value| *value < (1_usize << k))
        .ok_or_else(|| arithmetic_error("proof block does not fit in its bucket range"))
}

fn prove_constant_memory(
    discriminant: &BigInt,
    generator: &Form,
    threshold: &BigInt,
    rounds: u64,
    mut progress: impl FnMut(VdfProgressPhase, u64),
) -> Result<(Form, Form), KynVdfError> {
    let limb_discriminant = limb_arithmetic::to_limb(discriminant);
    let limb_threshold = limb_arithmetic::to_limb(threshold);
    let mut output = limb_arithmetic::LimbForm::from_form(generator);
    let mut output_scratch = limb_arithmetic::LimbFormScratch::default();
    for completed_rounds in 1..=rounds {
        output = output.nudupl_reduce_with_scratch(
            &limb_discriminant,
            &limb_threshold,
            &mut output_scratch,
        );
        progress(VdfProgressPhase::Output, completed_rounds);
    }
    let output = output.into_form();

    let challenge = get_b(discriminant, generator, &output)?;
    let generator_limb = limb_arithmetic::LimbForm::from_form(generator);
    let mut proof = limb_arithmetic::LimbForm::identity(&limb_discriminant);
    let mut proof_scratch = limb_arithmetic::LimbFormScratch::default();
    let mut remainder = BigUint::one() % &challenge;
    for completed_rounds in 1..=rounds {
        let doubled = &remainder << 1_usize;
        let carry = doubled >= challenge;
        proof = proof.nudupl_reduce_with_scratch(
            &limb_discriminant,
            &limb_threshold,
            &mut proof_scratch,
        );
        if carry {
            proof = proof.nucomp_reduce_with_scratch(
                &generator_limb,
                &limb_discriminant,
                &limb_threshold,
                &mut proof_scratch,
            );
        }
        remainder = doubled % &challenge;
        progress(VdfProgressPhase::Proof, completed_rounds);
    }

    Ok((output, proof.into_form()))
}

fn arithmetic_error(message: &str) -> KynVdfError {
    KynVdfError::ArithmeticError(message.to_owned())
}

impl ProofParameters {
    #[allow(clippy::approx_constant)] // Match Chia's published parameter heuristic.
    fn for_rounds(rounds: u64) -> Self {
        let log_memory = 23.253_496_66_f64;
        let log_rounds = (rounds as f64).log2();
        let l = if log_rounds - log_memory > 0.000_001 {
            2_f64.powf(log_memory - 20.0).ceil() as u64
        } else {
            1
        };
        let intermediate = rounds as f64 * 0.693_147_1 / (2.0 * l as f64);
        let mut k = if intermediate <= 1.0 {
            1
        } else {
            (intermediate.ln() - intermediate.ln().ln() + 0.25)
                .round()
                .max(1.0) as u32
        };
        if rounds >= 100_000 {
            k = k.max(10);
        }
        let checkpoint_stride = u64::from(k).saturating_mul(l).max(1);

        Self {
            k,
            l,
            checkpoint_count: rounds.div_ceil(checkpoint_stride),
            bucket_count: 1_u64.checked_shl(k).unwrap_or(u64::MAX),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use kyn_vdf::{Form, create_discriminant, isqrt_fourth};
    use num_traits::Signed;

    use super::{
        ClassGroup, ProofParameters, VdfProgressPhase, prove, prove_checkpointed,
        prove_constant_memory,
    };

    #[test]
    fn parameters_match_chia_reference_values() {
        for (rounds, k, l, checkpoints, buckets) in [
            (1, 1, 1, 1, 2),
            (100, 3, 1, 34, 8),
            (300, 3, 1, 100, 8),
            (100_000, 10, 1, 10_000, 1_024),
            (1_000_000, 10, 1, 100_000, 1_024),
            (1_500_000, 11, 1, 136_364, 2_048),
            (20_000_000, 11, 10, 181_819, 2_048),
        ] {
            assert_eq!(
                ProofParameters::for_rounds(rounds),
                ProofParameters {
                    k,
                    l,
                    checkpoint_count: checkpoints,
                    bucket_count: buckets,
                }
            );
        }
    }

    #[test]
    fn checkpoint_proof_matches_constant_memory_proof() {
        let discriminant = create_discriminant(b"iuna-vdf-checkpoint-differential", 1024).unwrap();
        let generator = Form::generator(&discriminant).unwrap();
        let threshold = isqrt_fourth(&discriminant.abs());

        for rounds in [1, 2, 16, 100, 300, 1_001] {
            let checkpoint =
                prove(&discriminant, &generator, &threshold, rounds, |_, _| {}).unwrap();
            let constant_memory =
                prove_constant_memory(&discriminant, &generator, &threshold, rounds, |_, _| {})
                    .unwrap();

            assert_eq!(checkpoint, constant_memory, "rounds={rounds}");
        }
    }

    #[test]
    fn multi_pass_checkpoint_proof_matches_constant_memory_and_reports_monotonic_progress() {
        let rounds = 301_u64;
        let discriminant = create_discriminant(b"iuna-vdf-checkpoint-multi-pass", 1024).unwrap();
        let generator = Form::generator(&discriminant).unwrap();
        let threshold = isqrt_fourth(&discriminant.abs());
        let group = ClassGroup {
            discriminant: &discriminant,
            threshold: &threshold,
        };
        let parameters = ProofParameters {
            k: 3,
            l: 2,
            checkpoint_count: rounds.div_ceil(6),
            bucket_count: 8,
        };
        let mut proof_progress = Vec::new();

        let checkpoint =
            prove_checkpointed(group, &generator, rounds, parameters, |phase, completed| {
                if phase == VdfProgressPhase::Proof {
                    proof_progress.push(completed);
                }
            })
            .unwrap();
        let constant_memory =
            prove_constant_memory(&discriminant, &generator, &threshold, rounds, |_, _| {})
                .unwrap();

        assert_eq!(checkpoint, constant_memory);
        assert!(proof_progress.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(proof_progress.last(), Some(&rounds));
    }

    #[test]
    #[ignore = "manual VDF prover benchmark"]
    fn benchmark_checkpoint_prover_against_constant_memory() {
        let rounds = 100_000_u64;
        let discriminant = create_discriminant(b"iuna-vdf-prover-benchmark", 1024).unwrap();
        let generator = Form::generator(&discriminant).unwrap();
        let threshold = isqrt_fourth(&discriminant.abs());

        let started = Instant::now();
        let checkpoint = prove(&discriminant, &generator, &threshold, rounds, |_, _| {}).unwrap();
        let checkpoint_elapsed = started.elapsed();
        let started = Instant::now();
        let constant_memory =
            prove_constant_memory(&discriminant, &generator, &threshold, rounds, |_, _| {})
                .unwrap();
        let constant_memory_elapsed = started.elapsed();

        assert_eq!(checkpoint, constant_memory);
        eprintln!(
            "rounds={rounds} checkpoint={checkpoint_elapsed:?} constant_memory={constant_memory_elapsed:?} speedup={:.2}x",
            constant_memory_elapsed.as_secs_f64() / checkpoint_elapsed.as_secs_f64()
        );
    }

    #[test]
    #[ignore = "manual VDF phase benchmark"]
    fn benchmark_checkpoint_prover_phases() {
        let rounds = 100_000_u64;
        let discriminant = create_discriminant(b"iuna-vdf-prover-benchmark", 1024).unwrap();
        let generator = Form::generator(&discriminant).unwrap();
        let threshold = isqrt_fourth(&discriminant.abs());
        let group = ClassGroup {
            discriminant: &discriminant,
            threshold: &threshold,
        };
        let parameters = ProofParameters::for_rounds(rounds);
        let checkpoint_stride = u64::from(parameters.k) * parameters.l;
        let mut checkpoints = Vec::with_capacity(parameters.checkpoint_count as usize);
        let limb_discriminant = crate::domain::vdf::limb_arithmetic::to_limb(&discriminant);
        let limb_threshold = crate::domain::vdf::limb_arithmetic::to_limb(&threshold);
        let mut output = crate::domain::vdf::limb_arithmetic::LimbForm::from_form(&generator);
        let mut output_scratch = crate::domain::vdf::limb_arithmetic::LimbFormScratch::default();

        let started = Instant::now();
        for completed_rounds in 1..=rounds {
            if (completed_rounds - 1) % checkpoint_stride == 0 {
                checkpoints.push(output.clone());
            }
            output = output.nudupl_reduce_with_scratch(
                &limb_discriminant,
                &limb_threshold,
                &mut output_scratch,
            );
        }
        let output = output.into_form();
        let output_elapsed = started.elapsed();

        let started = Instant::now();
        let proof = super::generate_checkpoint_proof(
            group,
            &generator,
            &output,
            &checkpoints,
            rounds,
            parameters,
            &mut |_, _| {},
        )
        .unwrap();
        let proof_elapsed = started.elapsed();

        assert_eq!(
            super::prove(&discriminant, &generator, &threshold, rounds, |_, _| {}).unwrap(),
            (output, proof)
        );
        eprintln!(
            "rounds={rounds} output={output_elapsed:?} proof={proof_elapsed:?} total={:?}",
            output_elapsed + proof_elapsed
        );
    }
    #[test]
    #[ignore = "manual VDF square phase benchmark"]
    fn benchmark_output_square_phases() {
        let rounds = 100_000;
        let discriminant = create_discriminant(b"iuna-vdf-prover-benchmark", 1024).unwrap();
        let threshold = isqrt_fourth(&discriminant.abs());
        let mut output = Form::generator(&discriminant).unwrap();
        let mut nudupl_elapsed = std::time::Duration::ZERO;
        let mut reduce_elapsed = std::time::Duration::ZERO;

        for _ in 0..rounds {
            let started = Instant::now();
            output =
                crate::domain::vdf::arithmetic::nudupl_owned(output, &discriminant, &threshold);
            nudupl_elapsed += started.elapsed();

            let started = Instant::now();
            crate::domain::vdf::reducer::reduce(&mut output);
            reduce_elapsed += started.elapsed();
        }

        assert!(output.is_reduced());
        eprintln!(
            "rounds={rounds} nudupl={nudupl_elapsed:?} reduce={reduce_elapsed:?} total={:?}",
            nudupl_elapsed + reduce_elapsed
        );
    }
}
