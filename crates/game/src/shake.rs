//! Screen shake (`Level.shakeTimer` / `Level.ShakeVector`).
//!
//! Vanilla keeps the shake as level state and never touches the simulation camera:
//! `Level.Update` rolls a new offset every `0.04s` of **raw** (wall-clock) time, and
//! `Level.BeforeRender` adds it to `Camera.Position`, floors the result, then
//! restores the pre-shake position in `AfterRender`
//! (`pureCeleste.cs:113702-113731`, `:113848-113862`, `:114204-114208`).
//!
//! The equivalent split here is:
//!
//! - [`Shake::update`] runs once per fixed tick from `GameService::simulate_tick` and
//!   owns the timer + the rolled offset, mirroring `Level.Update`.
//! - [`Shake::offset`] is added to the **interpolated** render camera inside
//!   `GameService::camera_pos`, so the simulation camera stays untouched and only the
//!   drawn frame shakes (the debug overlay and the parallax backdrops read the same
//!   value, exactly like vanilla where the shake is applied before every render pass).
//!
//! Vanilla lets the player turn shake off (`Settings.Instance.ScreenShake`). releste
//! has no options screen yet, so the port always takes the `Full` branch and
//! documents the omission instead of inventing a setting.

use releste_math::{Fx, Vec2};

/// Shake amplitude decay factor: `num2 = ceil(shakeTimer * 10f)` (`pureCeleste.cs:113709`).
const AMPLITUDE_PER_SECOND: Fx = Fx::from_int(10);

/// Roll interval in raw-clock seconds: `OnRawInterval(0.04f)` (`pureCeleste.cs:113705`).
///
/// The comparator works in raw seconds, so it is stored as bits for the integer
/// floor-division below.
const ROLL_INTERVAL_BITS: i64 = (Fx::ONE as i64) * 4 / 100;

/// Deterministic PRNG (splitmix64) so a replayed tick sequence - and therefore a
/// screenshot - always produces the same shake offsets.
///
/// Identical to the backdrop flake generator in [`crate::backdrop`] (that one is
/// private to its module); AGENTS.md 15.1 forbids pulling in a `rand` crate for this.
#[derive(Debug, Clone, Copy)]
struct SplitMix64(u64);

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform integer in `0..bound` (vanilla `Calc.Random.Next(bound)`).
    fn next_below(&mut self, bound: u32) -> i32 {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % u64::from(bound)) as i32
    }
}

/// `Math.Ceiling(v)` for a non-negative `Fx` (the math crate has no `ceil`).
fn ceil(v: Fx) -> i32 {
    let floor = v.floor();
    if Fx::from_int(floor) < v {
        floor + 1
    } else {
        floor
    }
}

/// `(int)(t / interval)` -- the operand `Scene.OnRawInterval` compares (`pureCeleste.cs:113697`).
fn raw_slots(t: Fx) -> i64 {
    if t.bits() <= 0 {
        return -1;
    }
    t.bits() as i64 / ROLL_INTERVAL_BITS
}

/// Screen-shake state (`Level.shakeTimer` + `ShakeVector`).
///
/// Plain data plus a deterministic PRNG, so it is cheap to copy.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Shake {
    /// Raw-clock accumulator (vanilla `Scene.RawTimeActive`).
    raw_time: Fx,
    /// Remaining shake time in seconds (vanilla `shakeTimer`).
    timer: Fx,
    /// Unit direction of the last `DirectionalShake`, or zero for an ambient shake
    /// (vanilla `shakeDirection`).
    direction: Vec2,
    /// Alternating sign for directional shakes (vanilla `lastDirectionalShake`).
    last_directional: i32,
    /// Current integer offset added to the render camera (vanilla `ShakeVector`).
    vector: Vec2,
    /// How hard the shake is applied (vanilla `Settings.Instance.ScreenShake`).
    ///
    /// `Half` collapses the rolled vector to its sign and drains the timer 1.5x faster;
    /// `Off` never starts a shake at all. Both are checked in `Level.Shake`/
    /// `DirectionalShake` and in the `Level.Update` shake block
    /// (`pureCeleste.cs:113702-113731`).
    pub(crate) amount: crate::settings::ScreenShake,
    rng: SplitMix64,
}

impl Shake {
    /// Fresh state: no shake, fixed seed.
    pub(crate) fn new() -> Self {
        Self {
            raw_time: Fx::zero(),
            timer: Fx::zero(),
            direction: Vec2::ZERO,
            last_directional: 0,
            vector: Vec2::ZERO,
            amount: crate::settings::ScreenShake::Full,
            rng: SplitMix64::new(0x5AFE_5EED_1234_5678),
        }
    }

    /// `Level.DirectionalShake(dir, time)`: shake along an axis, alternating sign each
    /// roll (`pureCeleste.cs:114219-114229`).
    pub(crate) fn directional(&mut self, dir: Vec2, time: Fx) {
        // `if (Settings.Instance.ScreenShake != ScreenshakeAmount.Off)` (114208-114216):
        // with the shake turned off vanilla never even arms the timer.
        if self.amount == crate::settings::ScreenShake::Off {
            return;
        }
        self.direction = dir.safe_normalized();
        self.last_directional = 0;
        self.timer = self.timer.max(time);
    }

    /// `Level.Shake(time)`: ambient (non-directional) shake (`pureCeleste.cs:114204-114214`).
    #[allow(dead_code)]
    pub(crate) fn ambient(&mut self, time: Fx) {
        if self.amount == crate::settings::ScreenShake::Off {
            return;
        }
        self.direction = Vec2::ZERO;
        self.timer = self.timer.max(time);
    }

    /// One `Level.Update` step of the shake block (`pureCeleste.cs:113702-113731`).
    pub(crate) fn update(&mut self, dt: Fx) {
        let previous = self.raw_time;
        self.raw_time += dt;
        if self.timer <= Fx::zero() {
            self.vector = Vec2::ZERO;
            return;
        }
        if raw_slots(previous) < raw_slots(self.raw_time) {
            let amplitude = ceil(self.timer * AMPLITUDE_PER_SECOND);
            self.vector = if self.direction == Vec2::ZERO {
                let span = (amplitude * 2 + 1).max(0) as u32;
                Vec2::new(
                    Fx::from_int(-amplitude + self.rng.next_below(span)),
                    Fx::from_int(-amplitude + self.rng.next_below(span)),
                )
            } else {
                if self.last_directional == 0 {
                    self.last_directional = 1;
                } else {
                    self.last_directional = -self.last_directional;
                }
                -self.direction * Fx::from_int(self.last_directional * amplitude)
            };
            // `if (Settings.Instance.ScreenShake == ScreenshakeAmount.Half) { float x =
            // Math.Sign(ShakeVector.X); float y = Math.Sign(ShakeVector.Y);
            // ShakeVector = new Vector2(x, y); }` (113711-113716).
            if self.amount == crate::settings::ScreenShake::Half {
                self.vector = Vec2::new(
                    Fx::from_int(self.vector.x.signum()),
                    Fx::from_int(self.vector.y.signum()),
                );
            }
        }
        // `float num3 = ((ScreenShake == Half) ? 1.5f : 1f); shakeTimer -=
        // Engine.RawDeltaTime * num3;` (113718-113720): the half setting also drains the
        // timer faster, so the shake is both smaller and shorter.
        let rate = if self.amount == crate::settings::ScreenShake::Half {
            Fx::from_ratio(3, 2)
        } else {
            Fx::one()
        };
        self.timer -= dt * rate;
    }

    /// The offset to add to the render camera, already floored to whole pixels
    /// (vanilla floors the shaken position, `pureCeleste.cs:113849`).
    pub(crate) fn offset(&self) -> Vec2 {
        self.vector
    }

    /// True while a shake is running (used by the tests).
    #[cfg(test)]
    pub(crate) fn active(&self) -> bool {
        self.timer > Fx::zero()
    }
}

impl Default for Shake {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: Fx = Fx::from_bits(Fx::DT_ONE_SIXTIETH);

    /// A 1/4 second directional shake rolls on the 0.04 s raw cadence, alternates sign
    /// every roll, decays, and lands back on zero.
    #[test]
    fn directional_shake_alternates_and_decays() {
        let mut shake = Shake::new();
        shake.directional(Vec2::new(Fx::one(), Fx::zero()), Fx::from_ratio(1, 4));
        assert!(shake.active());
        let mut rolls = Vec::new();
        let mut seen = Vec2::ZERO;
        for _ in 0..40 {
            let before = shake.vector;
            shake.update(DT);
            if shake.vector != before {
                rolls.push(shake.vector.x);
            }
            seen = shake.vector;
        }
        // The final zero is the "shake ended" write (`ShakeVector = Vector2.Zero` in the
        // `else` branch), not a roll -- keep only the actual re-rolls.
        let rolls: Vec<Fx> = rolls.into_iter().filter(|v| v.abs() > Fx::zero()).collect();
        // 0.25 s of shake spans ~6 roll intervals.
        assert!(rolls.len() >= 5, "expected several rolls, got {rolls:?}");
        // Direction is +X, so every roll is `-direction * sign * ceil(timer * 10)`: the
        // X magnitude is the amplitude and consecutive rolls alternate SIGN. The
        // magnitudes themselves shrink as the timer drains, so `pair[0] == -pair[1]`
        // only holds for rolls taken at the same amplitude -- the sign is the invariant.
        for pair in rolls.windows(2) {
            assert_eq!(
                pair[0].signum(),
                -pair[1].signum(),
                "rolls must alternate sign: {rolls:?}"
            );
        }
        // Amplitude shrinks over time (ceil(timer * 10)), so the magnitudes never grow.
        let magnitudes: Vec<Fx> = rolls.iter().map(|v| v.abs()).collect();
        for pair in magnitudes.windows(2) {
            assert!(
                pair[1] <= pair[0],
                "amplitude must not grow: {magnitudes:?}"
            );
        }
        assert!(magnitudes.last().unwrap() < magnitudes.first().unwrap());
        // 0.25 s of ticks expires the timer and zeroes the offset.
        assert!(!shake.active());
        assert_eq!(seen, Vec2::ZERO);
    }

    /// An ambient shake stays within `[-amplitude, amplitude]` on both axes.
    #[test]
    fn ambient_shake_stays_inside_the_amplitude_box() {
        let mut shake = Shake::new();
        shake.ambient(Fx::from_ratio(3, 10));
        for _ in 0..10 {
            shake.update(DT);
            let limit = Fx::from_int(3);
            assert!(shake.offset().x.abs() <= limit);
            assert!(shake.offset().y.abs() <= limit);
        }
    }

    /// `Half` collapses the rolled vector to its sign and drains the timer 1.5x faster
    /// (`pureCeleste.cs:113711-113720`), so the shake is both 1px and shorter.
    #[test]
    fn half_shake_collapses_to_a_one_pixel_twitch() {
        let mut shake = Shake::new();
        shake.amount = crate::settings::ScreenShake::Half;
        shake.directional(Vec2::new(Fx::one(), Fx::zero()), Fx::from_ratio(1, 4));
        shake.update(DT);
        assert_eq!(
            shake.offset(),
            Vec2::new(Fx::from_int(-1), Fx::zero()),
            "Half keeps only the sign of the rolled amplitude (113711-113716)"
        );
        // 0.25s at the 1.5x drain rate is spent after ~10 ticks instead of 15.
        for _ in 0..14 {
            shake.update(DT);
        }
        assert!(
            !shake.active(),
            "the 1.5x drain shortens the shake (113720)"
        );
    }

    /// `Off` never arms the timer at all (`Level.DirectionalShake`, 114208-114216).
    #[test]
    fn off_never_starts_a_shake() {
        let mut shake = Shake::new();
        shake.amount = crate::settings::ScreenShake::Off;
        shake.directional(Vec2::new(Fx::one(), Fx::zero()), Fx::from_ratio(1, 2));
        assert!(!shake.active());
        shake.ambient(Fx::from_ratio(1, 2));
        assert!(!shake.active());
        shake.update(DT);
        assert_eq!(shake.offset(), Vec2::ZERO);
    }

    /// `Math.Max` semantics: a long shake is not shortened by a later short one.
    #[test]
    fn a_shorter_shake_does_not_truncate_a_longer_one() {
        let mut shake = Shake::new();
        shake.directional(Vec2::new(Fx::zero(), Fx::one()), Fx::from_ratio(1, 2));
        shake.directional(Vec2::new(Fx::one(), Fx::zero()), Fx::from_ratio(1, 10));
        shake.update(DT);
        assert!(shake.active());
    }
}
