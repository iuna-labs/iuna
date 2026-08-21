use std::mem;

use kyn_vdf::Form;
use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{Signed, Zero};

const APPROXIMATION_EXPONENT_SPREAD: u64 = 31;
const TRANSFORM_COEFFICIENT_LIMIT: i128 = 1_i128 << 31;

pub(super) fn reduce(form: &mut Form) {
    while !finish_if_reduced(form) {
        let (a, a_exponent) = signed_63_bit_approximation(&form.a);
        let (b, b_exponent) = signed_63_bit_approximation(&form.b);
        let (c, c_exponent) = signed_63_bit_approximation(&form.c);
        let min_exponent = a_exponent.min(b_exponent).min(c_exponent);
        let max_exponent = a_exponent.max(b_exponent).max(c_exponent);

        if max_exponent - min_exponent > APPROXIMATION_EXPONENT_SPREAD {
            reduce_once(form);
            continue;
        }

        let common_exponent = max_exponent + 1;
        let a = signed_shift(a, a_exponent as i64 - common_exponent as i64);
        let b = signed_shift(b, b_exponent as i64 - common_exponent as i64);
        let c = signed_shift(c, c_exponent as i64 - common_exponent as i64);
        let transform = approximate_transform(a, b, c);
        apply_transform(form, transform);
    }
}

fn finish_if_reduced(form: &mut Form) -> bool {
    if form.a.abs() < form.b.abs() || form.c.abs() < form.b.abs() {
        return false;
    }

    if form.a > form.c {
        mem::swap(&mut form.a, &mut form.c);
        form.b = -mem::take(&mut form.b);
    } else if form.a == form.c && form.b.is_negative() {
        form.b = -mem::take(&mut form.b);
    }
    true
}

fn reduce_once(form: &mut Form) {
    let two_c = &form.c << 1_usize;
    let s = (&form.b + &form.c).div_floor(&two_c);
    let old_a = mem::take(&mut form.a);
    let old_b = mem::take(&mut form.b);
    let old_c = mem::take(&mut form.c);
    let c_times_s = &old_c * &s;

    form.a = old_c;
    form.b = (&c_times_s << 1_usize) - &old_b;
    form.c = old_a + &s * (c_times_s - old_b);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Transform {
    u: i128,
    v: i128,
    w: i128,
    x: i128,
}

fn approximate_transform(mut a: i128, mut b: i128, mut c: i128) -> Transform {
    let mut current = Transform {
        u: 1,
        v: 0,
        w: 0,
        x: 1,
    };

    loop {
        let s = if b >= 0 {
            (b + c) / (c << 1)
        } else {
            -((-b + c) / (c << 1))
        };
        let old_a = a;
        let old_b = b;
        a = c;
        b = -b + ((c * s) << 1);
        c = old_a - s * (old_b - c * s);

        let next = Transform {
            u: current.v,
            v: -current.u + s * current.v,
            w: current.x,
            x: -current.w + s * current.x,
        };
        let coefficients_fit = (next.v.abs() | next.x.abs()) <= TRANSFORM_COEFFICIENT_LIMIT;
        if coefficients_fit {
            current = next;
        }
        if !coefficients_fit || a <= c || c <= 0 {
            return current;
        }
    }
}

fn apply_transform(form: &mut Form, transform: Transform) {
    let old_a = mem::take(&mut form.a);
    let old_b = mem::take(&mut form.b);
    let old_c = mem::take(&mut form.c);
    let Transform { u, v, w, x } = transform;

    form.a = scaled(&old_a, u * u) + scaled(&old_b, u * w) + scaled(&old_c, w * w);
    form.b = scaled(&old_a, 2 * u * v) + scaled(&old_b, u * x + v * w) + scaled(&old_c, 2 * w * x);
    form.c = scaled(&old_a, v * v) + scaled(&old_b, v * x) + scaled(&old_c, x * x);
}

fn scaled(value: &BigInt, scalar: i128) -> BigInt {
    value * BigInt::from(scalar)
}

fn signed_63_bit_approximation(value: &BigInt) -> (i128, u64) {
    if value.is_zero() {
        return (0, 0);
    }

    let mut digits = value.iter_u64_digits();
    let digit_count = digits.len() as u64;
    let top = digits
        .next_back()
        .expect("a nonzero BigInt has a top digit");
    let top_bits = u64::from(64 - top.leading_zeros());
    let exponent = top_bits + (digit_count - 1) * 64;
    let mut approximation = if top_bits == 64 {
        top >> 1
    } else {
        top << (63 - top_bits)
    };
    if let Some(previous) = digits.next_back() {
        let shift = top_bits + 1;
        if shift < 64 {
            approximation += previous >> shift;
        }
    }

    let approximation = i128::from(approximation);
    if value.is_negative() {
        (-approximation, exponent)
    } else {
        (approximation, exponent)
    }
}

fn signed_shift(value: i128, shift: i64) -> i128 {
    if shift > 0 {
        value << shift
    } else if shift <= -128 {
        0
    } else {
        value >> -shift
    }
}

#[cfg(test)]
mod tests {
    use kyn_vdf::{Form, create_discriminant, isqrt_fourth};
    use num_traits::Signed;

    use super::reduce;

    #[test]
    fn pulmark_reducer_matches_canonical_reducer_across_sequential_squares() {
        let discriminant = create_discriminant(b"iuna-pulmark-reducer-differential", 1024).unwrap();
        let threshold = isqrt_fourth(&discriminant.abs());
        let mut form = Form::generator(&discriminant).unwrap();

        for round in 1..=10_000 {
            let unreduced = form.nudupl(&discriminant, &threshold);
            let mut expected = unreduced.clone();
            expected.reduce(&discriminant);
            let mut actual = unreduced;
            reduce(&mut actual);

            assert_eq!(actual, expected, "round={round}");
            assert!(actual.is_reduced(), "round={round}");
            form = actual;
        }
    }
}
