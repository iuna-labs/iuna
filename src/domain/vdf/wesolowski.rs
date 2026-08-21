use kyn_vdf::{
    Form, KynVdfError, create_discriminant, deserialize_form, isqrt_fourth, serialize_form,
    verify_wesolowski,
};
use num_traits::Signed;

use super::{VdfProgressPhase, prover};

pub(super) const DISCRIMINANT_BITS: usize = 1024;
const FORM_BYTES: usize = 100;
pub(super) const SOLUTION_BYTES: usize = FORM_BYTES * 2;

pub(super) fn prove(
    seed: &[u8],
    rounds: u64,
    mut progress: impl FnMut(VdfProgressPhase, u64),
) -> Result<Vec<u8>, KynVdfError> {
    if rounds == 0 {
        return Err(KynVdfError::InvalidIterations(rounds));
    }

    let discriminant = create_discriminant(seed, DISCRIMINANT_BITS)?;
    let generator =
        Form::generator(&discriminant).ok_or(KynVdfError::InvalidDiscriminantIdentity)?;
    let threshold = isqrt_fourth(&discriminant.abs());

    let (output, proof) =
        prover::prove(&discriminant, &generator, &threshold, rounds, &mut progress)?;
    serialize_solution(&output, &proof)
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
}
