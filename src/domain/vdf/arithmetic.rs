use kyn_vdf::Form;
use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{One, Signed, Zero};

#[cfg(test)]
pub(super) fn nudupl(form: &Form, discriminant: &BigInt, threshold: &BigInt) -> Form {
    nudupl_owned(form.clone(), discriminant, threshold)
}

pub(super) fn nudupl_owned(form: Form, discriminant: &BigInt, threshold: &BigInt) -> Form {
    let two = BigInt::from(2);
    let four = BigInt::from(4);
    let Form { a, b, c } = form;
    let mut a1 = a;
    let mut c1 = c;

    let gcd = if b.is_negative() {
        let b_abs = -&b;
        let gcd = b_abs.extended_gcd(&a1);
        (-gcd.x, gcd.gcd)
    } else {
        let gcd = b.extended_gcd(&a1);
        (gcd.x, gcd.gcd)
    };

    let mut k = -(&gcd.0 * &c1);
    let s = gcd.1;
    if s != BigInt::one() {
        a1 /= &s;
        c1 *= &s;
    }
    k = mod_positive(k, &a1);

    if a1 < *threshold {
        let t = &a1 * &k;
        let result_a = &a1 * &a1;
        let result_b = &two * &t + &b;
        let result_c = ((&b + &t) * &k + &c1) / &a1;
        Form::new(result_a, result_b, result_c)
    } else {
        let (co2, co1, _r2, r1) = xgcd_partial(&a1, &k, threshold);
        let m2 = (&b * &r1 - &c1 * &co1) / &a1;

        let mut result_a = &r1 * &r1 - &co1 * &m2;
        if !co1.is_negative() {
            result_a = -result_a;
        }

        let result_b = mod_positive(
            (&two * (&a1 * &r1 - &result_a * &co2)) / &co1 - &b,
            &(&result_a * &two),
        );
        let mut result_c = (&result_b * &result_b - discriminant) / (&result_a * &four);

        if result_a.is_negative() {
            result_a = -result_a;
            result_c = -result_c;
        }

        Form::new(result_a, result_b, result_c)
    }
}

pub(super) fn nucomp(left: &Form, right: &Form, discriminant: &BigInt, threshold: &BigInt) -> Form {
    if left.a > right.a {
        return nucomp(right, left, discriminant, threshold);
    }

    let two = BigInt::from(2);
    let four = BigInt::from(4);
    let mut a1 = left.a.clone();
    let mut a2 = right.a.clone();
    let mut c2 = right.c.clone();
    let ss = (&left.b + &right.b) / &two;
    let m = (&left.b - &right.b) / &two;

    let t = &a2 % &a1;
    let (v1, sp) = if t.is_zero() {
        (BigInt::zero(), a1.clone())
    } else {
        let gcd = t.extended_gcd(&a1);
        (gcd.x, gcd.gcd)
    };
    let mut k = mod_positive(&m * &v1, &a1);

    if sp != BigInt::one() {
        let gcd = ss.extended_gcd(&sp);
        let v2 = gcd.x;
        let u2 = gcd.y;
        let s = gcd.gcd;
        k = &k * &u2 - &v2 * &c2;
        if s != BigInt::one() {
            a1 /= &s;
            a2 /= &s;
            c2 *= &s;
        }
        k = mod_positive(k, &a1);
    }

    if a1 < *threshold {
        let t = &a2 * &k;
        let result_a = &a2 * &a1;
        let result_b = &two * &t + &right.b;
        let result_c = ((&right.b + &t) * &k + &c2) / &a1;
        Form::new(result_a, result_b, result_c)
    } else {
        let (co2, co1, _r2, r1) = xgcd_partial(&a1, &k, threshold);
        let m1 = (&m * &co1 + &a2 * &r1) / &a1;
        let m2 = (&ss * &r1 - &c2 * &co1) / &a1;

        let mut result_a = &r1 * &m1 - &co1 * &m2;
        if !co1.is_negative() {
            result_a = -result_a;
        }

        let t = &a2 * &r1;
        let result_b = mod_positive(
            (&two * (&t - &result_a * &co2)) / &co1 - &right.b,
            &(&result_a * &two),
        );
        let mut result_c = (&result_b * &result_b - discriminant) / (&result_a * &four);

        if result_a.is_negative() {
            result_a = -result_a;
            result_c = -result_c;
        }

        Form::new(result_a, result_b, result_c)
    }
}

pub(super) fn xgcd_partial(
    r2: &BigInt,
    r1: &BigInt,
    threshold: &BigInt,
) -> (BigInt, BigInt, BigInt, BigInt) {
    let mut r2 = r2.clone();
    let mut r1 = r1.clone();
    let mut co2 = BigInt::zero();
    let mut co1 = BigInt::from(-1);

    while !r1.is_zero() && &r1 > threshold {
        let bits = r2.bits().max(r1.bits()).saturating_sub(63);
        let mut rr2 = shifted_low_word(&r2, bits);
        let mut rr1 = shifted_low_word(&r1, bits);
        let threshold_word = shifted_low_word(threshold, bits);

        let mut aa2 = 0_i128;
        let mut aa1 = 1_i128;
        let mut bb2 = 1_i128;
        let mut bb1 = 0_i128;
        let mut steps = 0_u32;

        while rr1 != 0 && rr1 > threshold_word {
            let q = rr2 / rr1;
            let next_r = rr2 - q * rr1;
            let next_a = aa2 - q * aa1;
            let next_b = bb2 - q * bb1;

            if steps & 1 == 1 {
                if next_r < -next_b || rr1 - next_r < next_a - aa1 {
                    break;
                }
            } else if next_r < -next_a || rr1 - next_r < next_b - bb1 {
                break;
            }

            rr2 = rr1;
            rr1 = next_r;
            aa2 = aa1;
            aa1 = next_a;
            bb2 = bb1;
            bb1 = next_b;
            steps += 1;
        }

        if steps == 0 {
            let (q, next_r) = r2.div_rem(&r1);
            let next_co = &co2 - &q * &co1;
            r2 = r1;
            r1 = next_r;
            co2 = co1;
            co1 = next_co;
        } else {
            let old_r2 = r2;
            let old_r1 = r1;
            r2 = scaled(&old_r2, bb2) + scaled(&old_r1, aa2);
            r1 = scaled(&old_r1, aa1) + scaled(&old_r2, bb1);

            let old_co2 = co2;
            let old_co1 = co1;
            co2 = scaled(&old_co2, bb2) + scaled(&old_co1, aa2);
            co1 = scaled(&old_co1, aa1) + scaled(&old_co2, bb1);

            if r1.is_negative() {
                r1 = -r1;
                co1 = -co1;
            }
            if r2.is_negative() {
                r2 = -r2;
                co2 = -co2;
            }
        }
    }

    if r2.is_negative() {
        r2 = -r2;
        co2 = -co2;
        co1 = -co1;
    }

    (co2, co1, r2, r1)
}

fn scaled(value: &BigInt, scalar: i128) -> BigInt {
    value * BigInt::from(scalar)
}

fn mod_positive(mut value: BigInt, modulus: &BigInt) -> BigInt {
    value %= modulus;
    if value.is_negative() {
        value += modulus;
    }
    value
}

fn shifted_low_word(value: &BigInt, shift_bits: u64) -> i128 {
    debug_assert!(!value.is_negative());
    let mut digits = value.iter_u64_digits();
    let digit_index = usize::try_from(shift_bits / 64).unwrap_or(usize::MAX);
    let offset = (shift_bits % 64) as u32;
    let low = digits.nth(digit_index).unwrap_or(0);
    if offset == 0 {
        return i128::from(low);
    }
    let high = digits.next().unwrap_or(0);
    i128::from((low >> offset) | (high << (64 - offset)))
}

#[cfg(test)]
mod tests {
    use kyn_vdf::{Form, create_discriminant, isqrt_fourth};
    use num_traits::Signed;

    use super::{nucomp, nudupl};

    #[test]
    fn optimized_nudupl_matches_kyn_across_sequential_squares() {
        let discriminant = create_discriminant(b"iuna-vdf-arithmetic-nudupl", 1024).unwrap();
        let threshold = isqrt_fourth(&discriminant.abs());
        let mut form = Form::generator(&discriminant).unwrap();

        for round in 1..=10_000 {
            let mut expected = form.nudupl(&discriminant, &threshold);
            expected.reduce(&discriminant);
            let mut actual = nudupl(&form, &discriminant, &threshold);
            actual.reduce(&discriminant);

            assert_eq!(actual, expected, "round={round}");
            form = actual;
        }
    }

    #[test]
    fn optimized_nucomp_matches_kyn_across_sequential_compositions() {
        let discriminant = create_discriminant(b"iuna-vdf-arithmetic-nucomp", 1024).unwrap();
        let threshold = isqrt_fourth(&discriminant.abs());
        let generator = Form::generator(&discriminant).unwrap();
        let mut left = generator.clone();
        let mut right = generator.nudupl(&discriminant, &threshold);
        right.reduce(&discriminant);

        for round in 1..=2_000 {
            let mut expected = left.nucomp(&right, &discriminant, &threshold);
            expected.reduce(&discriminant);
            let mut actual = nucomp(&left, &right, &discriminant, &threshold);
            actual.reduce(&discriminant);

            assert_eq!(actual, expected, "round={round}");
            left = actual;
            right = right.nudupl(&discriminant, &threshold);
            right.reduce(&discriminant);
        }
    }
}
