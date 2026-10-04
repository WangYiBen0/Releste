//! 内核配置。

use reles_math::Fx;

/// 内核运行配置。
#[derive(Clone, Debug)]
pub struct KernelConfig {
    /// 目标固定时间步长（默认 `FIXED_DT` = 1/60）。
    pub fixed_dt: Fx,
    /// 最大追赶帧数（防止 death-spiral）。
    pub max_catchup_frames: u32,
}

impl Default for KernelConfig {
    fn default() -> Self {
        KernelConfig {
            fixed_dt: reles_math::FIXED_DT,
            max_catchup_frames: 5,
        }
    }
}
