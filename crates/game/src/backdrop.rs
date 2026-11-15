//! Animated backdrop effects: the `Backdrop` subclasses that animate on their own.
//!
//! Only the falling-snow backdrop (`snowfg` / `snowbg`) is ported so far. Vanilla
//! implements each of these as its own `Backdrop` subclass with a hand-written
//! `Update` + `Render` pair; [`Effect`] is the equivalent dispatch table, keyed by the
//! `<bg>`/`<fg>` element name the map stores in [`Backdrop::kind`].
//!
//! Simulation state lives in `Fx` (16.16 fixed point) and only turns into `f32` at
//! draw time, mirroring the parallax path in [`crate::session`]: flakes are drawn as
//! a single 1x1 quad each, wrapped into the 320x180 base viewport around the camera.
//!
//! [`Backdrop::kind`]: releste_map::Backdrop::kind

use releste_math::{Fx, Vec2};
use releste_render::SpriteDraw;

/// The 1x1 opaque-white sprite every animated effect is drawn with (vanilla
/// `Draw.Pixel`). Gameplay's `util\pixel` is a fully white 3x3 block, so it doubles as
/// the quad source without shipping a dedicated texture.
pub(crate) const PIXEL_SPRITE: &str = "util\\pixel";

/// Number of flakes (vanilla `Snow.particles = new Particle[60]`).
pub(crate) const PARTICLES: usize = 60;

/// Foreground palette: white and cornflower blue (`#6495ED`).
const FOREGROUND_COLORS: [[f32; 4]; 2] = [
    [1.0, 1.0, 1.0, 1.0],
    [0.392_156_86, 0.584_313_7, 0.929_411_76, 1.0],
];

/// Background palette: two dim blues.
const BACKGROUND_COLORS: [[f32; 4]; 2] = [[0.2, 0.2, 0.2, 1.0], [0.1, 0.2, 0.5, 1.0]];

/// Two colours per palette (vanilla `ForegroundColors`/`BackgroundColors`).
const COLOURS: usize = 2;

/// Target resolution the flakes wrap around (vanilla hard-codes 320x180).
const VIEW_W: f32 = releste_render::BASE_WIDTH;
const VIEW_H: f32 = releste_render::BASE_HEIGHT;

/// One flake.
///
/// `speed` is in base pixels per second and `phase` in radians, exactly like vanilla;
/// `colour` indexes the active palette.
struct Particle {
    x: Fx,
    y: Fx,
    colour: usize,
    speed: Fx,
    phase: Fx,
}

/// The falling-snow backdrop.
pub(crate) struct Snow {
    /// Foreground flakes are faster, brighter and larger-screened; background flakes are
    /// slow and dim.
    foreground: bool,
    particles: Vec<Particle>,
    /// Fade toward `Visible` at two units per second (vanilla `visibleFade`).
    visible_fade: Fx,
}

impl Snow {
    /// Build a snow backdrop with its flakes scattered over the viewport.
    ///
    /// `seed` must be stable for a given backdrop slot so a screenshot run is
    /// reproducible; vanilla draws from `Calc.Random`, so the exact implied start
    /// positions differ from the original game (documented deviation).
    pub(crate) fn new(foreground: bool, seed: u64) -> Self {
        let (speed_min, speed_max) = if foreground { (120, 300) } else { (40, 100) };
        let mut rng = SplitMix64::new(seed);
        let particles = (0..PARTICLES)
            .map(|_| Particle {
                x: rng.next_fx() * Fx::from_int(320),
                y: rng.next_fx() * Fx::from_int(180),
                colour: rng.next_below(COLOURS),
                speed: Fx::from_int(speed_min)
                    + rng.next_fx() * Fx::from_int(speed_max - speed_min),
                phase: rng.next_fx() * TWO_PI,
            })
            .collect();
        Self {
            foreground,
            particles,
            visible_fade: Fx::one(),
        }
    }

    /// Advance the flakes by one tick.
    ///
    /// Vanilla: `Position.x -= Speed * dt`, `Position.y += sin(Sin) * Speed * 0.2 * dt`,
    /// `Sin += dt`, while `visibleFade` approaches `IsVisible` at `dt * 2`.
    pub(crate) fn update(&mut self, visible: bool, dt: Fx) {
        let target = if visible { Fx::one() } else { Fx::zero() };
        approach(&mut self.visible_fade, target, dt * Fx::from_int(2));
        let drift = Fx::from_ratio(1, 5); // vanilla's 0.2f
        for p in &mut self.particles {
            p.x -= p.speed * dt;
            p.y += p.phase.sin() * p.speed * drift * dt;
            p.phase += dt;
        }
    }

    /// Palette colour for a flake, already multiplied by the fade (premultiplied alpha).
    fn colour(&self, index: usize) -> [f32; 4] {
        let palette = if self.foreground {
            &FOREGROUND_COLORS
        } else {
            &BACKGROUND_COLORS
        };
        let base = palette[index % palette.len()];
        let fade = self.visible_fade.to_f32();
        [
            base[0] * fade,
            base[1] * fade,
            base[2] * fade,
            base[3] * fade,
        ]
    }

    /// First flake's horizontal position (test hook: proves a tick really advanced).
    #[cfg(test)]
    pub(crate) fn first_x(&self) -> Fx {
        self.particles[0].x
    }

    /// Whether this is the foreground variant.
    #[cfg(test)]
    pub(crate) fn is_foreground(&self) -> bool {
        self.foreground
    }

    /// Queue this frame's quads (one 1x1 sprite per flake).
    pub(crate) fn push_draws(
        &self,
        out: &mut Vec<SpriteDraw>,
        camera: (f32, f32),
        atlas_id: u32,
        uv: ((f32, f32), (f32, f32)),
        layer: i32,
    ) {
        if self.visible_fade <= Fx::zero() {
            return;
        }
        for p in &self.particles {
            // Vanilla wraps in world space around the camera, then draws a centred pixel
            // (`Draw.Pixel.DrawCentered`), i.e. the quad's top-left is half a pixel up and
            // left of the flake position.
            let x = wrap(p.x.to_f32() - camera.0, VIEW_W) - 0.5;
            let y = wrap(p.y.to_f32() - camera.1, VIEW_H) - 0.5;
            out.push(
                SpriteDraw::new(atlas_id, Vec2::from_f32s(x, y), Vec2::ONE, uv)
                    .with_layer(layer)
                    .with_color(self.colour(p.colour)),
            );
        }
    }
}

/// A backdrop that animates itself, chosen from the map's element name.
pub(crate) enum Effect {
    Snow(Snow),
}

/// Whether this module renders `kind` (as opposed to the parallax path or a kind that is
/// still unimplemented).
pub(crate) fn handles_kind(kind: &str) -> bool {
    kind.eq_ignore_ascii_case("snowfg") || kind.eq_ignore_ascii_case("snowbg")
}

impl Effect {
    /// Build the effect for a `<bg>`/`<fg>` element name, or `None` when that kind is
    /// either static (`parallax`) or not ported yet.
    pub(crate) fn from_kind(kind: &str, slot: usize) -> Option<Self> {
        if kind.eq_ignore_ascii_case("snowfg") {
            Some(Self::Snow(Snow::new(true, seed_for(slot, true))))
        } else if kind.eq_ignore_ascii_case("snowbg") {
            Some(Self::Snow(Snow::new(false, seed_for(slot, false))))
        } else {
            None
        }
    }

    /// Whether an existing effect is still the right one for `kind` (hot reload may
    /// rewrite a slot from `snowbg` to `snowfg`, or to a plain `parallax`).
    pub(crate) fn same_kind(&self, kind: &str) -> bool {
        match self {
            Self::Snow(snow) => {
                (snow.foreground && kind.eq_ignore_ascii_case("snowfg"))
                    || (!snow.foreground && kind.eq_ignore_ascii_case("snowbg"))
            }
        }
    }

    /// Advance the effect by one tick; `visible` is the room-filtered `IsVisible`.
    pub(crate) fn update(&mut self, visible: bool, dt: Fx) {
        match self {
            Self::Snow(snow) => snow.update(visible, dt),
        }
    }

    /// Queue this frame's quads.
    pub(crate) fn push_draws(
        &self,
        out: &mut Vec<SpriteDraw>,
        camera: (f32, f32),
        atlas_id: u32,
        uv: ((f32, f32), (f32, f32)),
        layer: i32,
    ) {
        match self {
            Self::Snow(snow) => snow.push_draws(out, camera, atlas_id, uv, layer),
        }
    }
}

/// Which flakes a slot gets: stable per slot so a captured frame is reproducible.
fn seed_for(slot: usize, foreground: bool) -> u64 {
    let base = 0x9E37_79B9_7F4A_7C15u64;
    base ^ ((slot as u64) << 32) ^ u64::from(foreground)
}

/// Move `v` toward `target` by at most `rate` (vanilla `Calc.Approach`).
fn approach(v: &mut Fx, target: Fx, rate: Fx) {
    let diff = target - *v;
    *v = if diff.abs() <= rate {
        target
    } else {
        *v + Fx::from_int(diff.signum()) * rate
    };
}

/// Vanilla `mod(x, m) = (x % m + m) % m`: wrap into `0..m`.
fn wrap(value: f32, m: f32) -> f32 {
    ((value % m) + m) % m
}

/// `2 * pi` in Q16.16, the phase range vanilla scatters `Sin` over.
const TWO_PI: Fx = Fx::from_bits(411_775);

/// Deterministic PRNG (splitmix64) so flake placement never depends on wall time.
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

    /// Uniform in `0..1` with 16.16 resolution (`Fx`'s native precision).
    fn next_fx(&mut self) -> Fx {
        let top = (self.next_u64() >> 40) as u32; // 24 bits
        Fx::from_bits(((u64::from(top) * 65_536) >> 24) as i32)
    }

    /// Uniform integer in `0..bound`.
    fn next_below(&mut self, bound: usize) -> usize {
        (self.next_fx().bits() as i64 * bound as i64 / 65_536) as usize % bound
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use releste_render::Layer;

    const DT: Fx = Fx::from_bits(Fx::DT_ONE_SIXTIETH);
    const UV: ((f32, f32), (f32, f32)) = ((0.0, 0.0), (1.0, 1.0));

    fn draws(snow: &Snow) -> Vec<SpriteDraw> {
        let mut out = Vec::new();
        snow.push_draws(&mut out, (0.0, 0.0), 1, UV, Layer::Background.value());
        out
    }

    #[test]
    fn the_same_seed_scatters_the_same_flakes() {
        let a = Snow::new(true, 42);
        let b = Snow::new(true, 42);
        let mut c = Snow::new(true, 7);
        for i in 0..PARTICLES {
            assert_eq!(a.particles[i].x, b.particles[i].x);
            assert_eq!(a.particles[i].y, b.particles[i].y);
            assert_eq!(a.particles[i].speed, b.particles[i].speed);
        }
        assert!(a
            .particles
            .iter()
            .zip(&c.particles)
            .any(|(l, r)| l.x != r.x));
        // Seeds are drawn from the slot, so neighbouring slots differ too.
        c.update(true, DT);
        assert!(c.particles[0].x != a.particles[0].x);
    }

    #[test]
    fn flakes_start_inside_the_viewport_with_vanilla_speed_ranges() {
        for foreground in [true, false] {
            let snow = Snow::new(foreground, 3);
            let (lo, hi) = if foreground { (120, 301) } else { (40, 101) };
            for p in &snow.particles {
                assert!(p.x >= Fx::zero() && p.x < Fx::from_int(320));
                assert!(p.y >= Fx::zero() && p.y < Fx::from_int(180));
                let speed = p.speed.to_f32();
                assert!(
                    speed >= lo as f32 - 0.5 && speed <= hi as f32,
                    "speed {speed} outside {lo}..{hi}"
                );
            }
        }
    }

    #[test]
    fn a_tick_drifts_left_and_advances_the_bob_phase() {
        let mut snow = Snow::new(false, 11);
        let before: Vec<(f32, f32)> = snow
            .particles
            .iter()
            .map(|p| (p.x.to_f32(), p.phase.to_f32()))
            .collect();
        snow.update(true, DT);
        for (i, p) in snow.particles.iter().enumerate() {
            assert!(p.x.to_f32() < before[i].0, "flake {i} did not drift left");
            assert!(p.phase.to_f32() > before[i].1, "flake {i} phase stalled");
        }
    }

    #[test]
    fn draw_positions_stay_inside_the_viewport_after_many_ticks() {
        let mut snow = Snow::new(true, 5);
        for _ in 0..600 {
            snow.update(true, DT);
        }
        for draw in draws(&snow) {
            let (x, y) = draw.position.to_f32s();
            assert!(x > -1.0 && x < 320.0, "x {x} escaped the viewport");
            assert!(y > -1.0 && y < 180.0, "y {y} escaped the viewport");
            let (w, h) = draw.size.to_f32s();
            assert_eq!((w, h), (1.0, 1.0), "a flake must be one pixel");
        }
    }

    #[test]
    fn the_camera_shift_wraps_flakes_instead_of_moving_them_away() {
        let snow = Snow::new(false, 9);
        let at_origin = draws(&snow);
        let mut shifted = Vec::new();
        snow.push_draws(&mut shifted, (320.0, 0.0), 1, UV, Layer::Background.value());
        assert_eq!(at_origin.len(), shifted.len(), "same flake count");
        for draw in &shifted {
            let (x, _) = draw.position.to_f32s();
            assert!(
                x > -1.0 && x < 320.0,
                "x {x} escaped after the camera moved"
            );
        }
    }

    #[test]
    fn the_fade_follows_visibility_at_two_units_per_second() {
        let mut snow = Snow::new(false, 1);
        snow.update(false, DT);
        assert!((snow.visible_fade.to_f32() - (1.0 - 2.0 / 60.0)).abs() < 1e-3);
        for _ in 0..40 {
            snow.update(false, DT);
        }
        assert_eq!(snow.visible_fade, Fx::zero());
        assert!(
            draws(&snow).is_empty(),
            "a fully faded backdrop draws nothing"
        );
        for _ in 0..60 {
            snow.update(true, DT);
        }
        assert_eq!(snow.visible_fade, Fx::one());
        assert_eq!(draws(&snow).len(), PARTICLES);
    }

    #[test]
    fn only_snow_kinds_produce_an_effect_and_hot_reload_swaps_them() {
        assert!(Effect::from_kind("parallax", 0).is_none());
        assert!(Effect::from_kind("unknownkind", 0).is_none());
        let fg = Effect::from_kind("snowfg", 2).expect("snowfg has an effect");
        let bg = Effect::from_kind("snowbg", 2).expect("snowbg has an effect");
        assert!(fg.same_kind("snowfg"));
        assert!(!fg.same_kind("snowbg"));
        assert!(bg.same_kind("snowbg"));
    }
}
