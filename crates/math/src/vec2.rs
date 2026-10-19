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
    ///
    /// **会溢出**：`Fx` 是 16.16，分量的绝对值超过约 181 时 `x*x`
    /// 就装不下 `i32` 而回绕。只用于确定量级已知的短向量（速度、
    /// 朝向）。要比较任意两点的远近请用 [`Vec2::distance_sq`]。
    pub fn len_sq(self) -> Fx {
        self.x * self.x + self.y * self.y
    }

    /// 两点之间的平方距离，用 `i64` 承载，**不会回绕**。
    ///
    /// 返回值的单位是「定点位²」：`Fx::from_bits(v)` 才是 `Fx` 单位的
    /// 平方距离。之所以不能返回 `Fx`：`Fx` 的分量上限是 32767，
    /// 意味着任何超过 181px 的点对平方距离都会被截断，而"谁更近"
    /// 这类比较在地图尺度上（数百上千像素）全靠它 —— 回绕会让远点
    /// 因为符号翻转被判成更近。
    ///
    /// 用来做最近点 / 最近房间选择，以及任何需要单调距离的场景。
    pub fn distance_sq(self, other: Vec2) -> i64 {
        let dx = (self.x.bits() as i64) - (other.x.bits() as i64);
        let dy = (self.y.bits() as i64) - (other.y.bits() as i64);
        dx * dx + dy * dy
    }

    /// 点积。
    pub fn dot(self, other: Vec2) -> Fx {
        self.x * other.x + self.y * other.y
    }

    /// 取绝对值（每个分量）。
    /// 各分量向下取整（XNA `Vector2.Floor()`；原版头发渲染用它把发根
    /// 吸附到整数像素，`pureCeleste.cs:111179` `Nodes[0] = Nodes[0].Floor()`）。
    pub fn floor(self) -> Vec2 {
        Vec2::new(Fx::from_int(self.x.floor()), Fx::from_int(self.y.floor()))
    }

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
