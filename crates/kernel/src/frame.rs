//! Frame identity.

use std::fmt;
use std::num::NonZeroU64;

/// Monotonically increasing frame number (starting at 1).
///
/// Uses `NonZeroU64` so that `Option<FrameId>` gets the pointer optimization.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameId(NonZeroU64);

impl FrameId {
    /// The starting frame (frame 1).
    pub const ZERO: FrameId = FrameId(match NonZeroU64::new(1) {
        Some(v) => v,
        None => unreachable!(),
    });

    /// Construct from a number.
    ///
    /// # Panics
    /// Panics if `n` is 0.
    #[inline]
    pub fn new(n: u64) -> Self {
        assert!(n > 0, "FrameId must be >= 1; got 0");
        FrameId(NonZeroU64::new(n).unwrap())
    }

    /// Frame number.
    #[inline]
    pub fn get(self) -> u64 {
        self.0.get()
    }

    /// The next frame.
    #[inline]
    pub fn next(self) -> Self {
        FrameId::new(self.get().saturating_add(1))
    }

    /// The frame index since the start (0-based).
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
