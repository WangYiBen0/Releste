//! 2D vector -- fixed-point coordinates.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

use super::Fx;

/// Fixed-point 2D vector.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Vec2 {
    pub x: Fx,
    pub y: Fx,
}

impl Vec2 {
    /// Zero vector.
    pub const ZERO: Vec2 = Vec2 {
        x: Fx::zero(),
        y: Fx::zero(),
    };

    /// Unit vector (1, 1).
    pub const ONE: Vec2 = Vec2 {
        x: Fx::one(),
        y: Fx::one(),
    };

    /// Construct a new vector.
    pub const fn new(x: Fx, y: Fx) -> Self {
        Vec2 { x, y }
    }

    /// Construct from `f32` (only used when loading non-simulation data).
    pub fn from_f32s(x: f32, y: f32) -> Self {
        Vec2 {
            x: Fx::from_f32(x),
            y: Fx::from_f32(y),
        }
    }

    /// Convert to `(f32, f32)`.
    pub fn to_f32s(self) -> (f32, f32) {
        (self.x.to_f32(), self.y.to_f32())
    }

    /// Component-wise minimum.
    pub fn component_min(self, other: Vec2) -> Vec2 {
        Vec2 {
            x: self.x.min(other.x),
            y: self.y.min(other.y),
        }
    }

    /// Component-wise maximum.
    pub fn component_max(self, other: Vec2) -> Vec2 {
        Vec2 {
            x: self.x.max(other.x),
            y: self.y.max(other.y),
        }
    }

    /// Squared length.
    ///
    /// **Can overflow**: `Fx` is 16.16, so once a component's absolute value
    /// exceeds about 181, `x*x` no longer fits in `i32` and wraps around. Only
    /// use it for short vectors of known magnitude (velocity, facing). To
    /// compare the distance between arbitrary two points, use
    /// [`Vec2::distance_sq`].
    pub fn len_sq(self) -> Fx {
        self.x * self.x + self.y * self.y
    }

    /// Squared distance between two points, carried in `i64`, **cannot wrap**.
    ///
    /// The return value is in units of "fixed-point bits^2": only
    /// `Fx::from_bits(v)` gives the squared distance in `Fx` units. The reason
    /// it cannot return `Fx`: the maximum component of `Fx` is 32767, meaning
    /// the squared distance of any point pair beyond 181px would be truncated,
    /// while comparisons like "which is closer" at map scale (hundreds to
    /// thousands of pixels) depend entirely on it -- wrapping would make a
    /// farther point be judged closer because the sign flipped.
    ///
    /// Used for nearest-point / nearest-room selection and any scenario that
    /// needs a monotonic distance.
    pub fn distance_sq(self, other: Vec2) -> i64 {
        let dx = (self.x.bits() as i64) - (other.x.bits() as i64);
        let dy = (self.y.bits() as i64) - (other.y.bits() as i64);
        dx * dx + dy * dy
    }

    /// Dot product.
    pub fn dot(self, other: Vec2) -> Fx {
        self.x * other.x + self.y * other.y
    }

    /// Floor each component (XNA `Vector2.Floor()`; vanilla hair rendering
    /// uses it to snap the hair root to integer pixels,
    /// `pureCeleste.cs:111179` `Nodes[0] = Nodes[0].Floor()`).
    pub fn floor(self) -> Vec2 {
        Vec2::new(Fx::from_int(self.x.floor()), Fx::from_int(self.y.floor()))
    }

    /// Absolute value (of each component).
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
