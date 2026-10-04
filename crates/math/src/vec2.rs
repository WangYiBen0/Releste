//! 二维向量 — 固定点坐标。

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

use super::Fx;

/// 定点二维向量。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Vec2 {
    pub x: Fx,
    pub y: Fx,
}

impl Vec2 {
    /// 零向量。
    pub const ZERO: Vec2 = Vec2 {
        x: Fx::zero(),
        y: Fx::zero(),
    };

    /// 单位向量 (1, 1)。
    pub const ONE: Vec2 = Vec2 {
        x: Fx::one(),
        y: Fx::one(),
    };

    /// 构造新向量。
    pub const fn new(x: Fx, y: Fx) -> Self {
        Vec2 { x, y }
    }

    /// 从 `f32` 构造（仅在加载非仿真数据时使用）。
    pub fn from_f32s(x: f32, y: f32) -> Self {
        Vec2 {
            x: Fx::from_f32(x),
            y: Fx::from_f32(y),
        }
    }

    /// 转换为 `(f32, f32)`。
    pub fn to_f32s(self) -> (f32, f32) {
        (self.x.to_f32(), self.y.to_f32())
    }

    /// 分量式最小。
    pub fn component_min(self, other: Vec2) -> Vec2 {
        Vec2 {
            x: self.x.min(other.x),
            y: self.y.min(other.y),
        }
    }

    /// 分量式最大。
    pub fn component_max(self, other: Vec2) -> Vec2 {
        Vec2 {
            x: self.x.max(other.x),
            y: self.y.max(other.y),
        }
    }

    /// 平方长度。
    pub fn len_sq(self) -> Fx {
        self.x * self.x + self.y * self.y
    }

    /// 点积。
    pub fn dot(self, other: Vec2) -> Fx {
        self.x * other.x + self.y * other.y
    }

    /// 取绝对值（每个分量）。
    pub fn abs(self) -> Vec2 {
        Vec2 {
            x: self.x.abs(),
            y: self.y.abs(),
        }
    }
}

impl Add for Vec2 {
    type Output = Vec2;
    fn add(self, rhs: Vec2) -> Vec2 {
        Vec2 {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
        }
    }
}
impl AddAssign for Vec2 {
    fn add_assign(&mut self, rhs: Vec2) {
        *self = *self + rhs;
    }
}
impl Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, rhs: Vec2) -> Vec2 {
        Vec2 {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
        }
    }
}
impl SubAssign for Vec2 {
    fn sub_assign(&mut self, rhs: Vec2) {
        *self = *self - rhs;
    }
}
impl Mul<Fx> for Vec2 {
    type Output = Vec2;
    fn mul(self, s: Fx) -> Vec2 {
        Vec2 {
            x: self.x * s,
            y: self.y * s,
        }
    }
}
impl MulAssign<Fx> for Vec2 {
    fn mul_assign(&mut self, s: Fx) {
        *self = *self * s;
    }
}
impl Div<Fx> for Vec2 {
    type Output = Vec2;
    fn div(self, s: Fx) -> Vec2 {
        Vec2 {
            x: self.x / s,
            y: self.y / s,
        }
    }
}
impl DivAssign<Fx> for Vec2 {
    fn div_assign(&mut self, s: Fx) {
        *self = *self / s;
    }
}
impl Neg for Vec2 {
    type Output = Vec2;
    fn neg(self) -> Vec2 {
        Vec2 {
            x: -self.x,
            y: -self.y,
        }
    }
}

impl fmt::Debug for Vec2 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Vec2({:.4}, {:.4})", self.x.to_f64(), self.y.to_f64())
    }
}
impl fmt::Display for Vec2 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_ops() {
        let a = Vec2::new(Fx::from_int(2), Fx::from_int(3));
        let b = Vec2::new(Fx::from_int(1), Fx::from_int(4));
        let c = a + b;
        assert_eq!(c, Vec2::new(Fx::from_int(3), Fx::from_int(7)));
        let d = a * Fx::from_int(2);
        assert_eq!(d, Vec2::new(Fx::from_int(4), Fx::from_int(6)));
    }
}
