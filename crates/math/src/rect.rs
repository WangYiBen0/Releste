//! 轴对齐矩形。

use serde::{Deserialize, Serialize};
use std::fmt;

use super::{Fx, Vec2};

/// 定点轴对齐矩形。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Rect {
    pub min: Vec2,
    pub max: Vec2,
}

impl Rect {
    /// 零矩形。
    pub const ZERO: Rect = Rect {
        min: Vec2::ZERO,
        max: Vec2::ZERO,
    };

    /// 构造。
    pub const fn new(min: Vec2, max: Vec2) -> Self {
        Rect { min, max }
    }

    /// 从原点 + 尺寸构造。
    pub fn from_origin_size(origin: Vec2, size: Vec2) -> Self {
        Rect {
            min: origin,
            max: origin + size,
        }
    }

    /// 宽度。
    pub fn width(self) -> Fx {
        (self.max.x - self.min.x).abs()
    }

    /// 高度。
    pub fn height(self) -> Fx {
        (self.max.y - self.min.y).abs()
    }

    /// 尺寸。
    pub fn size(self) -> Vec2 {
        Vec2::new(self.width(), self.height())
    }

    /// 是否包含点。
    pub fn contains(self, point: Vec2) -> bool {
        point.x >= self.min.x
            && point.x <= self.max.x
            && point.y >= self.min.y
            && point.y <= self.max.y
    }

    /// 是否与另一矩形重叠。
    pub fn overlaps(self, other: Rect) -> bool {
        self.min.x <= other.max.x
            && self.max.x >= other.min.x
            && self.min.y <= other.max.y
            && self.max.y >= other.min.y
    }

    /// 并集。
    pub fn union(self, other: Rect) -> Rect {
        Rect {
            min: self.min.component_min(other.min),
            max: self.max.component_max(other.max),
        }
    }

    /// 膨胀（各边加上相同值）。
    pub fn inflate(self, amount: Fx) -> Rect {
        let inflate_vec = Vec2::new(amount, amount);
        Rect {
            min: self.min - inflate_vec,
            max: self.max + inflate_vec,
        }
    }
}

impl fmt::Debug for Rect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Rect({:?}, {:?})", self.min, self.max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_and_overlap() {
        let r = Rect::new(
            Vec2::new(Fx::from_int(0), Fx::from_int(0)),
            Vec2::new(Fx::from_int(10), Fx::from_int(10)),
        );
        assert!(r.contains(Vec2::new(Fx::from_int(5), Fx::from_int(5))));
        assert!(!r.contains(Vec2::new(Fx::from_int(15), Fx::from_int(5))));
        assert!(r.overlaps(Rect::new(
            Vec2::new(Fx::from_int(5), Fx::from_int(5)),
            Vec2::new(Fx::from_int(15), Fx::from_int(15))
        )));
    }
}
