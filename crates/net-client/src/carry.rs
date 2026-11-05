//! Carry state machine.
//!
//! # Key mechanism: the carried player disables collision locally
//!
//! When a player is carried by another player, its collision is disabled
//! **locally**. This lets the carrier bring the passenger through walls /
//! spikes / level boundaries -- "the world is not synced" is gameplay, not a
//! bug (AGENTS.md 4.4).
//!
//! The server is completely unaware of this: it only forwards
//! [`CarryRequest`](crate::protocol::Up::CarryRequest)
//! and accept / end messages, and takes no part in physics.

use releste_math::{Fx, Vec2};
use serde::{Deserialize, Serialize};

use crate::protocol::{CarryMode, PlayerId};

/// Local carry relationship.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct CarryLink {
    /// The carrier.
    pub carrier: PlayerId,
    /// The passenger.
    pub passenger: PlayerId,
    /// The mode.
    pub mode: CarryMode,
}

/// Carry state machine (locally authoritative).
///
/// Each client only maintains carry relationships involving "itself".
#[derive(Debug, Default)]
pub struct CarryState {
    /// Who is carrying me (`None` means I am free).
    carried_by: Option<CarryLink>,
    /// Who I am carrying.
    carrying: Option<CarryLink>,
    /// Short collision-disabled window after being launched (frames).
    launch_grace: u8,
}

/// Frames to stay collision-free after a launch: enough to pass through one
/// wall.
pub const LAUNCH_GRACE_FRAMES: u8 = 12;

/// Tether mode stiffness (pulls the passenger toward the carrier each frame).
const TETHER_STIFFNESS: f32 = 0.25;

impl CarryState {
    /// New.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether I am being carried.
    pub fn is_carried(&self) -> bool {
        self.carried_by.is_some()
    }

    /// Whether I am carrying someone.
    pub fn is_carrying(&self) -> bool {
        self.carrying.is_some()
    }

    /// Who I am carrying.
    pub fn carrying(&self) -> Option<PlayerId> {
        self.carrying.map(|l| l.passenger)
    }

    /// Who is carrying me.
    pub fn carried_by(&self) -> Option<PlayerId> {
        self.carried_by.map(|l| l.carrier)
    }

    /// The current carry mode.
    pub fn mode(&self) -> Option<CarryMode> {
        self.carried_by
            .map(|l| l.mode)
            .or_else(|| self.carrying.map(|l| l.mode))
    }

    /// **Core**: whether collision should be disabled locally.
    ///
    /// Disabled while carried; also disabled during the grace window after a
    /// launch, so that "carry through a wall" can really pass through.
    pub fn collides(&self) -> bool {
        !self.is_carried() && self.launch_grace == 0
    }

    /// Accept a carry request (I am the passenger).
    pub fn accept_from(&mut self, carrier: PlayerId, mode: CarryMode) -> bool {
        if self.carried_by.is_some() || self.carrying.is_some() {
            // Already in a carry relationship; reject nesting.
            return false;
        }
        self.carried_by = Some(CarryLink {
            carrier,
            passenger: PlayerId(0), // the local player ID is filled in by the upper layer
            mode,
        });
        true
    }

    /// Record that a carry request I sent was accepted (I am the carrier).
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

    /// End the carry (initiated by either side).
    pub fn end(&mut self) -> Option<CarryLink> {
        let link = self.carried_by.take().or_else(|| self.carrying.take());
        // Collision-free window after a launch / end.
        if let Some(l) = &link {
            if l.mode == CarryMode::Launch {
                self.launch_grace = LAUNCH_GRACE_FRAMES;
            }
        }
        link
    }

    /// Advance the grace timer each frame.
    pub fn tick(&mut self) {
        self.launch_grace = self.launch_grace.saturating_sub(1);
    }

    /// Compute the local position while being carried.
    ///
    /// - `Grab` / `Ride`: snap directly to the carrier's relative offset.
    /// - `Tether`: elastic interpolation.
    /// - `Launch`: no snapping (determined by launch velocity; the relationship
    ///   has usually ended).
    ///
    /// Returns `None` when the position needs no correction this frame.
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
        // 25% of the way closer
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
