use std::sync::atomic::AtomicBool;

use kyn_vdf::{
    Form, KynVdfError, create_discriminant, deserialize_form, isqrt_fourth, serialize_form,
    verify_wesolowski,
};
use num_traits::Signed;

use super::{VdfProgressPhase, prover};

pub(super) const DISCRIMINANT_BITS: usize = 1024;
const FORM_BYTES: usize = 100;
pub(super) const SOLUTION_BYTES: usize = FORM_BYTES * 2;

#[cfg(test)]
pub(super) fn prove(
    seed: &[u8],
    rounds: u64,
    progress: impl FnMut(VdfProgressPhase, u64),
) -> Result<Vec<u8>, KynVdfError> {
    let cancelled = AtomicBool::new(false);
    prove_cancellable(
        seed,
        rounds,
        progress,
        &cancelled,
        super::DEFAULT_VDF_MEMORY_MIB * 1024 * 1024,
    )?
    .ok_or_else(|| KynVdfError::ArithmeticError("non-cancellable VDF was cancelled".to_string()))
}

pub(super) fn prove_cancellable(
    seed: &[u8],
    rounds: u64,
    mut progress: impl FnMut(VdfProgressPhase, u64),
    cancelled: &AtomicBool,
    memory_budget_bytes: u64,
) -> Result<Option<Vec<u8>>, KynVdfError> {
    if rounds == 0 {
        return Err(KynVdfError::InvalidIterations(rounds));
    }

    let discriminant = create_discriminant(seed, DISCRIMINANT_BITS)?;
    let generator =
        Form::generator(&discriminant).ok_or(KynVdfError::InvalidDiscriminantIdentity)?;
    let threshold = isqrt_fourth(&discriminant.abs());

    let Some((output, proof)) = prover::prove_cancellable(
        &discriminant,
        &generator,
        &threshold,
        rounds,
        &mut progress,
        cancelled,
        memory_budget_bytes,
    )?
    else {
        return Ok(None);
    };
    serialize_solution(&output, &proof).map(Some)
}

pub(super) fn verify(seed: &[u8], rounds: u64, solution: &[u8]) -> bool {
    if rounds == 0 || solution.len() != SOLUTION_BYTES {
        return false;
    }

    let Some(discriminant) = create_discriminant(seed, DISCRIMINANT_BITS).ok() else {
        return false;
    };
    let Some(generator) = Form::generator(&discriminant) else {
        return false;
    };

    let (output_bytes, proof_bytes) = solution.split_at(FORM_BYTES);
    let Some(output) = canonical_form(&discriminant, output_bytes) else {
        return false;
    };
    let Some(proof) = canonical_form(&discriminant, proof_bytes) else {
        return false;
    };

    verify_wesolowski(&discriminant, &generator, &output, &proof, rounds).unwrap_or(false)
}

fn canonical_form(discriminant: &num_bigint::BigInt, bytes: &[u8]) -> Option<Form> {
    let form = deserialize_form(discriminant, bytes).ok()?;
    (serialize_form(&form, DISCRIMINANT_BITS).ok()?.as_slice() == bytes).then_some(form)
}

pub(super) fn serialize_solution(output: &Form, proof: &Form) -> Result<Vec<u8>, KynVdfError> {
    let mut solution = serialize_form(output, DISCRIMINANT_BITS)?;
    solution.extend_from_slice(&serialize_form(proof, DISCRIMINANT_BITS)?);
    debug_assert_eq!(solution.len(), SOLUTION_BYTES);
    Ok(solution)
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use std::process::Command;

    use super::{SOLUTION_BYTES, prove, verify};

    const CHIA_CHALLENGE_42_PROOF_HEX: &str = concat!(
        "0300032167dfd0eb393ed5d544e6499ba24def860ecd8a3600490f2f87b003c3e7855763969d34e2d1c60910297df3aead9f078a1f4d3973903f532977f9639f693cdbd331e8ba96bd61c895726dd157d67310ae98d1632c9bb9f28e0d7337403c0a0100",
        "04000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    );
    const CHIA_CHALLENGE_42_NONTRIVIAL_PROOF_HEX: &str = concat!(
        "0000235f6d0bfcbadbd5a0d6619a8611345eb63891876d37150fdef725695ab80c6deef7684c38fe0e086355baf4786fed8a5f843d0b7a62bf1125765b016dfe965b493cfc9bcde723c5299db8db25885d130f9aef4b029f98f42831aaf53e51e3350100",
        "0300d2b31e34c399ec49288e3fccb6ebaf0f3fb2e814c7c21e8579c17b5f2600b1a64d9d5b94435084b3458a9343fd1bcd3f0b9e5874556f1ab1529347b54788af1eb9268a5ee888fba85934c81b199a4228a41cb01c10b3195c95b26c17f16ff7020100",
    );

    #[test]
    fn prover_matches_chia_known_vector() {
        let solution = prove(&[0x42; 32], 100, |_, _| {}).unwrap();

        assert_eq!(
            solution,
            crate::domain::decode_hex(CHIA_CHALLENGE_42_PROOF_HEX).unwrap()
        );
        assert!(verify(&[0x42; 32], 100, &solution));
    }

    #[test]
    fn prover_matches_chia_nontrivial_proof_vector() {
        let solution = prove(&[0x42; 32], 300, |_, _| {}).unwrap();

        assert_eq!(
            solution,
            crate::domain::decode_hex(CHIA_CHALLENGE_42_NONTRIVIAL_PROOF_HEX).unwrap()
        );
        assert!(verify(&[0x42; 32], 300, &solution));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires the optional Python chiavdf package"]
    fn prover_matches_chiavdf_python_binding() {
        for (seed, rounds) in [([0x42; 32], 100), ([0x42; 32], 300)] {
            let expected = chiavdf_prove(&seed, rounds);
            let actual = prove(&seed, rounds, |_, _| {}).unwrap();

            assert_eq!(actual, expected, "rounds={rounds}");
            assert!(verify(&seed, rounds, &actual));
        }
    }

    #[test]
    fn proof_is_exactly_two_bqfc_forms() {
        let solution = prove(b"iuna-vdf-wire-format", 16, |_, _| {}).unwrap();

        assert_eq!(solution.len(), SOLUTION_BYTES);
    }

    #[test]
    fn malformed_and_mismatched_proofs_are_rejected() {
        let solution = prove(b"iuna-vdf-negative-test", 300, |_, _| {}).unwrap();
        let mut tampered = solution.clone();
        tampered[50] ^= 1;

        assert!(!verify(b"iuna-vdf-negative-test", 300, &tampered));
        assert!(!verify(b"iuna-vdf-negative-test", 301, &solution));
        assert!(!verify(b"other-seed", 300, &solution));
        assert!(!verify(b"iuna-vdf-negative-test", 300, &solution[..199]));
    }

    #[test]
    fn non_canonical_special_form_encodings_are_rejected() {
        let mut solution = prove(&[0x42; 32], 100, |_, _| {}).unwrap();
        assert_eq!(solution[100], 0x04, "the known proof is the identity");

        solution[199] = 1;

        assert!(!verify(&[0x42; 32], 100, &solution));
    }

    #[cfg(target_os = "macos")]
    fn chiavdf_prove(seed: &[u8; 32], rounds: u64) -> Vec<u8> {
        let python = std::env::var("IUNA_CHIAVDF_PYTHON").unwrap_or_else(|_| "python3".to_owned());
        let seed_hex = crate::domain::hex_encode(seed);
        let script = r#"
import sys
import tempfile

from chiavdf import prove

seed = bytes.fromhex(sys.argv[1])
rounds = int(sys.argv[2])
initial_el = b"\x08" + (b"\x00" * 99)

with tempfile.NamedTemporaryFile(prefix="iuna-chiavdf-shutdown-") as shutdown:
    solution = prove(seed, initial_el, 1024, rounds, shutdown.name)

sys.stdout.write(bytes(solution).hex())
"#;

        let output = Command::new(&python)
            .args(["-c", script, &seed_hex, &rounds.to_string()])
            .output()
            .unwrap_or_else(|error| panic!("failed to run {python}: {error}"));

        assert!(
            output.status.success(),
            "chiavdf subprocess failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8(output.stdout).expect("chiavdf stdout must be UTF-8 hex");
        crate::domain::decode_hex(stdout.trim()).expect("chiavdf stdout must be hex")
    }
}
