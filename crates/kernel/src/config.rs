//! Kernel configuration.

use releste_math::Fx;

/// Kernel runtime configuration.
#[derive(Clone, Debug)]
pub struct KernelConfig {
    /// Target fixed time step (default `FIXED_DT` = 1/60).
    pub fixed_dt: Fx,
    /// Maximum number of catch-up frames (prevents a death spiral).
    pub max_catchup_frames: u32,
}

impl Default for KernelConfig {
    fn default() -> Self {
        KernelConfig {
            fixed_dt: releste_math::FIXED_DT,
            max_catchup_frames: 5,
        }
    }
}
