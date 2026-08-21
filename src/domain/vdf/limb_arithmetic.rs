use std::cmp::Ordering;

use kyn_vdf::Form;
use num_bigint::BigInt;

use super::limbs::{LimbInt, LimbScratch, xgcd_partial_with_scratch};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LimbForm {
    a: LimbInt,
    b: LimbInt,
    c: LimbInt,
}

#[derive(Default)]
pub(super) struct LimbFormScratch {
    limbs: LimbScratch,
    k_product: LimbInt,
    work0: LimbInt,
    work1: LimbInt,
    modulus: LimbInt,
}

impl LimbForm {
    pub(super) fn identity(discriminant: &LimbInt) -> Self {
        let one = LimbInt::one();
        let four = LimbInt::from_u64(4);
        Self {
            a: one.clone(),
            b: one.clone(),
            c: one.sub(discriminant).div(&four),
        }
    }

    pub(super) fn from_form(form: &Form) -> Self {
        Self {
            a: to_limb(&form.a),
            b: to_limb(&form.b),
            c: to_limb(&form.c),
        }
    }

    pub(super) fn into_form(self) -> Form {
        Form::new(from_limb(&self.a), from_limb(&self.b), from_limb(&self.c))
    }

    #[cfg(test)]
    pub(super) fn nudupl_reduce(self, discriminant: &LimbInt, threshold: &LimbInt) -> Self {
        let mut scratch = LimbFormScratch::default();
        self.nudupl_reduce_with_scratch(discriminant, threshold, &mut scratch)
    }

    pub(super) fn nudupl_reduce_with_scratch(
        self,
        discriminant: &LimbInt,
        threshold: &LimbInt,
        scratch: &mut LimbFormScratch,
    ) -> Self {
        if self.is_identity() {
            return self;
        }
        let mut form = nudupl_owned(self, discriminant, threshold, scratch);
        reduce_with_scratch(&mut form, scratch);
        form
    }

    #[cfg(test)]
    pub(super) fn nucomp_reduce(
        &self,
        other: &Self,
        discriminant: &LimbInt,
        threshold: &LimbInt,
    ) -> Self {
        let mut scratch = LimbFormScratch::default();
        self.nucomp_reduce_with_scratch(other, discriminant, threshold, &mut scratch)
    }

    pub(super) fn nucomp_reduce_with_scratch(
        &self,
        other: &Self,
        discriminant: &LimbInt,
        threshold: &LimbInt,
        scratch: &mut LimbFormScratch,
    ) -> Self {
        if self.is_identity() {
            return other.clone();
        }
        if other.is_identity() {
            return self.clone();
        }
        let mut form = nucomp(self, other, discriminant, threshold, scratch);
        reduce_with_scratch(&mut form, scratch);
        form
    }

    pub(super) fn compose_unreduced(
        &self,
        other: &Self,
        discriminant: &LimbInt,
        threshold: &LimbInt,
        scratch: &mut LimbFormScratch,
    ) -> Self {
        if self.is_identity() {
            return other.clone();
        }
        if other.is_identity() {
            return self.clone();
        }
        nucomp(self, other, discriminant, threshold, scratch)
    }

    pub(super) fn fast_pow_u64_with_scratch(
        &self,
        exponent: u64,
        discriminant: &LimbInt,
        threshold: &LimbInt,
        scratch: &mut LimbFormScratch,
    ) -> Self {
        if exponent == 0 {
            return Self::identity(discriminant);
        }
        if self.is_identity() {
            return self.clone();
        }

        let mut result = self.clone();
        let max_bits = discriminant.bit_len() / 2;
        let num_bits = u64::BITS - exponent.leading_zeros();

        for bit in (0..num_bits.saturating_sub(1)).rev() {
            result = nudupl_owned(result, discriminant, threshold, scratch);
            if result.a.bit_len() > max_bits {
                reduce_with_scratch(&mut result, scratch);
            }

            if ((exponent >> bit) & 1) == 1 {
                result = nucomp(&result, self, discriminant, threshold, scratch);
            }
        }

        reduce_with_scratch(&mut result, scratch);
        result
    }

    pub(super) fn reduce(&mut self) {
        let mut scratch = LimbFormScratch::default();
        reduce_with_scratch(self, &mut scratch);
    }

    fn is_identity(&self) -> bool {
        self.a.is_one() && self.b.is_one()
    }
}

pub(super) fn to_limb(value: &BigInt) -> LimbInt {
    LimbInt::from_bigint(value)
}

pub(super) fn from_limb(value: &LimbInt) -> BigInt {
    value.to_bigint()
}

fn nudupl_owned(
    form: LimbForm,
    discriminant: &LimbInt,
    threshold: &LimbInt,
    scratch: &mut LimbFormScratch,
) -> LimbForm {
    let LimbForm { a, b, c } = form;
    let mut a1 = a;
    let mut c1 = c;

    let gcd = if b.is_negative() {
        let b_abs = b.clone().negated();
        let gcd = b_abs.left_extended_gcd_positive_with_scratch(&a1, &mut scratch.limbs);
        (gcd.x.negated(), gcd.gcd)
    } else {
        let gcd = b.left_extended_gcd_positive_with_scratch(&a1, &mut scratch.limbs);
        (gcd.x, gcd.gcd)
    };

    gcd.0.mul_into(&c1, &mut scratch.k_product);
    scratch.k_product.negate_assign();
    let s = gcd.1;
    if !s.is_one() {
        a1 = a1.div_with_scratch(&s, &mut scratch.limbs);
        c1 = c1.mul(&s);
    }
    let k = scratch
        .k_product
        .mod_positive_with_scratch(&a1, &mut scratch.limbs);

    if a1.cmp(threshold) == Ordering::Less {
        let t = a1.mul(&k);
        let result_a = a1.square();
        let result_b = t.shl_bits(1).add_owned(&b);
        let result_c = b
            .add(&t)
            .mul(&k)
            .add_owned(&c1)
            .div_with_scratch(&a1, &mut scratch.limbs);
        LimbForm {
            a: result_a,
            b: result_b,
            c: result_c,
        }
    } else {
        let (co2, co1, _r2, r1) = xgcd_partial_with_scratch(&a1, &k, threshold, &mut scratch.limbs);
        b.mul_into(&r1, &mut scratch.work0);
        c1.mul_into(&co1, &mut scratch.work1);
        scratch.work0.sub_assign(&scratch.work1);
        let m2 = scratch.work0.div_with_scratch(&a1, &mut scratch.limbs);

        r1.square_into(&mut scratch.work0);
        co1.mul_into(&m2, &mut scratch.work1);
        scratch.work0.sub_assign(&scratch.work1);
        let mut result_a = std::mem::take(&mut scratch.work0);
        if !co1.is_negative() {
            result_a = result_a.negated();
        }

        a1.mul_into(&r1, &mut scratch.work0);
        result_a.mul_into(&co2, &mut scratch.work1);
        scratch.work0.sub_assign(&scratch.work1);
        scratch.work0.shl_bits_assign(1);
        let mut result_b = scratch.work0.div_with_scratch(&co1, &mut scratch.limbs);
        result_b.sub_assign(&b);
        result_a.shl_bits_into(1, &mut scratch.modulus);
        let result_b = result_b.mod_positive_with_scratch(&scratch.modulus, &mut scratch.limbs);

        result_b.square_into(&mut scratch.work0);
        scratch.work0.sub_assign(discriminant);
        result_a.shl_bits_into(2, &mut scratch.modulus);
        let mut result_c = scratch
            .work0
            .div_with_scratch(&scratch.modulus, &mut scratch.limbs);

        if result_a.is_negative() {
            result_a = result_a.negated();
            result_c = result_c.negated();
        }

        LimbForm {
            a: result_a,
            b: result_b,
            c: result_c,
        }
    }
}

fn nucomp(
    left: &LimbForm,
    right: &LimbForm,
    discriminant: &LimbInt,
    threshold: &LimbInt,
    scratch: &mut LimbFormScratch,
) -> LimbForm {
    if left.a.cmp(&right.a) == Ordering::Greater {
        return nucomp(right, left, discriminant, threshold, scratch);
    }

    let mut a1 = left.a.clone();
    let mut a2 = right.a.clone();
    let mut c2 = right.c.clone();
    let ss = left.b.add(&right.b).div2_exact();
    let m = left.b.sub(&right.b).div2_exact();

    let t = a2.rem_with_scratch(&a1, &mut scratch.limbs);
    let (v1, sp) = if t.is_zero() {
        (LimbInt::zero(), a1.clone())
    } else {
        let gcd = t.left_extended_gcd_positive_with_scratch(&a1, &mut scratch.limbs);
        (gcd.x, gcd.gcd)
    };
    let mut k = m
        .mul(&v1)
        .mod_positive_with_scratch(&a1, &mut scratch.limbs);

    if !sp.is_one() {
        let gcd = ss.extended_gcd_with_scratch(&sp, &mut scratch.limbs);
        let v2 = gcd.x;
        let u2 = gcd.y;
        let s = gcd.gcd;
        k = k.mul(&u2).sub_owned(&v2.mul(&c2));
        if !s.is_one() {
            a1 = a1.div_with_scratch(&s, &mut scratch.limbs);
            a2 = a2.div_with_scratch(&s, &mut scratch.limbs);
            c2 = c2.mul(&s);
        }
        k = k.mod_positive_with_scratch(&a1, &mut scratch.limbs);
    }

    if a1.cmp(threshold) == Ordering::Less {
        let t = a2.mul(&k);
        let result_a = a2.mul(&a1);
        let result_b = t.shl_bits(1).add_owned(&right.b);
        let result_c = right
            .b
            .add(&t)
            .mul(&k)
            .add_owned(&c2)
            .div_with_scratch(&a1, &mut scratch.limbs);
        LimbForm {
            a: result_a,
            b: result_b,
            c: result_c,
        }
    } else {
        let (co2, co1, _r2, r1) = xgcd_partial_with_scratch(&a1, &k, threshold, &mut scratch.limbs);
        let m1 = m
            .mul(&co1)
            .add_owned(&a2.mul(&r1))
            .div_with_scratch(&a1, &mut scratch.limbs);
        let m2 = ss
            .mul(&r1)
            .sub_owned(&c2.mul(&co1))
            .div_with_scratch(&a1, &mut scratch.limbs);

        let mut result_a = r1.mul(&m1).sub_owned(&co1.mul(&m2));
        if !co1.is_negative() {
            result_a = result_a.negated();
        }

        let t = a2.mul(&r1);
        let result_b = t
            .sub_owned(&result_a.mul(&co2))
            .shl_bits_owned(1)
            .div_with_scratch(&co1, &mut scratch.limbs)
            .sub_owned(&right.b)
            .mod_positive_with_scratch(&result_a.shl_bits(1), &mut scratch.limbs);
        let mut result_c = result_b
            .square()
            .sub_owned(discriminant)
            .div_with_scratch(&result_a.shl_bits(2), &mut scratch.limbs);

        if result_a.is_negative() {
            result_a = result_a.negated();
            result_c = result_c.negated();
        }

        LimbForm {
            a: result_a,
            b: result_b,
            c: result_c,
        }
    }
}

#[inline(always)]
fn reduce_with_scratch(form: &mut LimbForm, scratch: &mut LimbFormScratch) {
    while !finish_if_reduced(form) {
        reduce_once(form, scratch);
    }
}

#[inline(always)]
fn finish_if_reduced(form: &mut LimbForm) -> bool {
    if form.a.abs_cmp(&form.b) == Ordering::Less || form.c.abs_cmp(&form.b) == Ordering::Less {
        return false;
    }

    match form.a.cmp(&form.c) {
        Ordering::Greater => {
            std::mem::swap(&mut form.a, &mut form.c);
            form.b = std::mem::take(&mut form.b).negated();
        }
        Ordering::Equal if form.b.is_negative() => {
            form.b = std::mem::take(&mut form.b).negated();
        }
        _ => {}
    }
    true
}

#[inline(always)]
fn reduce_once(form: &mut LimbForm, scratch: &mut LimbFormScratch) {
    form.c.shl_bits_into(1, &mut scratch.modulus);
    form.b.add_into(&form.c, &mut scratch.work0);

    if scratch.work0.is_zero()
        || (!scratch.work0.is_negative()
            && scratch.work0.abs_cmp(&scratch.modulus) == Ordering::Less)
    {
        let old_a = std::mem::take(&mut form.a);
        let old_b = std::mem::take(&mut form.b);
        let old_c = std::mem::take(&mut form.c);
        form.a = old_c;
        form.b = old_b.negated();
        form.c = old_a;
        return;
    }

    if scratch.work0.is_negative() {
        if scratch.work0.abs_cmp(&scratch.modulus) != Ordering::Greater {
            let old_a = std::mem::take(&mut form.a);
            let old_b = std::mem::take(&mut form.b);
            let old_c = std::mem::take(&mut form.c);

            old_c.shl_bits_into(1, &mut scratch.work0);
            scratch.work0.add_assign(&old_b);
            old_c.add_into(&old_b, &mut scratch.work1);

            form.a = old_c;
            form.b = std::mem::take(&mut scratch.work0).negated();
            form.c = old_a;
            form.c.add_assign(&scratch.work1);
            return;
        }
    } else if scratch.work0.abs_cmp_double(&scratch.modulus) == Ordering::Less {
        let old_a = std::mem::take(&mut form.a);
        let old_b = std::mem::take(&mut form.b);
        let old_c = std::mem::take(&mut form.c);

        old_c.shl_bits_into(1, &mut scratch.work0);
        scratch.work0.sub_assign(&old_b);
        old_c.sub_into(&old_b, &mut scratch.work1);

        form.a = old_c;
        form.b = std::mem::take(&mut scratch.work0);
        form.c = old_a;
        form.c.add_assign(&scratch.work1);
        return;
    }

    let s = scratch
        .work0
        .div_floor_with_scratch(&scratch.modulus, &mut scratch.limbs);
    let old_a = std::mem::take(&mut form.a);
    let old_b = std::mem::take(&mut form.b);
    let old_c = std::mem::take(&mut form.c);

    form.a = old_c;
    form.a.mul_into(&s, &mut scratch.work0);
    scratch.work0.sub_into(&old_b, &mut scratch.work1);

    form.b = std::mem::take(&mut scratch.work0);
    form.b.add_assign(&scratch.work1);

    s.mul_into(&scratch.work1, &mut scratch.work0);
    form.c = old_a;
    form.c.add_assign(&scratch.work0);
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use kyn_vdf::{Form, create_discriminant, isqrt_fourth};
    use num_traits::Signed;

    use super::{LimbForm, LimbFormScratch, to_limb};

    #[test]
    fn limb_nudupl_matches_kyn_across_sequential_squares() {
        let discriminant = create_discriminant(b"iuna-vdf-limb-nudupl", 1024).unwrap();
        let threshold = isqrt_fourth(&discriminant.abs());
        let limb_discriminant = to_limb(&discriminant);
        let limb_threshold = to_limb(&threshold);
        let mut expected = Form::generator(&discriminant).unwrap();
        let mut actual = LimbForm::from_form(&expected);

        for round in 1..=10_000 {
            expected = expected.nudupl(&discriminant, &threshold);
            expected.reduce(&discriminant);
            actual = actual.nudupl_reduce(&limb_discriminant, &limb_threshold);

            assert_eq!(actual.clone().into_form(), expected, "round={round}");
        }
    }

    #[test]
    fn limb_nucomp_matches_kyn_across_sequential_compositions() {
        let discriminant = create_discriminant(b"iuna-vdf-limb-nucomp", 1024).unwrap();
        let threshold = isqrt_fourth(&discriminant.abs());
        let limb_discriminant = to_limb(&discriminant);
        let limb_threshold = to_limb(&threshold);
        let generator = Form::generator(&discriminant).unwrap();
        let mut expected_left = generator.clone();
        let mut actual_left = LimbForm::from_form(&expected_left);
        let mut expected_right = generator.nudupl(&discriminant, &threshold);
        expected_right.reduce(&discriminant);
        let mut actual_right = LimbForm::from_form(&expected_right);

        for round in 1..=2_000 {
            let mut expected = expected_left.nucomp(&expected_right, &discriminant, &threshold);
            expected.reduce(&discriminant);
            let actual =
                actual_left.nucomp_reduce(&actual_right, &limb_discriminant, &limb_threshold);

            assert_eq!(actual.clone().into_form(), expected, "round={round}");
            expected_left = expected;
            actual_left = actual;
            expected_right = expected_right.nudupl(&discriminant, &threshold);
            expected_right.reduce(&discriminant);
            actual_right = actual_right.nudupl_reduce(&limb_discriminant, &limb_threshold);
        }
    }

    #[test]
    #[ignore = "manual custom limb NUDUPL benchmark"]
    fn benchmark_limb_nudupl_against_optimized_bigint() {
        let rounds = 100_000;
        let discriminant = create_discriminant(b"iuna-vdf-limb-benchmark", 1024).unwrap();
        let threshold = isqrt_fourth(&discriminant.abs());
        let limb_discriminant = to_limb(&discriminant);
        let limb_threshold = to_limb(&threshold);
        let generator = Form::generator(&discriminant).unwrap();

        let mut bigint_output = generator.clone();
        let started = Instant::now();
        for _ in 0..rounds {
            bigint_output = crate::domain::vdf::arithmetic::nudupl_owned(
                bigint_output,
                &discriminant,
                &threshold,
            );
            crate::domain::vdf::reducer::reduce(&mut bigint_output);
        }
        let bigint_elapsed = started.elapsed();

        let mut limb_output = LimbForm::from_form(&generator);
        let mut limb_scratch = LimbFormScratch::default();
        let started = Instant::now();
        for _ in 0..rounds {
            limb_output = limb_output.nudupl_reduce_with_scratch(
                &limb_discriminant,
                &limb_threshold,
                &mut limb_scratch,
            );
        }
        let limb_elapsed = started.elapsed();

        assert_eq!(limb_output.into_form(), bigint_output);
        eprintln!(
            "rounds={rounds} optimized_bigint={bigint_elapsed:?} custom_limb={limb_elapsed:?} speedup={:.2}x",
            bigint_elapsed.as_secs_f64() / limb_elapsed.as_secs_f64()
        );
    }

    #[test]
    #[ignore = "manual custom limb NUDUPL phase benchmark"]
    fn benchmark_limb_nudupl_phases() {
        let rounds = 100_000;
        let discriminant = create_discriminant(b"iuna-vdf-limb-benchmark", 1024).unwrap();
        let threshold = isqrt_fourth(&discriminant.abs());
        let limb_discriminant = to_limb(&discriminant);
        let limb_threshold = to_limb(&threshold);
        let generator = Form::generator(&discriminant).unwrap();
        let mut output = LimbForm::from_form(&generator);
        let mut scratch = LimbFormScratch::default();
        let mut nudupl_elapsed = std::time::Duration::ZERO;
        let mut reduce_elapsed = std::time::Duration::ZERO;

        for _ in 0..rounds {
            let started = Instant::now();
            output = super::nudupl_owned(output, &limb_discriminant, &limb_threshold, &mut scratch);
            nudupl_elapsed += started.elapsed();

            let started = Instant::now();
            super::reduce_with_scratch(&mut output, &mut scratch);
            reduce_elapsed += started.elapsed();
        }

        assert!(output.clone().into_form().is_reduced());
        eprintln!(
            "rounds={rounds} nudupl={nudupl_elapsed:?} reduce={reduce_elapsed:?} total={:?}",
            nudupl_elapsed + reduce_elapsed
        );
    }
}
