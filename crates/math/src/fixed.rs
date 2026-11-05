//! Fixed-point number [`Fx`].
//!
//! Uses a 32-bit signed fixed-point representation: 16.16, i.e. the low 16 bits
//! are fraction bits.
//! The integer part range is about +/-32767, and the fractional precision is
//! about 1/65536 ~ 1.5e-5.
//! Enough to express Celeste-style subpixel movement precisely, with no
//! floating-point nondeterminism and identical results across platforms.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

/// 16.16 fixed-point number.
///
/// Serialized as raw fixed-point bits (`i32`) to guarantee cross-platform
/// determinism.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Fx(i32);

/// Fixed-point representation of 1/60.
impl Fx {
    /// Number of fraction bits.
    pub const FRACTIONAL_BITS: u32 = 16;
    /// Representation of the unit 1 (`1 << 16`).
    pub const ONE: i32 = 1 << Self::FRACTIONAL_BITS;
    /// Fixed-point bit representation of 1/60.
    pub const DT_ONE_SIXTIETH: i32 = Self::ONE / 60;

    /// Construct from raw fixed-point bits (internal, for constants).
    #[inline]
    pub const fn from_bits(bits: i32) -> Self {
        Fx(bits)
    }

    /// Raw fixed-point bits.
    #[inline]
    pub const fn bits(self) -> i32 {
        self.0
    }

    /// 0.
    #[inline]
    pub const fn zero() -> Self {
        Fx(0)
    }

    /// 1.
    #[inline]
    pub const fn one() -> Self {
        Fx(Self::ONE)
    }

    /// Construct from an integer.
    #[inline]
    pub const fn from_int(n: i32) -> Self {
        Fx(n << Self::FRACTIONAL_BITS)
    }

    /// Construct from a rational of two integers (`num / denom`).
    ///
    /// Used to express values such as `1 / 60` precisely.
    #[inline]
    pub const fn from_ratio(num: i32, denom: i32) -> Self {
        Fx((num << Self::FRACTIONAL_BITS) / denom)
    }

    /// Convert to `f32` (rendering / display only; forbidden inside the
    /// simulation).
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.0 as f32 / Self::ONE as f32
    }

    /// Convert to `f64`.
    #[inline]
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / Self::ONE as f64
    }

    /// Construct from `f32` (only for loading non-simulation external data such
    /// as art parameters).
    #[inline]
    pub fn from_f32(v: f32) -> Self {
        Fx((v * Self::ONE as f32) as i32)
    }

    /// Round down to an integer.
    #[inline]
    pub const fn floor(self) -> i32 {
        self.0 >> Self::FRACTIONAL_BITS
    }

    /// Round to the nearest integer.
    #[inline]
    pub const fn round(self) -> i32 {
        (self.0 + (Self::ONE >> 1)) >> Self::FRACTIONAL_BITS
    }

    /// Absolute value.
    #[inline]
    pub const fn abs(self) -> Self {
        Fx(self.0.abs())
    }

    /// Sign (-1 / 0 / 1).
    #[inline]
    pub const fn signum(self) -> i32 {
        if self.0 > 0 {
            1
        } else if self.0 < 0 {
            -1
        } else {
            0
        }
    }

    /// Maximum.
    #[inline]
    pub fn max(self, other: Self) -> Self {
        Fx(self.0.max(other.0))
    }

    /// Minimum.
    #[inline]
    pub fn min(self, other: Self) -> Self {
        Fx(self.0.min(other.0))
    }

    /// Clamp to `[lo, hi]`.
    #[inline]
    pub fn clamp(self, lo: Self, hi: Self) -> Self {
        self.max(lo).min(hi)
    }
}

impl From<i32> for Fx {
    #[inline]
    fn from(v: i32) -> Self {
        Self::from_int(v)
    }
}

impl Add for Fx {
    type Output = Fx;
    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        Fx(self.0 + rhs.0)
    }
}
impl AddAssign for Fx {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}
impl Sub for Fx {
    type Output = Fx;
    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        Fx(self.0 - rhs.0)
    }
}
impl SubAssign for Fx {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.0 -= rhs.0;
    }
}
impl Mul for Fx {
    type Output = Fx;
    /// Fixed-point multiplication: `(a * b) >> 16`.
    #[inline]
    fn mul(self, rhs: Self) -> Self::Output {
        Fx(((self.0 as i64 * rhs.0 as i64) >> Self::FRACTIONAL_BITS) as i32)
    }
}
impl MulAssign for Fx {
    #[inline]
    fn mul_assign(&mut self, rhs: Self) {
        *self = *self * rhs;
    }
}
impl Div for Fx {
    type Output = Fx;
    /// Fixed-point division: `(a << 16) / b`.
    #[inline]
    fn div(self, rhs: Self) -> Self::Output {
        debug_assert!(rhs.0 != 0, "division by zero in Fx");
        let num = (self.0 as i64) << Self::FRACTIONAL_BITS;
        let denom = rhs.0 as i64;
        Fx((num / denom) as i32)
    }
}
impl DivAssign for Fx {
    #[inline]
    fn div_assign(&mut self, rhs: Self) {
        *self = *self / rhs;
    }
}
impl Neg for Fx {
    type Output = Fx;
    #[inline]
    fn neg(self) -> Self::Output {
        Fx(-self.0)
    }
}

impl fmt::Debug for Fx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fx({:.4})", self.to_f64())
    }
}
impl fmt::Display for Fx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_int_roundtrip() {
        assert_eq!(Fx::from_int(3).to_f64(), 3.0);
        assert_eq!(Fx::from_int(-7).floor(), -7);
    }

    #[test]
    fn one_sixtieth_is_stable() {
        // The fixed-point representation of 1/60 has a truncation error:
        // 60 * floor(65536/60) ~ 1 - 16/65536.
        let dt = Fx::from_bits(Fx::DT_ONE_SIXTIETH);
        let mut acc = Fx::zero();
        for _ in 0..60 {
            acc += dt;
        }
        let diff = (acc - Fx::one()).abs();
        // Truncation error = 16 fixed-point least-significant units.
        assert!(diff <= Fx::from_bits(16));
    }

    #[test]
    fn mul_div_roundtrip() {
        let a = Fx::from_ratio(3, 2); // 1.5
        let b = Fx::from_int(4);
        let c = a * b; // 6.0
        assert_eq!(c, Fx::from_int(6));
        assert_eq!(c / a, b);
    }

    #[test]
    fn clamp_and_abs() {
        let x = Fx::from_int(-5);
        assert_eq!(x.abs(), Fx::from_int(5));
        assert_eq!(x.clamp(Fx::from_int(0), Fx::from_int(10)), Fx::from_int(0));
    }
}
