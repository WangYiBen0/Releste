//! 携带状态机。
//!
//! # 关键机制：被携带者本地禁用碰撞
//!
//! 当玩家被另一名玩家携带时，其碰撞在**本地**被禁用。
//! 这使得携带者可以把被携带者带过墙 / 刺 / 关卡边界——
//! "世界不同步"是玩法而非 bug（AGENTS.md §4.4）。
//!
//! 服务器对此完全无感知：它只转发 [`CarryRequest`](crate::protocol::Up::CarryRequest)
//! 与接受 / 结束消息，不参与物理。

use reles_math::{Fx, Vec2};
use serde::{Deserialize, Serialize};

use crate::protocol::{CarryMode, PlayerId};

/// 本地的携带关系。
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct CarryLink {
    /// 携带者。
    pub carrier: PlayerId,
    /// 被携带者。
    pub passenger: PlayerId,
    /// 模式。
    pub mode: CarryMode,
}

/// 携带状态机（本地权威）。
///
/// 每个客户端只维护与"自己"相关的携带关系。
#[derive(Debug, Default)]
pub struct CarryState {
    /// 谁在携带我（`None` 表示我是自由的）。
    carried_by: Option<CarryLink>,
    /// 我在携带谁。
    carrying: Option<CarryLink>,
    /// 被抛出后的短暂无碰撞窗口（帧）。
    launch_grace: u8,
}

/// 抛出后保持无碰撞的帧数：够穿一层墙。
pub const LAUNCH_GRACE_FRAMES: u8 = 12;

/// 绳索模式的弹性系数（每帧把被携带者拉向携带者）。
const TETHER_STIFFNESS: f32 = 0.25;

impl CarryState {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 我是否正被携带。
    pub fn is_carried(&self) -> bool {
        self.carried_by.is_some()
    }

    /// 我是否正在携带别人。
    pub fn is_carrying(&self) -> bool {
        self.carrying.is_some()
    }

    /// 我在携带谁。
    pub fn carrying(&self) -> Option<PlayerId> {
        self.carrying.map(|l| l.passenger)
    }

    /// 谁在携带我。
    pub fn carried_by(&self) -> Option<PlayerId> {
        self.carried_by.map(|l| l.carrier)
    }

    /// 当前携带模式。
    pub fn mode(&self) -> Option<CarryMode> {
        self.carried_by
            .map(|l| l.mode)
            .or_else(|| self.carrying.map(|l| l.mode))
    }

    /// **核心**：本地是否应禁用碰撞。
    ///
    /// 被携带时禁用；抛出后的 grace 窗口内也禁用，
    /// 以便"穿墙带人"能真正穿过去。
    pub fn collides(&self) -> bool {
        !self.is_carried() && self.launch_grace == 0
    }

    /// 接受携带请求（我是被携带者）。
    pub fn accept_from(&mut self, carrier: PlayerId, mode: CarryMode) -> bool {
        if self.carried_by.is_some() || self.carrying.is_some() {
            // 已经在携带关系里，拒绝嵌套。
            return false;
        }
        self.carried_by = Some(CarryLink {
            carrier,
            passenger: PlayerId(0), // 本地玩家 ID 由上层填充
            mode,
        });
        true
    }

    /// 记录我发出的携带请求被接受（我是携带者）。
    pub fn mark_carrying(&mut self, passenger: PlayerId, mode: CarryMode) -> bool {
        if self.carrying.is_some() || self.carried_by.is_some() {
            return false;
        }
        self.carrying = Some(CarryLink {
            carrier: PlayerId(0),
            passenger,
            mode,
        });
        true
    }

    /// 结束携带（任一方发起）。
    pub fn end(&mut self) -> Option<CarryLink> {
        let link = self.carried_by.take().or_else(|| self.carrying.take());
        // 抛出 / 结束后的无碰撞窗口。
        if let Some(l) = &link {
            if l.mode == CarryMode::Launch {
                self.launch_grace = LAUNCH_GRACE_FRAMES;
            }
        }
        link
    }

    /// 每帧推进 grace 计时。
    pub fn tick(&mut self) {
        self.launch_grace = self.launch_grace.saturating_sub(1);
    }

    /// 计算被携带时的本地位置。
    ///
    /// - `Grab` / `Ride`：直接吸附到携带者的相对偏移。
    /// - `Tether`：按弹性插值。
    /// - `Launch`：不吸附（由抛出速度决定，通常已结束关系）。
    ///
    /// 返回 `None` 表示本帧不需要修正位置。
    pub fn resolve_position(&self, my_pos: Vec2, carrier_pos: Vec2, offset: Vec2) -> Option<Vec2> {
        let link = self.carried_by?;
        match link.mode {
            CarryMode::Grab | CarryMode::Ride => Some(carrier_pos + offset),
            CarryMode::Tether => {
                let target = carrier_pos + offset;
                let delta = target - my_pos;
                if delta.abs().x == Fx::zero() && delta.abs().y == Fx::zero() {
                    return None;
                }
                let stiffness = Fx::from_f32(TETHER_STIFFNESS);
                Some(my_pos + delta * stiffness)
            }
            CarryMode::Launch => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carried_disables_collision() {
        let mut st = CarryState::new();
        assert!(st.collides(), "free player collides");
        assert!(st.accept_from(PlayerId(1), CarryMode::Grab));
        assert!(!st.collides(), "carried player must not collide");
    }

    #[test]
    fn launch_grace_keeps_collision_off() {
        let mut st = CarryState::new();
        st.accept_from(PlayerId(1), CarryMode::Launch);
        st.end();
        assert!(!st.collides(), "grace window must keep collision off");
        for _ in 0..LAUNCH_GRACE_FRAMES {
            st.tick();
        }
        assert!(st.collides(), "collision restored after grace");
    }

    #[test]
    fn grab_snaps_to_carrier() {
        let mut st = CarryState::new();
        st.accept_from(PlayerId(1), CarryMode::Grab);
        let pos = st.resolve_position(
            Vec2::from_f32s(0.0, 0.0),
            Vec2::from_f32s(10.0, 20.0),
            Vec2::from_f32s(0.0, -8.0),
        );
        assert_eq!(pos, Some(Vec2::from_f32s(10.0, 12.0)));
    }

    #[test]
    fn tether_interpolates() {
        let mut st = CarryState::new();
        st.accept_from(PlayerId(1), CarryMode::Tether);
        let pos = st
            .resolve_position(
                Vec2::from_f32s(0.0, 0.0),
                Vec2::from_f32s(100.0, 0.0),
                Vec2::ZERO,
            )
            .expect("tether should move");
        // 25% 靠近
        assert!((pos.x.to_f32() - 25.0).abs() < 0.01);
    }

    #[test]
    fn no_nested_carry() {
        let mut st = CarryState::new();
        assert!(st.accept_from(PlayerId(1), CarryMode::Grab));
        assert!(!st.accept_from(PlayerId(2), CarryMode::Grab));
        assert!(!st.mark_carrying(PlayerId(3), CarryMode::Ride));
    }
}
