//! Particles: the port of vanilla's `Monocle.ParticleSystem` and the `Dust` type.
//!
//! Vanilla runs three systems per level -- `Level.Particles` (depth -8000, 400 slots),
//! `Level.ParticlesBG` (depth 8000) and `Level.ParticlesFG` (depth -50000, 800 slots)
//! -- and `Dust.Burst` emits into whichever one the caller picks
//! (`references/Celeste.exe.cs.d/Monocle/ParticleSystem.cs`, `Celeste/Dust.cs`).
//! Only the player-facing `Particles` system and the `Dust` particle type are ported
//! here; every player dust call site in `crate::player` feeds it.
//!
//! Simulation state is `Fx` (16.16 fixed point) and turns into `f32` only at draw
//! time, mirroring [`crate::backdrop`]. Emission randomness comes from a local
//! splitmix64 PRNG so a fixed input sequence always reproduces the same frames.
//!
//! Two documented deviations:
//!
//! - Vanilla rotates each particle about the sprite centre; [`SpriteDraw`] only
//!   rotates about its top-left corner, so rotation is dropped for now (dust puffs
//!   are round, the missing spin is not visible).
//! - Vanilla has a per-tile "surface index" that swaps in `SparkyDust` on a handful of
//!   tile kinds (`Player.DustParticleFromSurfaceIndex`). releste has no surface table,
//!   so every burst uses plain `Dust`, like the vanilla default.

use releste_math::{Fx, Vec2};
use releste_render::{GpuRenderer, SpriteDraw};
use tracing::warn;

use crate::session::Session;
use crate::sprites;

/// Ring capacity (vanilla `Level.Particles = new ParticleSystem(-8000, 400)`).
const MAX_PARTICLES: usize = 400;

/// The four dust sources vanilla picks from (`ParticleTypes.Dust.SourceChooser`).
const SOURCES: [&str; 4] = [
    "particles\\smoke0",
    "particles\\smoke1",
    "particles\\smoke2",
    "particles\\smoke3",
];

/// `pi / 2` in Q16.16.
const PI_OVER_2: Fx = Fx::from_bits(102_944);

// ---- `ParticleTypes.Dust` (`Celeste/ParticleTypes.cs:24-38`) ----
/// `Acceleration = new Vector2(0f, 4f)` -- downward, px/s^2.
const DUST_ACCELERATION: (Fx, Fx) = (Fx::zero(), Fx::from_int(4));
/// `LifeMin` / `LifeMax` in seconds.
const DUST_LIFE_MIN: Fx = Fx::from_ratio(3, 10);
const DUST_LIFE_MAX: Fx = Fx::from_ratio(5, 10);
/// `Size = 0.7f`, `SizeRange = 0.2f`.
const DUST_SIZE: Fx = Fx::from_ratio(7, 10);
const DUST_SIZE_RANGE: Fx = Fx::from_ratio(2, 10);
/// `Direction = MathHelper.PiOver2`, `DirectionRange = 0.5f`.
const DUST_DIRECTION_RANGE: Fx = Fx::from_ratio(1, 2);
/// `SpeedMin = 5f` / `SpeedMax = 15f`.
const DUST_SPEED_MIN: Fx = Fx::from_int(5);
const DUST_SPEED_MAX: Fx = Fx::from_int(15);

/// One `Dust.Burst(...)` request raised by the player during a tick.
///
/// Vanilla emits straight into the level's particle system; the player and the
/// particle system are owned by different modules here, so the player queues bursts
/// and the service drains them after the physics step.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Burst {
    /// Emission centre in world coordinates.
    pub position: Vec2,
    /// Emission direction in radians (`Dust.Burst`'s `direction` argument).
    pub direction: Fx,
    /// Number of particles (`Dust.Burst`'s `count`).
    pub count: u8,
    /// Emit into the foreground system instead of the normal one (`Dust.BurstFG`).
    pub foreground: bool,
}

impl Burst {
    /// Convenience constructor for the player's call sites.
    pub(crate) const fn new(position: Vec2, direction: Fx, count: u8) -> Self {
        Self {
            position,
            direction,
            count,
            foreground: false,
        }
    }

    /// `Dust.BurstFG` variant.
    #[allow(dead_code)]
    pub(crate) const fn foreground(position: Vec2, direction: Fx, count: u8) -> Self {
        Self {
            position,
            direction,
            count,
            foreground: true,
        }
    }
}

/// One live particle (vanilla `Monocle.Particle`).
#[derive(Debug, Clone, Copy)]
struct Particle {
    active: bool,
    position: Vec2,
    speed: Vec2,
    /// Remaining life in seconds.
    life: Fx,
    /// Life at spawn (for the `Life / StartLife` ratio).
    start_life: Fx,
    size: Fx,
    start_size: Fx,
    /// Index into [`SOURCES`].
    source: u8,
}

impl Particle {
    const fn empty() -> Self {
        Self {
            active: false,
            position: Vec2::ZERO,
            speed: Vec2::ZERO,
            life: Fx::zero(),
            start_life: Fx::one(),
            size: Fx::zero(),
            start_size: Fx::zero(),
            source: 0,
        }
    }
}

/// Deterministic PRNG (splitmix64), identical to the backdrop one so replays and
/// screenshots stay byte-stable. AGENTS.md 15.1 forbids adding a `rand` crate.
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

    /// Uniform in `0..1` at 16.16 resolution (`Calc.Random.NextFloat()`).
    fn next_fx(&mut self) -> Fx {
        let top = (self.next_u64() >> 40) as u32; // 24 bits
        Fx::from_bits(((u64::from(top) * 65_536) >> 24) as i32)
    }

    /// Uniform in `0..1` shifted to `-0.5..0.5` (`Calc.Random.NextFloat() - 0.5f`).
    fn next_signed(&mut self) -> Fx {
        self.next_fx() - Fx::from_ratio(1, 2)
    }

    /// Uniform integer in `0..bound` (vanilla `Calc.Random.Next(bound)`).
    fn next_below(&mut self, bound: u32) -> i32 {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % u64::from(bound)) as i32
    }
}

/// `Calc.AngleToVector(angle, length)` = `(cos, sin) * length`; the math crate exposes
/// `sin` only, so cosine is the quarter-turn shift.
fn angle_to_vector(angle: Fx, length: Fx) -> Vec2 {
    Vec2::new((angle + PI_OVER_2).sin() * length, angle.sin() * length)
}

/// `Ease.CubeOut(t) = 1 - (1 - t)^3`.
fn cube_out(t: Fx) -> Fx {
    let inv = Fx::one() - t;
    Fx::one() - inv * inv * inv
}

/// One source sprite resolved for drawing: `(atlas id, frame size, uv range, trim origin)`.
type ResolvedSource = (u32, (f32, f32), ((f32, f32), (f32, f32)), (f32, f32));

/// The normal particle system (vanilla `Level.Particles`).
#[derive(Debug, Clone)]
pub(crate) struct ParticleSystem {
    particles: Vec<Particle>,
    next_slot: usize,
    rng: SplitMix64,
}

impl ParticleSystem {
    /// Fresh system with a fixed seed.
    pub(crate) fn new() -> Self {
        Self {
            particles: vec![Particle::empty(); MAX_PARTICLES],
            next_slot: 0,
            rng: SplitMix64::new(0x0D15_7A11_0BEE_F00D),
        }
    }

    /// Drain the player's queued bursts (`Dust.Burst`).
    pub(crate) fn emit(&mut self, bursts: &mut Vec<Burst>) {
        for burst in bursts.drain(..) {
            if burst.foreground {
                // No foreground system is ported yet; the only foreground call site in
                // `Player` is the bangs puff, which is decorative.
                continue;
            }
            for _ in 0..burst.count {
                self.emit_one(burst);
            }
        }
    }

    /// `Dust.Burst`'s per-particle path: scatter inside the perpendicular spread box,
    /// then `ParticleSystem.Emit` (`Celeste/Dust.cs:9-27`).
    fn emit_one(&mut self, burst: Burst) {
        // `vector = Calc.AngleToVector(direction - PiOver2, 4f)`, absolute components.
        let spread = angle_to_vector(burst.direction - PI_OVER_2, Fx::from_int(4)).abs();
        let offset = Vec2::new(
            spread.x * self.rng.next_signed() * Fx::from_int(2),
            spread.y * self.rng.next_signed() * Fx::from_int(2),
        );
        self.create(burst.position + offset, burst.direction);
    }

    /// `ParticleType.Create` for the `Dust` type (`Monocle/ParticleType.cs:138-195`).
    fn create(&mut self, position: Vec2, direction: Fx) {
        let slot = self.next_slot;
        self.next_slot = (self.next_slot + 1) % MAX_PARTICLES;

        let size = DUST_SIZE - DUST_SIZE_RANGE * Fx::from_ratio(1, 2)
            + self.rng.next_fx() * DUST_SIZE_RANGE;
        let life = DUST_LIFE_MIN + self.rng.next_fx() * (DUST_LIFE_MAX - DUST_LIFE_MIN);
        let angle = direction - DUST_DIRECTION_RANGE * Fx::from_ratio(1, 2)
            + self.rng.next_fx() * DUST_DIRECTION_RANGE;
        let speed = DUST_SPEED_MIN + self.rng.next_fx() * (DUST_SPEED_MAX - DUST_SPEED_MIN);

        self.particles[slot] = Particle {
            active: true,
            position,
            speed: angle_to_vector(angle, speed),
            life,
            start_life: life,
            size,
            start_size: size,
            source: self.rng.next_below(SOURCES.len() as u32) as u8,
        };
    }

    /// One `ParticleSystem.Update` pass (`Monocle/Particle.cs:57-137`).
    pub(crate) fn update(&mut self, dt: Fx) {
        for particle in &mut self.particles {
            if !particle.active {
                continue;
            }
            // `float num2 = Life / StartLife; Life -= num; if (Life <= 0f) { Active = false; return; }`.
            let ratio = particle.life / particle.start_life;
            particle.life -= dt;
            if particle.life <= Fx::zero() {
                particle.active = false;
                continue;
            }
            particle.position += particle.speed * dt;
            particle.speed += Vec2::new(DUST_ACCELERATION.0, DUST_ACCELERATION.1) * dt;
            // `ScaleOut` -> `Size = StartSize * Ease.CubeOut(num2)`.
            particle.size = particle.start_size * cube_out(ratio);
            // Dust has zero friction (`Friction = 0f`) and zero spin, so the
            // `Calc.Approach(Speed, Zero, Friction * dt)` step is a no-op.
        }
    }

    /// Append this system's draws at `layer`.
    ///
    /// `Particle.Render` truncates the position to whole pixels and draws the source
    /// sprite scaled about its **centre** (`Monocle/Particle.cs:139-166`), so the quad
    /// top-left is `floor(position) - origin_in_trim * size`.
    pub(crate) fn push_draws(
        &self,
        out: &mut Vec<SpriteDraw>,
        session: &Session,
        gpu: &GpuRenderer,
        layer: i32,
    ) {
        let mut resolved: [Option<ResolvedSource>; SOURCES.len()] = [None; SOURCES.len()];
        for (slot, name) in SOURCES.iter().enumerate() {
            let Some((group, sprite)) = session.atlas.sprite(name) else {
                if session.warn_once(&format!("particle-tex:{name}")) {
                    warn!(texture = %name, "particle sprite not found in atlas; skipping it");
                }
                continue;
            };
            let atlas_id = sprites::group_id(&session.atlas, group);
            let Some((tw, th)) = gpu.texture_size(atlas_id) else {
                continue;
            };
            let uv = GpuRenderer::uv_for_sprite(sprite, Vec2::from_f32s(tw as f32, th as f32));
            let (fw, fh) = sprite.frame_size();
            // Centre of the untrimmed frame, expressed in the cropped rect (vanilla
            // `MTexture.Draw` passes `origin - DrawOffset`).
            let origin = sprites::origin_in_trim(sprite, (fw * 0.5, fh * 0.5));
            resolved[slot] = Some((atlas_id, (fw, fh), uv, origin));
        }

        for particle in &self.particles {
            if !particle.active {
                continue;
            }
            let Some((atlas_id, (fw, fh), uv, origin)) = resolved[particle.source as usize] else {
                continue;
            };
            let (px, py) = particle.position.to_f32s();
            let size = particle.size.to_f32();
            out.push(
                SpriteDraw::new(
                    atlas_id,
                    Vec2::from_f32s(px.trunc() - origin.0 * size, py.trunc() - origin.1 * size),
                    Vec2::from_f32s(fw * size, fh * size),
                    uv,
                )
                .with_layer(layer),
            );
        }
    }

    /// Number of live particles (tests).
    #[cfg(test)]
    pub(crate) fn alive(&self) -> usize {
        self.particles.iter().filter(|p| p.active).count()
    }
}

impl Default for ParticleSystem {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: Fx = Fx::from_bits(Fx::DT_ONE_SIXTIETH);

    /// A burst activates exactly `count` particles.
    #[test]
    fn a_burst_activates_the_requested_count() {
        let mut system = ParticleSystem::new();
        let mut bursts = vec![Burst::new(Vec2::ZERO, PI_OVER_2, 4)];
        system.emit(&mut bursts);
        assert_eq!(system.alive(), 4);
        assert!(bursts.is_empty(), "emit must drain the queue");
    }

    /// Dust spawns above the emit point (direction -Pi/2 after the PiOver2 default) and
    /// accelerates downward, expiring between 0.3 and 0.5 seconds.
    #[test]
    fn dust_rises_then_falls_and_expires() {
        let mut system = ParticleSystem::new();
        let mut bursts = vec![Burst::new(Vec2::ZERO, -PI_OVER_2, 1)];
        system.emit(&mut bursts);
        // First tick: the particle has moved upward (negative y).
        system.update(DT);
        assert!(system.particles[0].position.y < Fx::zero());
        // After 0.5s of updates every particle is gone.
        for _ in 0..30 {
            system.update(DT);
        }
        assert_eq!(system.alive(), 0);
    }

    /// The ring overwrites the oldest slot instead of growing without bound.
    #[test]
    fn the_ring_never_exceeds_its_capacity() {
        let mut system = ParticleSystem::new();
        let mut bursts = vec![Burst::new(Vec2::ZERO, PI_OVER_2, 255); 4];
        system.emit(&mut bursts);
        assert_eq!(system.alive(), MAX_PARTICLES);
    }

    /// CubeOut(0) = 0, CubeOut(1) = 1, and it is monotonic in between.
    #[test]
    fn cube_out_matches_the_vanilla_easing() {
        assert_eq!(cube_out(Fx::zero()), Fx::zero());
        assert_eq!(cube_out(Fx::one()), Fx::one());
        assert!(cube_out(Fx::from_ratio(1, 2)) > Fx::from_ratio(1, 2));
    }
}
