//! 定点数 [`Fx`]。
//!
//! 采用 32 位有符号定点表示：16.16，即低 16 位为小数位。
//! 整数部分范围约 ±32767，小数精度约 1/65536 ≈ 1.5e-5。
//! 足以精确表达 Celeste 风格的亚像素运动，且无浮点不确定性，跨平台结果一致。

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

/// 16.16 定点数。
///
/// 序列化时写原始定点位（`i32`），保证跨平台确定性。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Fx(i32);

/// 1/60 的定点表示。
impl Fx {
    /// 小数位宽。
    pub const FRACTIONAL_BITS: u32 = 16;
    /// 单位 1 的表示（`1 << 16`）。
    pub const ONE: i32 = 1 << Self::FRACTIONAL_BITS;
    /// 1/60 的定点位表示。
    pub const DT_ONE_SIXTIETH: i32 = Self::ONE / 60;

    /// 从原始定点位构造（内部，用于常量）。
    #[inline]
    pub const fn from_bits(bits: i32) -> Self {
        Fx(bits)
    }

    /// 原始定点位。
    #[inline]
    pub const fn bits(self) -> i32 {
        self.0
    }

    /// 0。
    #[inline]
    pub const fn zero() -> Self {
        Fx(0)
    }

    /// 1。
    #[inline]
    pub const fn one() -> Self {
        Fx(Self::ONE)
    }

    /// 从整数构造。
    #[inline]
    pub const fn from_int(n: i32) -> Self {
        Fx(n << Self::FRACTIONAL_BITS)
    }

    /// 从两个整数构成的有理数构造（`num / denom`）。
    ///
    /// 用于精确表达如 `1 / 60` 这类值。
    #[inline]
    pub const fn from_ratio(num: i32, denom: i32) -> Self {
        Fx((num << Self::FRACTIONAL_BITS) / denom)
    }

    /// 转换为 `f32`（仅用于渲染 / 展示，仿真内禁止）。
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.0 as f32 / Self::ONE as f32
    }

    /// 转换为 `f64`。
    #[inline]
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / Self::ONE as f64
    }

    /// 从 `f32` 构造（仅用于加载非仿真的外部数据，如美术参数）。
    #[inline]
    pub fn from_f32(v: f32) -> Self {
        Fx((v * Self::ONE as f32) as i32)
    }

    /// 向下取整到整数。
    #[inline]
    pub const fn floor(self) -> i32 {
        self.0 >> Self::FRACTIONAL_BITS
    }

    /// 舍入到最近整数。
    #[inline]
    pub const fn round(self) -> i32 {
        (self.0 + (Self::ONE >> 1)) >> Self::FRACTIONAL_BITS
    }

    /// 取绝对值。
    #[inline]
    pub const fn abs(self) -> Self {
        Fx(self.0.abs())
    }

    /// 符号（-1 / 0 / 1）。
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

    /// 最大值。
    #[inline]
    pub fn max(self, other: Self) -> Self {
        Fx(self.0.max(other.0))
    }

    /// 最小值。
    #[inline]
    pub fn min(self, other: Self) -> Self {
        Fx(self.0.min(other.0))
    }

    /// 限制到 `[lo, hi]`。
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
    /// 定点乘法：`(a * b) >> 16`。
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
    /// 定点除法：`(a << 16) / b`。
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
        // 1/60 定点表示有截断误差：60 * floor(65536/60) ≈ 1 - 16/65536。
        let dt = Fx::from_bits(Fx::DT_ONE_SIXTIETH);
        let mut acc = Fx::zero();
        for _ in 0..60 {
            acc += dt;
        }
        let diff = (acc - Fx::one()).abs();
        // 截断误差 = 16 个定点最小单位。
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
