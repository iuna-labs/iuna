#![allow(dead_code)]

use std::cmp::Ordering;

use num_bigint::{BigInt, Sign as BigSign};
use num_traits::Signed;

type LimbVec = Vec<u64>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Sign {
    Negative,
    Zero,
    Positive,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LimbInt {
    sign: Sign,
    limbs: LimbVec,
}

impl Default for LimbInt {
    fn default() -> Self {
        Self::zero()
    }
}

pub(super) struct ExtendedGcd {
    pub(super) x: LimbInt,
    pub(super) y: LimbInt,
    pub(super) gcd: LimbInt,
}

pub(super) struct LeftExtendedGcd {
    pub(super) x: LimbInt,
    pub(super) gcd: LimbInt,
}

#[derive(Default)]
pub(super) struct LimbScratch {
    division: DivisionScratch,
    linear_left: LimbVec,
    linear_right: LimbVec,
}

#[derive(Default)]
struct DivisionScratch {
    normalized_numerator: LimbVec,
    normalized_denominator: LimbVec,
    quotient: LimbVec,
    remainder: LimbVec,
}

impl LimbInt {
    pub(super) fn zero() -> Self {
        Self {
            sign: Sign::Zero,
            limbs: LimbVec::new(),
        }
    }

    pub(super) fn one() -> Self {
        Self {
            sign: Sign::Positive,
            limbs: vec![1],
        }
    }

    pub(super) fn from_u64(value: u64) -> Self {
        if value == 0 {
            Self::zero()
        } else {
            Self {
                sign: Sign::Positive,
                limbs: vec![value],
            }
        }
    }

    pub(super) fn from_i128(value: i128) -> Self {
        if value == 0 {
            return Self::zero();
        }

        let sign = if value < 0 {
            Sign::Negative
        } else {
            Sign::Positive
        };
        let mut magnitude = value.unsigned_abs();
        let mut limbs = LimbVec::new();
        while magnitude > 0 {
            limbs.push(magnitude as u64);
            magnitude >>= 64;
        }
        Self { sign, limbs }
    }

    pub(super) fn from_bigint(value: &BigInt) -> Self {
        if value == &BigInt::from(0) {
            return Self::zero();
        }

        let sign = if value.is_negative() {
            Sign::Negative
        } else {
            Sign::Positive
        };
        let magnitude = value
            .abs()
            .to_biguint()
            .expect("absolute BigInt is non-negative");
        let mut limbs: LimbVec = magnitude.iter_u64_digits().collect();
        trim_leading_zero_limbs(&mut limbs);
        Self { sign, limbs }
    }

    pub(super) fn to_bigint(&self) -> BigInt {
        let sign = match self.sign {
            Sign::Negative => BigSign::Minus,
            Sign::Zero => BigSign::NoSign,
            Sign::Positive => BigSign::Plus,
        };
        BigInt::from_biguint(sign, self.abs_biguint())
    }

    #[inline(always)]
    pub(super) fn is_zero(&self) -> bool {
        self.sign == Sign::Zero
    }

    #[inline(always)]
    pub(super) fn is_negative(&self) -> bool {
        self.sign == Sign::Negative
    }

    #[inline(always)]
    pub(super) fn is_one(&self) -> bool {
        self.sign == Sign::Positive && self.limbs.as_slice() == [1]
    }

    #[inline(always)]
    pub(super) fn is_minus_one(&self) -> bool {
        self.sign == Sign::Negative && self.limbs.as_slice() == [1]
    }

    #[inline(always)]
    pub(super) fn bit_len(&self) -> u64 {
        let Some(last) = self.limbs.last() else {
            return 0;
        };
        let top_bits = u64::BITS - last.leading_zeros();
        ((self.limbs.len() as u64 - 1) * 64) + u64::from(top_bits)
    }

    #[inline(always)]
    pub(super) fn abs_cmp(&self, other: &Self) -> Ordering {
        cmp_abs_limbs(&self.limbs, &other.limbs)
    }

    #[inline(always)]
    pub(super) fn abs_cmp_double(&self, other: &Self) -> Ordering {
        cmp_abs_to_double(&self.limbs, &other.limbs)
    }

    pub(super) fn cmp(&self, other: &Self) -> Ordering {
        match (self.sign, other.sign) {
            (Sign::Negative, Sign::Negative) => cmp_abs_limbs(&other.limbs, &self.limbs),
            (Sign::Negative, _) => Ordering::Less,
            (_, Sign::Negative) => Ordering::Greater,
            (Sign::Zero, Sign::Zero) => Ordering::Equal,
            (Sign::Zero, Sign::Positive) => Ordering::Less,
            (Sign::Positive, Sign::Zero) => Ordering::Greater,
            (Sign::Positive, Sign::Positive) => cmp_abs_limbs(&self.limbs, &other.limbs),
        }
    }

    pub(super) fn negated(mut self) -> Self {
        self.sign = match self.sign {
            Sign::Negative => Sign::Positive,
            Sign::Zero => Sign::Zero,
            Sign::Positive => Sign::Negative,
        };
        self
    }

    pub(super) fn negate_assign(&mut self) {
        self.sign = match self.sign {
            Sign::Negative => Sign::Positive,
            Sign::Zero => Sign::Zero,
            Sign::Positive => Sign::Negative,
        };
    }

    pub(super) fn add(&self, other: &Self) -> Self {
        match (self.sign, other.sign) {
            (Sign::Zero, _) => other.clone(),
            (_, Sign::Zero) => self.clone(),
            (Sign::Positive, Sign::Positive) => {
                Self::from_parts(Sign::Positive, add_abs_limbs(&self.limbs, &other.limbs))
            }
            (Sign::Negative, Sign::Negative) => {
                Self::from_parts(Sign::Negative, add_abs_limbs(&self.limbs, &other.limbs))
            }
            (Sign::Positive, Sign::Negative) => {
                subtract_signed_abs(&self.limbs, Sign::Positive, &other.limbs, Sign::Negative)
            }
            (Sign::Negative, Sign::Positive) => {
                subtract_signed_abs(&other.limbs, Sign::Positive, &self.limbs, Sign::Negative)
            }
        }
    }

    pub(super) fn add_owned(mut self, other: &Self) -> Self {
        self.add_assign(other);
        self
    }

    #[inline(always)]
    pub(super) fn add_into(&self, other: &Self, output: &mut Self) {
        match (self.sign, other.sign) {
            (Sign::Zero, _) => {
                *output = other.clone();
            }
            (_, Sign::Zero) => {
                *output = self.clone();
            }
            (Sign::Positive, Sign::Positive) | (Sign::Negative, Sign::Negative) => {
                add_abs_limbs_into(&self.limbs, &other.limbs, &mut output.limbs);
                output.sign = if output.limbs.is_empty() {
                    Sign::Zero
                } else {
                    self.sign
                };
            }
            (Sign::Positive, Sign::Negative) | (Sign::Negative, Sign::Positive) => {
                let sign = combine_signed_abs_limbs_into(
                    self.sign,
                    &self.limbs,
                    other.sign,
                    &other.limbs,
                    &mut output.limbs,
                );
                output.sign = if output.limbs.is_empty() {
                    Sign::Zero
                } else {
                    sign
                };
            }
        }
    }

    pub(super) fn add_assign(&mut self, other: &Self) {
        match (self.sign, other.sign) {
            (Sign::Zero, _) => {
                *self = other.clone();
            }
            (_, Sign::Zero) => {}
            (Sign::Positive, Sign::Positive) | (Sign::Negative, Sign::Negative) => {
                add_abs_limbs_assign(&mut self.limbs, &other.limbs);
            }
            (Sign::Positive, Sign::Negative) => match cmp_abs_limbs(&self.limbs, &other.limbs) {
                Ordering::Greater => sub_abs_limbs_assign(&mut self.limbs, &other.limbs),
                Ordering::Less => {
                    self.limbs = sub_abs_limbs(&other.limbs, &self.limbs);
                    self.sign = Sign::Negative;
                }
                Ordering::Equal => *self = Self::zero(),
            },
            (Sign::Negative, Sign::Positive) => match cmp_abs_limbs(&self.limbs, &other.limbs) {
                Ordering::Greater => sub_abs_limbs_assign(&mut self.limbs, &other.limbs),
                Ordering::Less => {
                    self.limbs = sub_abs_limbs(&other.limbs, &self.limbs);
                    self.sign = Sign::Positive;
                }
                Ordering::Equal => *self = Self::zero(),
            },
        }
    }

    pub(super) fn sub(&self, other: &Self) -> Self {
        let mut result = self.clone();
        result.sub_assign(other);
        result
    }

    pub(super) fn sub_owned(mut self, other: &Self) -> Self {
        self.sub_assign(other);
        self
    }

    #[inline(always)]
    pub(super) fn sub_into(&self, other: &Self, output: &mut Self) {
        match (self.sign, other.sign) {
            (_, Sign::Zero) => {
                *output = self.clone();
            }
            (Sign::Zero, _) => {
                *output = other.clone().negated();
            }
            (Sign::Positive, Sign::Positive) | (Sign::Negative, Sign::Negative) => {
                match cmp_abs_limbs(&self.limbs, &other.limbs) {
                    Ordering::Greater => {
                        sub_abs_limbs_into(&self.limbs, &other.limbs, &mut output.limbs);
                        output.sign = self.sign;
                    }
                    Ordering::Less => {
                        sub_abs_limbs_into(&other.limbs, &self.limbs, &mut output.limbs);
                        output.sign = match self.sign {
                            Sign::Positive => Sign::Negative,
                            Sign::Negative => Sign::Positive,
                            Sign::Zero => Sign::Zero,
                        };
                    }
                    Ordering::Equal => {
                        *output = Self::zero();
                    }
                }
            }
            (Sign::Positive, Sign::Negative) | (Sign::Negative, Sign::Positive) => {
                add_abs_limbs_into(&self.limbs, &other.limbs, &mut output.limbs);
                output.sign = if output.limbs.is_empty() {
                    Sign::Zero
                } else {
                    self.sign
                };
            }
        }
    }

    pub(super) fn sub_assign(&mut self, other: &Self) {
        match (self.sign, other.sign) {
            (_, Sign::Zero) => {}
            (Sign::Zero, _) => {
                *self = other.clone().negated();
            }
            (Sign::Positive, Sign::Positive) => match cmp_abs_limbs(&self.limbs, &other.limbs) {
                Ordering::Greater => sub_abs_limbs_assign(&mut self.limbs, &other.limbs),
                Ordering::Less => {
                    self.limbs = sub_abs_limbs(&other.limbs, &self.limbs);
                    self.sign = Sign::Negative;
                }
                Ordering::Equal => *self = Self::zero(),
            },
            (Sign::Negative, Sign::Negative) => match cmp_abs_limbs(&self.limbs, &other.limbs) {
                Ordering::Greater => sub_abs_limbs_assign(&mut self.limbs, &other.limbs),
                Ordering::Less => {
                    self.limbs = sub_abs_limbs(&other.limbs, &self.limbs);
                    self.sign = Sign::Positive;
                }
                Ordering::Equal => *self = Self::zero(),
            },
            (Sign::Positive, Sign::Negative) | (Sign::Negative, Sign::Positive) => {
                add_abs_limbs_assign(&mut self.limbs, &other.limbs);
            }
        }
    }

    pub(super) fn mul(&self, other: &Self) -> Self {
        if self.is_zero() || other.is_zero() {
            return Self::zero();
        }

        let sign = if self.sign == other.sign {
            Sign::Positive
        } else {
            Sign::Negative
        };
        let limbs = match (self.limbs.as_slice(), other.limbs.as_slice()) {
            ([scalar], value) => mul_abs_one_limb(value, *scalar),
            (value, [scalar]) => mul_abs_one_limb(value, *scalar),
            _ => mul_abs_limbs(&self.limbs, &other.limbs),
        };
        Self::from_parts(sign, limbs)
    }

    pub(super) fn mul_into(&self, other: &Self, output: &mut Self) {
        if self.is_zero() || other.is_zero() {
            *output = Self::zero();
            return;
        }

        output.sign = if self.sign == other.sign {
            Sign::Positive
        } else {
            Sign::Negative
        };
        match (self.limbs.as_slice(), other.limbs.as_slice()) {
            ([scalar], value) => mul_abs_one_limb_into(value, *scalar, &mut output.limbs),
            (value, [scalar]) => mul_abs_one_limb_into(value, *scalar, &mut output.limbs),
            _ => mul_abs_limbs_into(&self.limbs, &other.limbs, &mut output.limbs),
        }
        if output.limbs.is_empty() {
            output.sign = Sign::Zero;
        }
    }

    pub(super) fn mul_i128(&self, scalar: i128) -> Self {
        if self.is_zero() || scalar == 0 {
            return Self::zero();
        }
        if scalar == 1 {
            return self.clone();
        }
        if scalar == -1 {
            return self.clone().negated();
        }

        let sign = if scalar < 0 {
            match self.sign {
                Sign::Negative => Sign::Positive,
                Sign::Positive => Sign::Negative,
                Sign::Zero => Sign::Zero,
            }
        } else {
            self.sign
        };
        Self::from_parts(sign, mul_abs_small(&self.limbs, scalar.unsigned_abs()))
    }

    pub(super) fn linear_combination_i128_with_scratch(
        left: &Self,
        left_scalar: i128,
        right: &Self,
        right_scalar: i128,
        scratch: &mut LimbScratch,
    ) -> Self {
        let mut output = Self::zero();
        Self::linear_combination_i128_into(
            left,
            left_scalar,
            right,
            right_scalar,
            &mut output,
            scratch,
        );
        output
    }

    #[inline(always)]
    fn linear_combination_i128_into(
        left: &Self,
        left_scalar: i128,
        right: &Self,
        right_scalar: i128,
        output: &mut Self,
        scratch: &mut LimbScratch,
    ) {
        if left_scalar == 0 && right_scalar == 0 {
            *output = Self::zero();
            return;
        }
        if left_scalar == 0 {
            right.mul_i128_into(right_scalar, output);
            return;
        }
        if right_scalar == 0 {
            left.mul_i128_into(left_scalar, output);
            return;
        }

        let left_abs = left_scalar.unsigned_abs();
        let right_abs = right_scalar.unsigned_abs();
        let left_sign = signed_scalar_sign(left.sign, left_scalar);
        let right_sign = signed_scalar_sign(right.sign, right_scalar);

        if left_abs == 1 && right_abs == 1 {
            let sign = combine_signed_abs_limbs_into(
                left_sign,
                &left.limbs,
                right_sign,
                &right.limbs,
                &mut output.limbs,
            );
            output.sign = if output.limbs.is_empty() {
                Sign::Zero
            } else {
                sign
            };
            return;
        }

        if left_abs == 1 {
            mul_abs_small_into(&right.limbs, right_abs, &mut scratch.linear_right);
            let sign = combine_signed_abs_limbs_into(
                left_sign,
                &left.limbs,
                right_sign,
                &scratch.linear_right,
                &mut output.limbs,
            );
            output.sign = if output.limbs.is_empty() {
                Sign::Zero
            } else {
                sign
            };
            return;
        }

        if right_abs == 1 {
            mul_abs_small_into(&left.limbs, left_abs, &mut scratch.linear_left);
            let sign = combine_signed_abs_limbs_into(
                left_sign,
                &scratch.linear_left,
                right_sign,
                &right.limbs,
                &mut output.limbs,
            );
            output.sign = if output.limbs.is_empty() {
                Sign::Zero
            } else {
                sign
            };
            return;
        }

        mul_abs_small_into(&left.limbs, left_abs, &mut scratch.linear_left);
        mul_abs_small_into(&right.limbs, right_abs, &mut scratch.linear_right);

        let sign = combine_signed_abs_limbs_into(
            left_sign,
            &scratch.linear_left,
            right_sign,
            &scratch.linear_right,
            &mut output.limbs,
        );

        output.sign = if output.limbs.is_empty() {
            Sign::Zero
        } else {
            sign
        };
    }

    #[inline(always)]
    fn mul_i128_into(&self, scalar: i128, output: &mut Self) {
        if self.is_zero() || scalar == 0 {
            *output = Self::zero();
            return;
        }

        let sign = if scalar < 0 {
            match self.sign {
                Sign::Negative => Sign::Positive,
                Sign::Positive => Sign::Negative,
                Sign::Zero => Sign::Zero,
            }
        } else {
            self.sign
        };
        output.sign = sign;
        output.limbs.clear();
        mul_abs_small_into(&self.limbs, scalar.unsigned_abs(), &mut output.limbs);
        if output.limbs.is_empty() {
            output.sign = Sign::Zero;
        }
    }

    pub(super) fn square(&self) -> Self {
        if self.is_zero() {
            return Self::zero();
        }
        Self::from_parts(Sign::Positive, square_abs_limbs(&self.limbs))
    }

    pub(super) fn square_into(&self, output: &mut Self) {
        if self.is_zero() {
            *output = Self::zero();
            return;
        }
        output.sign = Sign::Positive;
        mul_abs_limbs_into(&self.limbs, &self.limbs, &mut output.limbs);
        if output.limbs.is_empty() {
            output.sign = Sign::Zero;
        }
    }

    pub(super) fn shl_bits(&self, bits: usize) -> Self {
        if self.is_zero() || bits == 0 {
            return self.clone();
        }
        Self::from_parts(self.sign, shl_abs_limbs(&self.limbs, bits))
    }

    pub(super) fn shl_bits_into(&self, bits: usize, output: &mut Self) {
        if self.is_zero() {
            *output = Self::zero();
            return;
        }
        if bits == 0 {
            *output = self.clone();
            return;
        }
        output.sign = self.sign;
        shl_abs_limbs_into(&self.limbs, bits, &mut output.limbs);
        if output.limbs.is_empty() {
            output.sign = Sign::Zero;
        }
    }

    pub(super) fn shl_bits_owned(mut self, bits: usize) -> Self {
        if self.is_zero() || bits == 0 {
            return self;
        }
        shl_abs_limbs_assign(&mut self.limbs, bits);
        self
    }

    pub(super) fn shl_bits_assign(&mut self, bits: usize) {
        if self.is_zero() || bits == 0 {
            return;
        }
        shl_abs_limbs_assign(&mut self.limbs, bits);
    }

    pub(super) fn shr_abs_bits(&self, bits: usize) -> Self {
        if self.is_zero() || bits == 0 {
            return self.abs();
        }
        Self::from_parts(Sign::Positive, shr_abs_limbs(&self.limbs, bits))
    }

    pub(super) fn div_rem(&self, divisor: &Self) -> (Self, Self) {
        let mut scratch = LimbScratch::default();
        self.div_rem_with_scratch(divisor, &mut scratch)
    }

    pub(super) fn div_rem_with_scratch(
        &self,
        divisor: &Self,
        scratch: &mut LimbScratch,
    ) -> (Self, Self) {
        assert!(!divisor.is_zero(), "division by zero");
        if self.is_zero() {
            return (Self::zero(), Self::zero());
        }

        let (quotient_limbs, remainder_limbs) =
            div_rem_abs_limbs_with_scratch(&self.limbs, &divisor.limbs, &mut scratch.division);
        let quotient_sign = if quotient_limbs.is_empty() {
            Sign::Zero
        } else if self.sign == divisor.sign {
            Sign::Positive
        } else {
            Sign::Negative
        };
        let remainder_sign = if remainder_limbs.is_empty() {
            Sign::Zero
        } else {
            self.sign
        };

        (
            Self::from_parts(quotient_sign, quotient_limbs),
            Self::from_parts(remainder_sign, remainder_limbs),
        )
    }

    pub(super) fn rem(&self, divisor: &Self) -> Self {
        self.div_rem(divisor).1
    }

    pub(super) fn rem_with_scratch(&self, divisor: &Self, scratch: &mut LimbScratch) -> Self {
        assert!(!divisor.is_zero(), "division by zero");
        if self.is_zero() {
            return Self::zero();
        }

        let remainder_limbs =
            rem_abs_limbs_with_scratch(&self.limbs, &divisor.limbs, &mut scratch.division);
        let remainder_sign = if remainder_limbs.is_empty() {
            Sign::Zero
        } else {
            self.sign
        };
        Self::from_parts(remainder_sign, remainder_limbs)
    }

    pub(super) fn div(&self, divisor: &Self) -> Self {
        self.div_rem(divisor).0
    }

    pub(super) fn div_with_scratch(&self, divisor: &Self, scratch: &mut LimbScratch) -> Self {
        assert!(!divisor.is_zero(), "division by zero");
        if self.is_zero() {
            return Self::zero();
        }
        match self.abs_cmp(divisor) {
            Ordering::Less => return Self::zero(),
            Ordering::Equal => {
                return if self.sign == divisor.sign {
                    Self::one()
                } else {
                    Self::from_i128(-1)
                };
            }
            Ordering::Greater => {
                if cmp_abs_to_double(&self.limbs, &divisor.limbs) == Ordering::Less {
                    return if self.sign == divisor.sign {
                        Self::one()
                    } else {
                        Self::from_i128(-1)
                    };
                }
            }
        }
        let quotient_limbs =
            div_abs_limbs_with_scratch(&self.limbs, &divisor.limbs, &mut scratch.division);
        let quotient_sign = if quotient_limbs.is_empty() {
            Sign::Zero
        } else if self.sign == divisor.sign {
            Sign::Positive
        } else {
            Sign::Negative
        };
        Self::from_parts(quotient_sign, quotient_limbs)
    }

    pub(super) fn div2_exact(&self) -> Self {
        debug_assert!(self.limbs.first().copied().unwrap_or(0) & 1 == 0);
        if self.is_zero() {
            return Self::zero();
        }
        Self::from_parts(self.sign, shr_abs_limbs(&self.limbs, 1))
    }

    pub(super) fn div_floor(&self, divisor: &Self) -> Self {
        let (quotient, remainder) = self.div_rem(divisor);
        self.finish_div_floor(divisor, quotient, remainder)
    }

    pub(super) fn div_floor_with_scratch(&self, divisor: &Self, scratch: &mut LimbScratch) -> Self {
        assert!(!divisor.is_zero(), "division by zero");
        if self.is_zero() {
            return Self::zero();
        }

        match self.abs_cmp(divisor) {
            Ordering::Less => {
                return if self.sign == divisor.sign {
                    Self::zero()
                } else {
                    Self::from_i128(-1)
                };
            }
            Ordering::Equal => {
                return if self.sign == divisor.sign {
                    Self::one()
                } else {
                    Self::from_i128(-1)
                };
            }
            Ordering::Greater => {
                if cmp_abs_to_double(&self.limbs, &divisor.limbs) == Ordering::Less {
                    return if self.sign == divisor.sign {
                        Self::one()
                    } else {
                        Self::from_i128(-2)
                    };
                }
            }
        }

        if self.sign == divisor.sign {
            let quotient_limbs =
                div_abs_limbs_with_scratch(&self.limbs, &divisor.limbs, &mut scratch.division);
            let quotient_sign = if quotient_limbs.is_empty() {
                Sign::Zero
            } else {
                Sign::Positive
            };
            return Self::from_parts(quotient_sign, quotient_limbs);
        }

        let (quotient, remainder) = self.div_rem_with_scratch(divisor, scratch);
        self.finish_div_floor(divisor, quotient, remainder)
    }

    fn finish_div_floor(&self, divisor: &Self, quotient: Self, remainder: Self) -> Self {
        if remainder.is_zero() || self.sign == divisor.sign {
            quotient
        } else {
            quotient.sub(&Self::one())
        }
    }

    pub(super) fn mod_positive(&self, modulus: &Self) -> Self {
        let mut scratch = LimbScratch::default();
        self.mod_positive_with_scratch(modulus, &mut scratch)
    }

    pub(super) fn mod_positive_with_scratch(
        &self,
        modulus: &Self,
        scratch: &mut LimbScratch,
    ) -> Self {
        assert!(!modulus.is_zero(), "division by zero");
        if self.is_zero() {
            return Self::zero();
        }
        match self.abs_cmp(modulus) {
            Ordering::Less => {
                return if self.is_negative() {
                    self.add(modulus)
                } else {
                    self.clone()
                };
            }
            Ordering::Equal | Ordering::Greater => {}
        }

        let remainder_limbs =
            rem_abs_limbs_with_scratch(&self.limbs, &modulus.limbs, &mut scratch.division);
        let remainder_sign = if remainder_limbs.is_empty() {
            Sign::Zero
        } else {
            self.sign
        };
        let mut remainder = Self::from_parts(remainder_sign, remainder_limbs);
        if remainder.is_negative() {
            remainder = remainder.add(modulus);
        }
        remainder
    }

    pub(super) fn abs(&self) -> Self {
        if self.is_zero() {
            Self::zero()
        } else {
            Self {
                sign: Sign::Positive,
                limbs: self.limbs.clone(),
            }
        }
    }

    #[inline(always)]
    pub(super) fn shifted_low_word(&self, shift_bits: u64) -> u64 {
        debug_assert!(!self.is_negative());
        let digit_index = usize::try_from(shift_bits / 64).unwrap_or(usize::MAX);
        let offset = (shift_bits % 64) as u32;
        let low = self.limbs.get(digit_index).copied().unwrap_or(0);
        if offset == 0 {
            return low;
        }
        let high = self.limbs.get(digit_index + 1).copied().unwrap_or(0);
        (low >> offset) | (high << (64 - offset))
    }

    pub(super) fn extended_gcd(&self, other: &Self) -> ExtendedGcd {
        let mut scratch = LimbScratch::default();
        self.extended_gcd_with_scratch(other, &mut scratch)
    }

    pub(super) fn extended_gcd_with_scratch(
        &self,
        other: &Self,
        scratch: &mut LimbScratch,
    ) -> ExtendedGcd {
        let mut old_r = self.clone();
        let mut r = other.clone();
        let mut old_s = Self::one();
        let mut s = Self::zero();
        let mut old_t = Self::zero();
        let mut t = Self::one();
        let mut next_old_r = Self::zero();
        let mut next_r = Self::zero();
        let mut next_old_s = Self::zero();
        let mut next_s = Self::zero();
        let mut next_old_t = Self::zero();
        let mut next_t = Self::zero();

        while !r.is_zero() {
            if old_r.is_one() {
                break;
            }
            if r.is_one() {
                old_r = r;
                old_s = s;
                old_t = t;
                break;
            }

            if !old_r.is_negative() && !r.is_negative() && old_r.abs_cmp(&r) == Ordering::Less {
                std::mem::swap(&mut old_r, &mut r);
                std::mem::swap(&mut old_s, &mut s);
                std::mem::swap(&mut old_t, &mut t);
                continue;
            }

            if !old_r.is_negative() && !r.is_negative() {
                let bits = old_r.bit_len().saturating_sub(63);
                let mut rr2 = old_r.shifted_low_word(bits);
                let mut rr1 = r.shifted_low_word(bits);

                let mut aa2 = 0_i128;
                let mut aa1 = 1_i128;
                let mut bb2 = 1_i128;
                let mut bb1 = 0_i128;
                let mut steps = 0_u32;

                while rr1 != 0 {
                    let q = rr2 / rr1;
                    if q == 0 {
                        break;
                    }
                    let next_r = rr2 - q * rr1;
                    let q = i128::from(q);
                    let next_a = aa2 - q * aa1;
                    let next_b = bb2 - q * bb1;
                    let next_r_i = i128::from(next_r);
                    let rr1_minus_next_r = i128::from(rr1 - next_r);

                    if steps & 1 == 1 {
                        if next_r_i < -next_b || rr1_minus_next_r < next_a - aa1 {
                            break;
                        }
                    } else if next_r_i < -next_a || rr1_minus_next_r < next_b - bb1 {
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

                if steps != 0 {
                    Self::linear_combination_i128_into(
                        &old_r,
                        bb2,
                        &r,
                        aa2,
                        &mut next_old_r,
                        scratch,
                    );
                    Self::linear_combination_i128_into(&r, aa1, &old_r, bb1, &mut next_r, scratch);
                    Self::linear_combination_i128_into(
                        &old_s,
                        bb2,
                        &s,
                        aa2,
                        &mut next_old_s,
                        scratch,
                    );
                    Self::linear_combination_i128_into(&s, aa1, &old_s, bb1, &mut next_s, scratch);
                    Self::linear_combination_i128_into(
                        &old_t,
                        bb2,
                        &t,
                        aa2,
                        &mut next_old_t,
                        scratch,
                    );
                    Self::linear_combination_i128_into(&t, aa1, &old_t, bb1, &mut next_t, scratch);

                    std::mem::swap(&mut old_r, &mut next_old_r);
                    std::mem::swap(&mut r, &mut next_r);
                    std::mem::swap(&mut old_s, &mut next_old_s);
                    std::mem::swap(&mut s, &mut next_s);
                    std::mem::swap(&mut old_t, &mut next_old_t);
                    std::mem::swap(&mut t, &mut next_t);

                    if old_r.is_negative() {
                        old_r = old_r.negated();
                        old_s = old_s.negated();
                        old_t = old_t.negated();
                    }
                    if r.is_negative() {
                        r = r.negated();
                        s = s.negated();
                        t = t.negated();
                    }
                    continue;
                }
            }

            let (quotient, next_r) = old_r.div_rem_with_scratch(&r, scratch);
            old_r = r;
            r = next_r;

            quotient.mul_into(&s, &mut next_s);
            let mut fallback_next_s = std::mem::take(&mut old_s);
            fallback_next_s.sub_assign(&next_s);
            old_s = s;
            s = fallback_next_s;

            quotient.mul_into(&t, &mut next_t);
            let mut fallback_next_t = std::mem::take(&mut old_t);
            fallback_next_t.sub_assign(&next_t);
            old_t = t;
            t = fallback_next_t;
        }

        if old_r.is_negative() {
            old_r = old_r.negated();
            old_s = old_s.negated();
            old_t = old_t.negated();
        }

        ExtendedGcd {
            x: old_s,
            y: old_t,
            gcd: old_r,
        }
    }

    pub(super) fn left_extended_gcd_with_scratch(
        &self,
        other: &Self,
        scratch: &mut LimbScratch,
    ) -> LeftExtendedGcd {
        let mut old_r = self.clone();
        let mut r = other.clone();
        let mut old_s = Self::one();
        let mut s = Self::zero();
        let mut next_old_r = Self::zero();
        let mut next_r = Self::zero();
        let mut next_old_s = Self::zero();
        let mut next_s = Self::zero();

        while !r.is_zero() {
            if old_r.is_one() {
                break;
            }
            if r.is_one() {
                old_r = r;
                old_s = s;
                break;
            }

            if !old_r.is_negative() && !r.is_negative() && old_r.abs_cmp(&r) == Ordering::Less {
                std::mem::swap(&mut old_r, &mut r);
                std::mem::swap(&mut old_s, &mut s);
                continue;
            }

            if !old_r.is_negative() && !r.is_negative() {
                let bits = old_r.bit_len().saturating_sub(63);
                let mut rr2 = old_r.shifted_low_word(bits);
                let mut rr1 = r.shifted_low_word(bits);

                let mut aa2 = 0_i128;
                let mut aa1 = 1_i128;
                let mut bb2 = 1_i128;
                let mut bb1 = 0_i128;
                let mut steps = 0_u32;

                while rr1 != 0 {
                    let q = rr2 / rr1;
                    if q == 0 {
                        break;
                    }
                    let next_r = rr2 - q * rr1;
                    let q = i128::from(q);
                    let next_a = aa2 - q * aa1;
                    let next_b = bb2 - q * bb1;
                    let next_r_i = i128::from(next_r);
                    let rr1_minus_next_r = i128::from(rr1 - next_r);

                    if steps & 1 == 1 {
                        if next_r_i < -next_b || rr1_minus_next_r < next_a - aa1 {
                            break;
                        }
                    } else if next_r_i < -next_a || rr1_minus_next_r < next_b - bb1 {
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

                if steps != 0 {
                    Self::linear_combination_i128_into(
                        &old_r,
                        bb2,
                        &r,
                        aa2,
                        &mut next_old_r,
                        scratch,
                    );
                    Self::linear_combination_i128_into(&r, aa1, &old_r, bb1, &mut next_r, scratch);
                    Self::linear_combination_i128_into(
                        &old_s,
                        bb2,
                        &s,
                        aa2,
                        &mut next_old_s,
                        scratch,
                    );
                    Self::linear_combination_i128_into(&s, aa1, &old_s, bb1, &mut next_s, scratch);

                    std::mem::swap(&mut old_r, &mut next_old_r);
                    std::mem::swap(&mut r, &mut next_r);
                    std::mem::swap(&mut old_s, &mut next_old_s);
                    std::mem::swap(&mut s, &mut next_s);

                    if old_r.is_negative() {
                        old_r = old_r.negated();
                        old_s = old_s.negated();
                    }
                    if r.is_negative() {
                        r = r.negated();
                        s = s.negated();
                    }
                    continue;
                }
            }

            let (quotient, next_r) = old_r.div_rem_with_scratch(&r, scratch);
            old_r = r;
            r = next_r;

            quotient.mul_into(&s, &mut next_s);
            let mut fallback_next_s = std::mem::take(&mut old_s);
            fallback_next_s.sub_assign(&next_s);
            old_s = s;
            s = fallback_next_s;
        }

        if old_r.is_negative() {
            old_r = old_r.negated();
            old_s = old_s.negated();
        }

        LeftExtendedGcd {
            x: old_s,
            gcd: old_r,
        }
    }

    pub(super) fn left_extended_gcd_positive_with_scratch(
        &self,
        other: &Self,
        scratch: &mut LimbScratch,
    ) -> LeftExtendedGcd {
        debug_assert!(!self.is_negative());
        debug_assert!(!other.is_negative());

        let mut old_r;
        let mut r;
        let mut old_s;
        let mut s;
        if self.abs_cmp(other) == Ordering::Less {
            old_r = other.clone();
            r = self.clone();
            old_s = Self::zero();
            s = Self::one();
        } else {
            old_r = self.clone();
            r = other.clone();
            old_s = Self::one();
            s = Self::zero();
        }
        let mut next_old_r = Self::zero();
        let mut next_r = Self::zero();
        let mut next_old_s = Self::zero();
        let mut next_s = Self::zero();

        while !r.is_zero() {
            if old_r.is_one() {
                break;
            }
            if r.is_one() {
                old_r = r;
                old_s = s;
                break;
            }

            let bits = old_r.bit_len().saturating_sub(63);
            let mut rr2 = old_r.shifted_low_word(bits);
            let mut rr1 = r.shifted_low_word(bits);

            let mut aa2 = 0_i128;
            let mut aa1 = 1_i128;
            let mut bb2 = 1_i128;
            let mut bb1 = 0_i128;
            let mut steps = 0_u32;

            while rr1 != 0 {
                let q = rr2 / rr1;
                if q == 0 {
                    break;
                }
                let next_r = rr2 - q * rr1;
                let q = i128::from(q);
                let next_a = aa2 - q * aa1;
                let next_b = bb2 - q * bb1;
                let next_r_i = i128::from(next_r);
                let rr1_minus_next_r = i128::from(rr1 - next_r);

                if steps & 1 == 1 {
                    if next_r_i < -next_b || rr1_minus_next_r < next_a - aa1 {
                        break;
                    }
                } else if next_r_i < -next_a || rr1_minus_next_r < next_b - bb1 {
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

            if steps != 0 {
                Self::linear_combination_i128_into(&old_r, bb2, &r, aa2, &mut next_old_r, scratch);
                Self::linear_combination_i128_into(&r, aa1, &old_r, bb1, &mut next_r, scratch);
                Self::linear_combination_i128_into(&old_s, bb2, &s, aa2, &mut next_old_s, scratch);
                Self::linear_combination_i128_into(&s, aa1, &old_s, bb1, &mut next_s, scratch);

                std::mem::swap(&mut old_r, &mut next_old_r);
                std::mem::swap(&mut r, &mut next_r);
                std::mem::swap(&mut old_s, &mut next_old_s);
                std::mem::swap(&mut s, &mut next_s);

                if old_r.is_negative() {
                    old_r = old_r.negated();
                    old_s = old_s.negated();
                }
                if r.is_negative() {
                    r = r.negated();
                    s = s.negated();
                }
                continue;
            }

            let (quotient, next_r) = old_r.div_rem_with_scratch(&r, scratch);
            old_r = r;
            r = next_r;

            quotient.mul_into(&s, &mut next_s);
            let mut fallback_next_s = std::mem::take(&mut old_s);
            fallback_next_s.sub_assign(&next_s);
            old_s = s;
            s = fallback_next_s;
        }

        if old_r.is_negative() {
            old_r = old_r.negated();
            old_s = old_s.negated();
        }

        LeftExtendedGcd {
            x: old_s,
            gcd: old_r,
        }
    }

    fn from_parts(sign: Sign, mut limbs: LimbVec) -> Self {
        trim_leading_zero_limbs(&mut limbs);
        let sign = if limbs.is_empty() { Sign::Zero } else { sign };
        Self { sign, limbs }
    }

    fn abs_biguint(&self) -> num_bigint::BigUint {
        let mut bytes = Vec::with_capacity(self.limbs.len() * 8);
        for limb in &self.limbs {
            bytes.extend_from_slice(&limb.to_le_bytes());
        }
        num_bigint::BigUint::from_bytes_le(&bytes)
    }
}

pub(super) fn xgcd_partial(
    r2: &LimbInt,
    r1: &LimbInt,
    threshold: &LimbInt,
) -> (LimbInt, LimbInt, LimbInt, LimbInt) {
    let mut scratch = LimbScratch::default();
    xgcd_partial_with_scratch(r2, r1, threshold, &mut scratch)
}

pub(super) fn xgcd_partial_with_scratch(
    r2: &LimbInt,
    r1: &LimbInt,
    threshold: &LimbInt,
    scratch: &mut LimbScratch,
) -> (LimbInt, LimbInt, LimbInt, LimbInt) {
    let mut r2 = r2.clone();
    let mut r1 = r1.clone();
    let mut co2 = LimbInt::zero();
    let mut co1 = LimbInt::from_i128(-1);
    let mut next_r2 = LimbInt::zero();
    let mut next_r1 = LimbInt::zero();
    let mut next_co2 = LimbInt::zero();
    let mut next_co1 = LimbInt::zero();

    while !r1.is_zero() && r1.cmp(threshold) == Ordering::Greater {
        let bits = r2.bit_len().saturating_sub(63);
        let mut rr2 = r2.shifted_low_word(bits);
        let mut rr1 = r1.shifted_low_word(bits);
        let threshold_word = threshold.shifted_low_word(bits);

        let mut aa2 = 0_i128;
        let mut aa1 = 1_i128;
        let mut bb2 = 1_i128;
        let mut bb1 = 0_i128;
        let mut steps = 0_u32;

        while rr1 != 0 && rr1 > threshold_word {
            let q = rr2 / rr1;
            if q == 0 {
                break;
            }
            let next_r = rr2 - q * rr1;
            let q = i128::from(q);
            let next_a = aa2 - q * aa1;
            let next_b = bb2 - q * bb1;
            let next_r_i = i128::from(next_r);
            let rr1_minus_next_r = i128::from(rr1 - next_r);

            if steps & 1 == 1 {
                if next_r_i < -next_b || rr1_minus_next_r < next_a - aa1 {
                    break;
                }
            } else if next_r_i < -next_a || rr1_minus_next_r < next_b - bb1 {
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
            let (q, next_r) = r2.div_rem_with_scratch(&r1, scratch);
            q.mul_into(&co1, &mut next_co1);
            let mut next_co = std::mem::take(&mut co2);
            next_co.sub_assign(&next_co1);
            r2 = r1;
            r1 = next_r;
            co2 = co1;
            co1 = next_co;
        } else {
            LimbInt::linear_combination_i128_into(&r2, bb2, &r1, aa2, &mut next_r2, scratch);
            LimbInt::linear_combination_i128_into(&r1, aa1, &r2, bb1, &mut next_r1, scratch);
            LimbInt::linear_combination_i128_into(&co2, bb2, &co1, aa2, &mut next_co2, scratch);
            LimbInt::linear_combination_i128_into(&co1, aa1, &co2, bb1, &mut next_co1, scratch);

            std::mem::swap(&mut r2, &mut next_r2);
            std::mem::swap(&mut r1, &mut next_r1);
            std::mem::swap(&mut co2, &mut next_co2);
            std::mem::swap(&mut co1, &mut next_co1);

            if r1.is_negative() {
                r1 = r1.negated();
                co1 = co1.negated();
            }
            if r2.is_negative() {
                r2 = r2.negated();
                co2 = co2.negated();
            }
        }
    }

    if r2.is_negative() {
        r2 = r2.negated();
        co2 = co2.negated();
        co1 = co1.negated();
    }

    (co2, co1, r2, r1)
}

fn subtract_signed_abs(
    positive_limbs: &[u64],
    positive_sign: Sign,
    negative_limbs: &[u64],
    negative_sign: Sign,
) -> LimbInt {
    match cmp_abs_limbs(positive_limbs, negative_limbs) {
        Ordering::Greater => {
            LimbInt::from_parts(positive_sign, sub_abs_limbs(positive_limbs, negative_limbs))
        }
        Ordering::Less => {
            LimbInt::from_parts(negative_sign, sub_abs_limbs(negative_limbs, positive_limbs))
        }
        Ordering::Equal => LimbInt::zero(),
    }
}

fn signed_scalar_sign(value_sign: Sign, scalar: i128) -> Sign {
    if scalar == 0 || value_sign == Sign::Zero {
        Sign::Zero
    } else if scalar < 0 {
        match value_sign {
            Sign::Negative => Sign::Positive,
            Sign::Zero => Sign::Zero,
            Sign::Positive => Sign::Negative,
        }
    } else {
        value_sign
    }
}

#[inline(always)]
fn combine_signed_abs_limbs_into(
    left_sign: Sign,
    left: &[u64],
    right_sign: Sign,
    right: &[u64],
    output: &mut LimbVec,
) -> Sign {
    output.clear();
    match (left_sign, right_sign) {
        (Sign::Zero, Sign::Zero) => Sign::Zero,
        (Sign::Zero, _) => {
            output.extend_from_slice(right);
            right_sign
        }
        (_, Sign::Zero) => {
            output.extend_from_slice(left);
            left_sign
        }
        (Sign::Positive, Sign::Positive) | (Sign::Negative, Sign::Negative) => {
            add_abs_limbs_into(left, right, output);
            left_sign
        }
        (Sign::Positive, Sign::Negative) | (Sign::Negative, Sign::Positive) => {
            match cmp_abs_limbs(left, right) {
                Ordering::Greater => {
                    sub_abs_limbs_into(left, right, output);
                    left_sign
                }
                Ordering::Less => {
                    sub_abs_limbs_into(right, left, output);
                    right_sign
                }
                Ordering::Equal => Sign::Zero,
            }
        }
    }
}

#[inline(always)]
fn cmp_abs_limbs(left: &[u64], right: &[u64]) -> Ordering {
    match left.len().cmp(&right.len()) {
        Ordering::Equal => left.iter().rev().cmp(right.iter().rev()),
        other => other,
    }
}

fn cmp_abs_to_double(left: &[u64], right: &[u64]) -> Ordering {
    debug_assert!(!right.is_empty());
    let doubled_len = right.len() + usize::from(right.last().copied().unwrap_or(0) >> 63 != 0);
    match left.len().cmp(&doubled_len) {
        Ordering::Equal => {}
        other => return other,
    }

    for index in (0..doubled_len).rev() {
        let doubled_limb = shifted_left_one_limb_unchecked(right, index);
        match left[index].cmp(&doubled_limb) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    Ordering::Equal
}

fn shifted_left_one_limb_unchecked(limbs: &[u64], index: usize) -> u64 {
    debug_assert!(index <= limbs.len());
    let low = if index < limbs.len() {
        limbs[index] << 1
    } else {
        0
    };
    let carry = if index == 0 {
        0
    } else {
        limbs[index - 1] >> 63
    };
    low | carry
}

fn add_abs_limbs(left: &[u64], right: &[u64]) -> LimbVec {
    let mut output = LimbVec::new();
    add_abs_limbs_into(left, right, &mut output);
    output
}

#[inline(always)]
#[allow(clippy::uninit_vec)]
fn add_abs_limbs_into(left: &[u64], right: &[u64], output: &mut LimbVec) {
    output.clear();
    let len = left.len().max(right.len());
    output.reserve(len + 1);
    // SAFETY: u64 has no drop glue, reserve guarantees room for len + 1 limbs,
    // and the loops initialize every slot in 0..len before the vector is read.
    unsafe {
        output.set_len(len);
    }
    let mut carry = 0_u64;
    let shared_len = left.len().min(right.len());

    for index in 0..shared_len {
        let (sum, carry_a) = left[index].overflowing_add(right[index]);
        let (sum, carry_b) = sum.overflowing_add(carry);
        output[index] = sum;
        carry = u64::from(carry_a || carry_b);
    }

    let remaining = if left.len() > right.len() {
        &left[shared_len..]
    } else {
        &right[shared_len..]
    };
    for (offset, limb) in remaining.iter().copied().enumerate() {
        let (sum, next_carry) = limb.overflowing_add(carry);
        output[shared_len + offset] = sum;
        carry = u64::from(next_carry);
    }
    if carry != 0 {
        output.push(carry);
    }
}

fn add_abs_limbs_assign(left: &mut LimbVec, right: &[u64]) {
    let len = left.len().max(right.len());
    let original_left_len = left.len();
    left.resize(len, 0);
    let mut carry = 0_u64;
    let shared_len = original_left_len.min(right.len());

    for index in 0..shared_len {
        let (sum, carry_a) = left[index].overflowing_add(right[index]);
        let (sum, carry_b) = sum.overflowing_add(carry);
        left[index] = sum;
        carry = u64::from(carry_a || carry_b);
    }

    if right.len() > original_left_len {
        for index in shared_len..right.len() {
            let (sum, next_carry) = right[index].overflowing_add(carry);
            left[index] = sum;
            carry = u64::from(next_carry);
        }
    } else {
        for left_limb in left.iter_mut().take(original_left_len).skip(shared_len) {
            let (sum, next_carry) = left_limb.overflowing_add(carry);
            *left_limb = sum;
            carry = u64::from(next_carry);
        }
    }

    if carry != 0 {
        left.push(carry);
    }
}

fn sub_abs_limbs(left: &[u64], right: &[u64]) -> LimbVec {
    let mut output = LimbVec::new();
    sub_abs_limbs_into(left, right, &mut output);
    output
}

#[inline(always)]
#[allow(clippy::uninit_vec)]
fn sub_abs_limbs_into(left: &[u64], right: &[u64], output: &mut LimbVec) {
    debug_assert!(cmp_abs_limbs(left, right) != Ordering::Less);
    output.clear();
    output.reserve(left.len());
    // SAFETY: u64 has no drop glue, reserve guarantees room for left.len()
    // limbs, and the loops initialize every slot before trimming reads them.
    unsafe {
        output.set_len(left.len());
    }
    let mut borrow = 0_u64;
    let shared_len = right.len();

    for index in 0..shared_len {
        let (difference, borrow_a) = left[index].overflowing_sub(right[index]);
        let (difference, borrow_b) = difference.overflowing_sub(borrow);
        output[index] = difference;
        borrow = u64::from(borrow_a || borrow_b);
    }

    for (offset, left_limb) in left[shared_len..].iter().copied().enumerate() {
        let (difference, next_borrow) = left_limb.overflowing_sub(borrow);
        output[shared_len + offset] = difference;
        borrow = u64::from(next_borrow);
    }

    debug_assert_eq!(borrow, 0);
    trim_leading_zero_limbs(output);
}

fn sub_abs_limbs_assign(left: &mut LimbVec, right: &[u64]) {
    debug_assert!(cmp_abs_limbs(left, right) != Ordering::Less);
    let mut borrow = 0_u64;
    let shared_len = right.len();

    for index in 0..shared_len {
        let (difference, borrow_a) = left[index].overflowing_sub(right[index]);
        let (difference, borrow_b) = difference.overflowing_sub(borrow);
        left[index] = difference;
        borrow = u64::from(borrow_a || borrow_b);
    }

    let mut index = shared_len;
    while borrow != 0 && index < left.len() {
        let (difference, next_borrow) = left[index].overflowing_sub(borrow);
        left[index] = difference;
        borrow = u64::from(next_borrow);
        index += 1;
    }

    debug_assert_eq!(borrow, 0);
    trim_leading_zero_limbs(left);
}

fn mul_abs_limbs(left: &[u64], right: &[u64]) -> LimbVec {
    let mut output = LimbVec::new();
    mul_abs_limbs_into(left, right, &mut output);
    output
}

fn mul_abs_limbs_into(left: &[u64], right: &[u64], output: &mut LimbVec) {
    output.clear();
    if left.is_empty() || right.is_empty() {
        return;
    }

    let (outer, inner) = if left.len() <= right.len() {
        (left, right)
    } else {
        (right, left)
    };

    output.resize(outer.len() + inner.len(), 0);
    for (left_index, left_limb) in outer.iter().copied().enumerate() {
        let mut carry = 0_u128;
        for (right_index, right_limb) in inner.iter().copied().enumerate() {
            let output_index = left_index + right_index;
            let product = u128::from(left_limb) * u128::from(right_limb)
                + u128::from(output[output_index])
                + carry;
            output[output_index] = product as u64;
            carry = product >> 64;
        }

        let mut output_index = left_index + inner.len();
        while carry != 0 {
            let sum = u128::from(output[output_index]) + carry;
            output[output_index] = sum as u64;
            carry = sum >> 64;
            output_index += 1;
        }
    }

    trim_leading_zero_limbs(output);
}

fn mul_abs_one_limb(value: &[u64], scalar: u64) -> LimbVec {
    let mut output = LimbVec::new();
    mul_abs_one_limb_into(value, scalar, &mut output);
    output
}

#[inline(always)]
#[allow(clippy::uninit_vec)]
fn mul_abs_one_limb_into(value: &[u64], scalar: u64, output: &mut LimbVec) {
    output.clear();
    if value.is_empty() || scalar == 0 {
        return;
    }
    if scalar == 1 {
        output.extend_from_slice(value);
        return;
    }

    output.reserve(value.len() + 1);
    let mut carry = 0_u128;
    // SAFETY: u64 has no drop glue, reserve guarantees room for value.len() + 1
    // limbs, and the loop initializes every slot before the vector is read.
    unsafe {
        output.set_len(value.len() + 1);
    }
    for (index, limb) in value.iter().copied().enumerate() {
        let product = u128::from(limb) * u128::from(scalar) + carry;
        output[index] = product as u64;
        carry = product >> 64;
    }
    if carry != 0 {
        output[value.len()] = carry as u64;
    } else {
        output.truncate(value.len());
    }
}

fn square_abs_limbs(value: &[u64]) -> LimbVec {
    mul_abs_limbs(value, value)
}

fn mul_abs_small(value: &[u64], scalar: u128) -> LimbVec {
    let mut output = LimbVec::new();
    mul_abs_small_into(value, scalar, &mut output);
    output
}

#[inline(always)]
fn mul_abs_small_into(value: &[u64], scalar: u128, output: &mut LimbVec) {
    output.clear();
    if value.is_empty() || scalar == 0 {
        return;
    }
    if scalar == 1 {
        output.extend_from_slice(value);
        return;
    }
    let scalar_low = scalar as u64;
    let scalar_high = (scalar >> 64) as u64;
    if scalar_high == 0 {
        mul_abs_one_limb_into(value, scalar_low, output);
        return;
    }

    output.resize(value.len() + 2, 0);

    if scalar_low != 0 {
        for (index, limb) in value.iter().copied().enumerate() {
            add_u128_at(output, index, u128::from(limb) * u128::from(scalar_low));
        }
    }
    if scalar_high != 0 {
        for (index, limb) in value.iter().copied().enumerate() {
            add_u128_at(
                output,
                index + 1,
                u128::from(limb) * u128::from(scalar_high),
            );
        }
    }

    trim_leading_zero_limbs(output);
}

fn add_u128_at(output: &mut LimbVec, index: usize, value: u128) {
    let low = value as u64;
    let high = (value >> 64) as u64;
    ensure_len(output, index + 2);

    let (sum_low, carry_low) = output[index].overflowing_add(low);
    output[index] = sum_low;

    let (sum_high, carry_high_a) = output[index + 1].overflowing_add(high);
    let (sum_high, carry_high_b) = sum_high.overflowing_add(u64::from(carry_low));
    output[index + 1] = sum_high;

    let mut carry = u64::from(carry_high_a) + u64::from(carry_high_b);
    let mut carry_index = index + 2;
    while carry != 0 {
        ensure_len(output, carry_index + 1);
        let (sum, overflowed) = output[carry_index].overflowing_add(carry);
        output[carry_index] = sum;
        carry = u64::from(overflowed);
        carry_index += 1;
    }
}

fn ensure_len(output: &mut LimbVec, len: usize) {
    if output.len() < len {
        output.resize(len, 0);
    }
}

fn div_rem_abs_limbs(numerator: &[u64], denominator: &[u64]) -> (LimbVec, LimbVec) {
    let mut scratch = DivisionScratch::default();
    div_rem_abs_limbs_with_scratch(numerator, denominator, &mut scratch)
}

fn div_rem_abs_limbs_with_scratch(
    numerator: &[u64],
    denominator: &[u64],
    scratch: &mut DivisionScratch,
) -> (LimbVec, LimbVec) {
    div_rem_abs_limbs_into_scratch(numerator, denominator, scratch, true, true);
    (scratch.quotient.clone(), scratch.remainder.clone())
}

fn div_abs_limbs_with_scratch(
    numerator: &[u64],
    denominator: &[u64],
    scratch: &mut DivisionScratch,
) -> LimbVec {
    div_rem_abs_limbs_into_scratch(numerator, denominator, scratch, true, false);
    scratch.quotient.clone()
}

fn rem_abs_limbs_with_scratch(
    numerator: &[u64],
    denominator: &[u64],
    scratch: &mut DivisionScratch,
) -> LimbVec {
    div_rem_abs_limbs_into_scratch(numerator, denominator, scratch, false, true);
    scratch.remainder.clone()
}

#[inline(always)]
fn div_rem_abs_limbs_into_scratch(
    numerator: &[u64],
    denominator: &[u64],
    scratch: &mut DivisionScratch,
    keep_quotient: bool,
    keep_remainder: bool,
) {
    debug_assert!(!denominator.is_empty());
    scratch.quotient.clear();
    scratch.remainder.clear();

    if numerator.is_empty() {
        return;
    }
    if cmp_abs_limbs(numerator, denominator) == Ordering::Less {
        if keep_remainder {
            scratch.remainder.extend_from_slice(numerator);
        }
        return;
    }
    if denominator == [1] {
        if keep_quotient {
            scratch.quotient.extend_from_slice(numerator);
        }
        return;
    }
    if denominator.len() == 1 {
        div_rem_abs_one_limb_into(
            numerator,
            denominator[0],
            &mut scratch.quotient,
            &mut scratch.remainder,
            keep_quotient,
            keep_remainder,
        );
        return;
    }

    let shift = denominator.last().copied().unwrap_or(0).leading_zeros() as usize;
    shl_abs_limbs_into(numerator, shift, &mut scratch.normalized_numerator);
    shl_abs_limbs_into(denominator, shift, &mut scratch.normalized_denominator);
    scratch.normalized_numerator.push(0);

    let denominator_len = scratch.normalized_denominator.len();
    let quotient_len = scratch.normalized_numerator.len() - denominator_len;
    if keep_quotient {
        scratch.quotient.resize(quotient_len, 0);
    }

    for quotient_index in (0..quotient_len).rev() {
        let (mut qhat, mut rhat, mut rhat_overflowed) = estimate_quotient(
            scratch.normalized_numerator[quotient_index + denominator_len],
            scratch.normalized_numerator[quotient_index + denominator_len - 1],
            scratch.normalized_denominator[denominator_len - 1],
        );

        if denominator_len > 1 {
            let next_denominator_limb = scratch.normalized_denominator[denominator_len - 2];
            let next_numerator_limb =
                scratch.normalized_numerator[quotient_index + denominator_len - 2];
            while !rhat_overflowed
                && quotient_too_large(qhat, rhat, next_denominator_limb, next_numerator_limb)
            {
                qhat -= 1;
                let (next_rhat, overflowed) =
                    rhat.overflowing_add(scratch.normalized_denominator[denominator_len - 1]);
                rhat = next_rhat;
                rhat_overflowed = overflowed;
            }
        }

        if sub_mul_at(
            &mut scratch.normalized_numerator,
            &scratch.normalized_denominator,
            qhat,
            quotient_index,
        ) {
            qhat -= 1;
            add_at(
                &mut scratch.normalized_numerator,
                &scratch.normalized_denominator,
                quotient_index,
            );
        }
        if keep_quotient {
            scratch.quotient[quotient_index] = qhat;
        }
    }

    if keep_remainder {
        shr_abs_limbs_into(
            &scratch.normalized_numerator[..denominator_len],
            shift,
            &mut scratch.remainder,
        );
        trim_leading_zero_limbs(&mut scratch.remainder);
    }
    if keep_quotient {
        trim_leading_zero_limbs(&mut scratch.quotient);
    }
}

fn div_rem_abs_one_limb(numerator: &[u64], denominator: u64) -> (LimbVec, LimbVec) {
    let mut quotient = LimbVec::new();
    let mut remainder = LimbVec::new();
    div_rem_abs_one_limb_into(
        numerator,
        denominator,
        &mut quotient,
        &mut remainder,
        true,
        true,
    );
    (quotient, remainder)
}

fn div_rem_abs_one_limb_into(
    numerator: &[u64],
    denominator: u64,
    quotient: &mut LimbVec,
    remainder_output: &mut LimbVec,
    keep_quotient: bool,
    keep_remainder: bool,
) {
    debug_assert_ne!(denominator, 0);
    quotient.clear();
    remainder_output.clear();
    if keep_quotient {
        quotient.resize(numerator.len(), 0);
    }
    let mut remainder = 0_u128;

    for (index, limb) in numerator.iter().copied().enumerate().rev() {
        let value = (remainder << 64) | u128::from(limb);
        if keep_quotient {
            quotient[index] = (value / u128::from(denominator)) as u64;
        }
        remainder = value % u128::from(denominator);
    }

    if keep_quotient {
        trim_leading_zero_limbs(quotient);
    }
    if keep_remainder && remainder != 0 {
        remainder_output.push(remainder as u64);
    }
}

#[inline(always)]
fn estimate_quotient(high: u64, low: u64, denominator_high: u64) -> (u64, u64, bool) {
    if high == denominator_high {
        let (remainder, overflowed) = low.overflowing_add(denominator_high);
        (u64::MAX, remainder, overflowed)
    } else {
        let numerator = (u128::from(high) << 64) | u128::from(low);
        (
            (numerator / u128::from(denominator_high)) as u64,
            (numerator % u128::from(denominator_high)) as u64,
            false,
        )
    }
}

#[inline(always)]
fn quotient_too_large(qhat: u64, rhat: u64, denominator_next: u64, numerator_next: u64) -> bool {
    let left = u128::from(qhat) * u128::from(denominator_next);
    let right = (u128::from(rhat) << 64) | u128::from(numerator_next);
    left > right
}

#[inline(always)]
fn sub_mul_at(target: &mut [u64], value: &[u64], multiplier: u64, offset: usize) -> bool {
    if multiplier == 0 {
        return false;
    }

    let mut carry = 0_u128;
    for (index, value_limb) in value.iter().copied().enumerate() {
        let product = u128::from(multiplier) * u128::from(value_limb) + carry;
        let product_low = product as u64;
        carry = product >> 64;

        let target_index = offset + index;
        let (difference, borrowed) = target[target_index].overflowing_sub(product_low);
        target[target_index] = difference;
        carry += u128::from(borrowed);
    }

    let target_index = offset + value.len();
    let carry_low = carry as u64;
    let carry_high = carry >> 64;
    let (difference, borrowed) = target[target_index].overflowing_sub(carry_low);
    target[target_index] = difference;
    borrowed || carry_high != 0
}

#[inline(always)]
fn add_at(target: &mut [u64], value: &[u64], offset: usize) {
    let mut carry = 0_u128;
    for (index, value_limb) in value.iter().copied().enumerate() {
        let target_index = offset + index;
        let sum = u128::from(target[target_index]) + u128::from(value_limb) + carry;
        target[target_index] = sum as u64;
        carry = sum >> 64;
    }

    let mut target_index = offset + value.len();
    while carry != 0 {
        let sum = u128::from(target[target_index]) + carry;
        target[target_index] = sum as u64;
        carry = sum >> 64;
        target_index += 1;
    }
}

fn shl_abs_limbs(value: &[u64], bits: usize) -> LimbVec {
    let mut output = LimbVec::new();
    shl_abs_limbs_into(value, bits, &mut output);
    output
}

fn shl_abs_limbs_into(value: &[u64], bits: usize, output: &mut LimbVec) {
    output.clear();
    if value.is_empty() {
        return;
    }
    if bits == 0 {
        output.extend_from_slice(value);
        return;
    }

    let limb_shift = bits / 64;
    let bit_shift = (bits % 64) as u32;
    if limb_shift == 0 && bit_shift != 0 {
        shl_abs_limbs_small_into(value, bit_shift, output);
        return;
    }
    output.resize(limb_shift + value.len() + usize::from(bit_shift != 0), 0);
    let mut carry = 0_u64;

    for (index, limb) in value.iter().copied().enumerate() {
        output[index + limb_shift] = if bit_shift == 0 {
            limb
        } else {
            (limb << bit_shift) | carry
        };
        carry = if bit_shift == 0 {
            0
        } else {
            limb >> (64 - bit_shift)
        };
    }
    if bit_shift != 0 {
        output[limb_shift + value.len()] = carry;
    }

    trim_leading_zero_limbs(output);
}

#[allow(clippy::uninit_vec)]
fn shl_abs_limbs_small_into(value: &[u64], bit_shift: u32, output: &mut LimbVec) {
    debug_assert!((1..64).contains(&bit_shift));
    output.reserve(value.len() + 1);
    // SAFETY: u64 has no drop glue, reserve guarantees room for value.len() + 1
    // limbs, and the loop initializes every slot before the vector is read.
    unsafe {
        output.set_len(value.len() + 1);
    }
    let mut carry = 0_u64;
    for (index, limb) in value.iter().copied().enumerate() {
        output[index] = (limb << bit_shift) | carry;
        carry = limb >> (64 - bit_shift);
    }
    if carry != 0 {
        output[value.len()] = carry;
    } else {
        output.truncate(value.len());
    }
}

fn shl_abs_limbs_assign(value: &mut LimbVec, bits: usize) {
    if value.is_empty() || bits == 0 {
        return;
    }

    let limb_shift = bits / 64;
    let bit_shift = (bits % 64) as u32;
    if limb_shift == 0 && bit_shift != 0 {
        shl_abs_limbs_small_assign(value, bit_shift);
        return;
    }
    let original_len = value.len();
    value.resize(original_len + limb_shift + usize::from(bit_shift != 0), 0);

    for index in (0..original_len).rev() {
        let limb = value[index];
        let output_index = index + limb_shift;
        value[output_index] = limb << bit_shift;
        if bit_shift != 0 {
            value[output_index + 1] |= limb >> (64 - bit_shift);
        }
    }

    value[..limb_shift].fill(0);
    trim_leading_zero_limbs(value);
}

fn shl_abs_limbs_small_assign(value: &mut LimbVec, bit_shift: u32) {
    debug_assert!((1..64).contains(&bit_shift));
    let mut carry = 0_u64;
    for limb in value.iter_mut() {
        let current = *limb;
        *limb = (current << bit_shift) | carry;
        carry = current >> (64 - bit_shift);
    }
    if carry != 0 {
        value.push(carry);
    }
}

fn shr_abs_limbs(value: &[u64], bits: usize) -> LimbVec {
    let mut output = LimbVec::new();
    shr_abs_limbs_into(value, bits, &mut output);
    output
}

fn shr_abs_limbs_into(value: &[u64], bits: usize, output: &mut LimbVec) {
    output.clear();
    let limb_shift = bits / 64;
    if limb_shift >= value.len() {
        return;
    }

    let bit_shift = (bits % 64) as u32;
    if limb_shift == 0 && bit_shift != 0 {
        shr_abs_limbs_small_into(value, bit_shift, output);
        return;
    }
    output.reserve(value.len() - limb_shift);
    for index in limb_shift..value.len() {
        let mut limb = value[index] >> bit_shift;
        if bit_shift != 0 {
            limb |= value.get(index + 1).copied().unwrap_or(0) << (64 - bit_shift);
        }
        output.push(limb);
    }

    trim_leading_zero_limbs(output);
}

fn shr_abs_limbs_small_into(value: &[u64], bit_shift: u32, output: &mut LimbVec) {
    debug_assert!((1..64).contains(&bit_shift));
    output.reserve(value.len());
    for index in 0..value.len() {
        let limb = (value[index] >> bit_shift)
            | (value.get(index + 1).copied().unwrap_or(0) << (64 - bit_shift));
        output.push(limb);
    }
    trim_leading_zero_limbs(output);
}

fn trim_leading_zero_limbs(limbs: &mut LimbVec) {
    while limbs.last() == Some(&0) {
        limbs.pop();
    }
}

fn abs_bit_len(limbs: &[u64]) -> usize {
    let Some(last) = limbs.last() else {
        return 0;
    };
    ((limbs.len() - 1) * 64) + (u64::BITS - last.leading_zeros()) as usize
}

fn get_abs_bit(limbs: &[u64], bit: usize) -> bool {
    let limb = bit / 64;
    let offset = bit % 64;
    limbs
        .get(limb)
        .map(|value| (value & (1_u64 << offset)) != 0)
        .unwrap_or(false)
}

fn set_abs_bit(limbs: &mut LimbVec, bit: usize) {
    let limb = bit / 64;
    let offset = bit % 64;
    if limbs.len() <= limb {
        limbs.resize(limb + 1, 0);
    }
    limbs[limb] |= 1_u64 << offset;
}

#[cfg(test)]
mod tests {
    use num_bigint::BigInt;
    use num_integer::Integer;
    use num_traits::Signed;
    use proptest::prelude::*;

    use super::{LimbInt, LimbScratch};

    fn arb_bigint() -> impl Strategy<Value = BigInt> {
        proptest::collection::vec(any::<u64>(), 0..=24).prop_flat_map(|limbs| {
            any::<bool>().prop_map(move |negative| {
                let mut bytes = Vec::with_capacity(limbs.len() * 8);
                for limb in &limbs {
                    bytes.extend_from_slice(&limb.to_le_bytes());
                }
                let magnitude = num_bigint::BigUint::from_bytes_le(&bytes);
                let sign = if magnitude == num_bigint::BigUint::from(0_u8) {
                    num_bigint::Sign::NoSign
                } else if negative {
                    num_bigint::Sign::Minus
                } else {
                    num_bigint::Sign::Plus
                };
                BigInt::from_biguint(sign, magnitude)
            })
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 512, .. ProptestConfig::default() })]

        #[test]
        fn roundtrips_bigint(value in arb_bigint()) {
            prop_assert_eq!(LimbInt::from_bigint(&value).to_bigint(), value);
        }

        #[test]
        fn add_matches_bigint(left in arb_bigint(), right in arb_bigint()) {
            let actual = LimbInt::from_bigint(&left).add(&LimbInt::from_bigint(&right)).to_bigint();
            prop_assert_eq!(actual, left + right);
        }

        #[test]
        fn sub_matches_bigint(left in arb_bigint(), right in arb_bigint()) {
            let actual = LimbInt::from_bigint(&left).sub(&LimbInt::from_bigint(&right)).to_bigint();
            prop_assert_eq!(actual, left - right);
        }

        #[test]
        fn mul_matches_bigint(left in arb_bigint(), right in arb_bigint()) {
            let actual = LimbInt::from_bigint(&left).mul(&LimbInt::from_bigint(&right)).to_bigint();
            prop_assert_eq!(actual, left * right);
        }

        #[test]
        fn square_matches_bigint(value in arb_bigint()) {
            let actual = LimbInt::from_bigint(&value).square().to_bigint();
            prop_assert_eq!(actual, &value * &value);
        }

        #[test]
        fn shifts_match_positive_bigint(value in arb_bigint(), bits in 0_usize..512) {
            let value = value.abs();
            let limbs = LimbInt::from_bigint(&value);
            prop_assert_eq!(limbs.shl_bits(bits).to_bigint(), &value << bits);
            prop_assert_eq!(limbs.shr_abs_bits(bits).to_bigint(), &value >> bits);

            let mut owned_shift = limbs.clone();
            owned_shift = owned_shift.shl_bits_owned(bits);
            prop_assert_eq!(owned_shift.to_bigint(), &value << bits);
        }

        #[test]
        fn shifted_low_word_matches_num_bigint(value in arb_bigint(), shift in 0_u64..1536) {
            let value = value.abs();
            let limbs = LimbInt::from_bigint(&value);
            let mut digits = value.iter_u64_digits();
            let digit_index = usize::try_from(shift / 64).unwrap_or(usize::MAX);
            let offset = (shift % 64) as u32;
            let low = digits.nth(digit_index).unwrap_or(0);
            let expected = if offset == 0 {
                low
            } else {
                let high = digits.next().unwrap_or(0);
                (low >> offset) | (high << (64 - offset))
            };

            prop_assert_eq!(limbs.shifted_low_word(shift), expected);
        }

        #[test]
        fn div_rem_matches_bigint(left in arb_bigint(), right in arb_bigint().prop_filter("non-zero divisor", |value| value != &BigInt::from(0))) {
            let left_limbs = LimbInt::from_bigint(&left);
            let right_limbs = LimbInt::from_bigint(&right);
            let (actual_q, actual_r) = left_limbs.div_rem(&right_limbs);
            let mut scratch = LimbScratch::default();
            let actual_div = left_limbs.div_with_scratch(&right_limbs, &mut scratch);
            let actual_floor = left_limbs.div_floor_with_scratch(&right_limbs, &mut scratch);
            let (expected_q, expected_r) = left.div_rem(&right);

            prop_assert_eq!(actual_q.to_bigint(), expected_q);
            prop_assert_eq!(actual_r.to_bigint(), expected_r);
            prop_assert_eq!(actual_div.to_bigint(), actual_q.to_bigint());
            prop_assert_eq!(actual_floor.to_bigint(), left.div_floor(&right));
        }

        #[test]
        fn extended_gcd_matches_bezout(
            left in arb_bigint(),
            right in arb_bigint().prop_filter("not both zero", |right| right != &BigInt::from(0)),
        ) {
            let left_limbs = LimbInt::from_bigint(&left);
            let right_limbs = LimbInt::from_bigint(&right);
            let actual = left_limbs.extended_gcd(&right_limbs);
            let actual_x = actual.x.to_bigint();
            let actual_y = actual.y.to_bigint();
            let actual_gcd = actual.gcd.to_bigint();

            prop_assert_eq!(&left * actual_x + &right * actual_y, actual_gcd.clone());
            prop_assert_eq!(actual_gcd.clone(), left.gcd(&right));
        }

        #[test]
        fn left_extended_gcd_matches_modular_bezout(
            left in arb_bigint(),
            right in arb_bigint().prop_filter("non-zero modulus", |value| value != &BigInt::from(0)),
        ) {
            let left_limbs = LimbInt::from_bigint(&left);
            let right_limbs = LimbInt::from_bigint(&right);
            let mut scratch = LimbScratch::default();
            let actual = left_limbs.left_extended_gcd_with_scratch(&right_limbs, &mut scratch);
            let actual_x = actual.x.to_bigint();
            let actual_gcd = actual.gcd.to_bigint();

            prop_assert_eq!(actual_gcd.clone(), left.gcd(&right));
            prop_assert_eq!((&left * actual_x - actual_gcd) % &right, BigInt::from(0));
        }

        #[test]
        fn mod_positive_matches_corrected_bigint(value in arb_bigint(), modulus in arb_bigint().prop_filter("positive modulus", |value| value > &BigInt::from(0))) {
            let value_limbs = LimbInt::from_bigint(&value);
            let modulus_limbs = LimbInt::from_bigint(&modulus);
            let mut expected = value % &modulus;
            if expected.is_negative() {
                expected += &modulus;
            }

            prop_assert_eq!(value_limbs.mod_positive(&modulus_limbs).to_bigint(), expected);
        }
    }

    #[test]
    fn from_i128_handles_i128_min() {
        assert_eq!(
            LimbInt::from_i128(i128::MIN).to_bigint(),
            BigInt::from(i128::MIN)
        );
    }

    #[test]
    fn xgcd_partial_matches_bigint_reference() {
        let mut state = 0x811c_9dc5_0123_4567_u64;

        for case in 0..128 {
            let mut r2 = next_positive_bigint(&mut state, 6);
            let mut r1 = next_positive_bigint(&mut state, 6);
            if r2 < r1 {
                std::mem::swap(&mut r2, &mut r1);
            }

            let threshold_seed = next_positive_bigint(&mut state, 3);
            let threshold = (threshold_seed % &r1).max(BigInt::from(1));

            let expected = crate::domain::vdf::arithmetic::xgcd_partial(&r2, &r1, &threshold);
            let actual = super::xgcd_partial(
                &LimbInt::from_bigint(&r2),
                &LimbInt::from_bigint(&r1),
                &LimbInt::from_bigint(&threshold),
            );

            assert_eq!(actual.0.to_bigint(), expected.0, "case={case} co2");
            assert_eq!(actual.1.to_bigint(), expected.1, "case={case} co1");
            assert_eq!(actual.2.to_bigint(), expected.2, "case={case} r2");
            assert_eq!(actual.3.to_bigint(), expected.3, "case={case} r1");
        }
    }

    fn next_positive_bigint(state: &mut u64, max_limbs: usize) -> BigInt {
        let limb_count = (next_u64(state) as usize % max_limbs) + 1;
        let mut bytes = Vec::with_capacity(limb_count * 8);
        for _ in 0..limb_count {
            bytes.extend_from_slice(&next_u64(state).to_le_bytes());
        }
        let value = num_bigint::BigUint::from_bytes_le(&bytes);
        BigInt::from(value.max(num_bigint::BigUint::from(1_u8)))
    }

    fn next_u64(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        *state
    }
}
