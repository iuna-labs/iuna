use kyn_vdf::{Form, KynVdfError, create_discriminant, get_b};
use num_bigint::BigUint;
use num_traits::One;

use super::wesolowski::{DISCRIMINANT_BITS, serialize_solution};

fn prove(seed: &[u8], rounds: u64) -> Result<Vec<u8>, KynVdfError> {
    if rounds == 0 {
        return Err(KynVdfError::InvalidIterations(rounds));
    }
    let shift = usize::try_from(rounds).map_err(|_| KynVdfError::InvalidIterations(rounds))?;
    let discriminant = create_discriminant(seed, DISCRIMINANT_BITS)?;
    let generator =
        Form::generator(&discriminant).ok_or(KynVdfError::InvalidDiscriminantIdentity)?;
    let exponent = BigUint::one() << shift;
    let output = generator.pow(&exponent, &discriminant);
    let challenge = get_b(&discriminant, &generator, &output)?;
    let quotient = &exponent / challenge;
    let proof = generator.pow(&quotient, &discriminant);
    serialize_solution(&output, &proof)
}

#[cfg(test)]
mod tests {
    use super::prove as reference_prove;
    use crate::domain::vdf::wesolowski::prove;

    #[test]
    fn sequential_prover_matches_independent_exponent_oracle() {
        for rounds in [1, 2, 16, 100, 300] {
            let sequential = prove(b"iuna-vdf-reference-oracle", rounds, |_, _| {}).unwrap();
            let reference = reference_prove(b"iuna-vdf-reference-oracle", rounds).unwrap();

            assert_eq!(sequential, reference, "rounds={rounds}");
        }
    }
}
