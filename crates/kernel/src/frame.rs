//! 帧标识。

use std::fmt;
use std::num::NonZeroU64;

/// 单调递增的帧编号（从 1 开始）。
///
/// 用 `NonZeroU64` 以保证 `Option<FrameId>` 可享指针优化。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameId(NonZeroU64);

impl FrameId {
    /// 起始帧（帧 1）。
    pub const ZERO: FrameId = FrameId(match NonZeroU64::new(1) {
        Some(v) => v,
        None => unreachable!(),
    });

    /// 从编号构造。
    ///
    /// # Panics
    /// `n` 为 0 时 panic。
    #[inline]
    pub fn new(n: u64) -> Self {
        assert!(n > 0, "FrameId must be >= 1; got 0");
        FrameId(NonZeroU64::new(n).unwrap())
    }

    /// 帧编号。
    #[inline]
    pub fn get(self) -> u64 {
        self.0.get()
    }

    /// 下一帧。
    #[inline]
    pub fn next(self) -> Self {
        FrameId::new(self.get().saturating_add(1))
    }

    /// 开始后的第几帧（从 0 开始）。
    #[inline]
    pub fn index(self) -> u64 {
        self.get() - 1
    }
}

impl Default for FrameId {
    fn default() -> Self {
        FrameId::ZERO
    }
}

impl fmt::Debug for FrameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FrameId({})", self.get())
    }
}

impl fmt::Display for FrameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.get())
    }
}
