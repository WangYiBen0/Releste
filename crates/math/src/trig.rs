//! Fixed-point trigonometry.
//!
//! `f32` is forbidden in the simulation (AGENTS.md 15.7), so the handful of places that
//! need a sine -- vanilla `PlayerHair`'s wave (`references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:81`), `Player.Update`'s
//! two-dash hair offsets (`references/Celeste.exe.cs.d/Celeste/Player.cs:1714-1724`), and `WindMove` once wind lands -- use
//! [`Fx::sin`]: a 64-step quarter-wave table with linear interpolation.
//!
//! The table is generated from `sin(i / 64 * pi/2) * 65536` and every interpolation uses
//! integers only, so the result is bit-identical on every machine. Worst-case error is
//! below `8e-5` (about `1/200` of a pixel), far under what the hair can show.

use crate::Fx;

/// `pi / 2` in Q16.16 (`1.5707963 * 65536`).
const PI_OVER_2: i32 = 102_944;
/// `pi` in Q16.16.
const PI: i32 = 205_887;
/// `2 * pi` in Q16.16.
const TWO_PI: i32 = 411_775;

/// `sin(i / 64 * pi/2)` in Q16.16 for `i` in `0..=64`.
const SIN_QUARTER: [i32; 65] = [
    0, 1608, 3216, 4821, 6424, 8022, 9616, 11204, 12785, 14359, 15924, 17479, 19024, 20557, 22078,
    23586, 25080, 26558, 28020, 29466, 30893, 32303, 33692, 35062, 36410, 37736, 39040, 40320,
    41576, 42806, 44011, 45190, 46341, 47464, 48559, 49624, 50660, 51665, 52639, 53581, 54491,
    55368, 56212, 57022, 57798, 58538, 59244, 59914, 60547, 61145, 61705, 62228, 62714, 63162,
    63572, 63944, 64277, 64571, 64827, 65043, 65220, 65358, 65457, 65516, 65536,
];

impl Fx {
    /// Sine of `self` radians, in `-1.0..=1.0`.
    ///
    /// Integer range reduction (modulo `2 * pi`) plus the quarter-wave table above, so it
    /// is deterministic and needs no `f32`. Large arguments lose precision to the
    /// fixed-point step, exactly like any fixed-point angle accumulator would; the hair
    /// wave (`wave += 4 * dt`) only reaches a few hundred radians in a session.
    pub const fn sin(self) -> Fx {
        let mut bits = self.bits() % TWO_PI;
        if bits < 0 {
            bits += TWO_PI;
        }
        let value = if bits <= PI_OVER_2 {
            quarter(bits)
        } else if bits <= PI {
            quarter(PI - bits)
        } else if bits <= PI + PI_OVER_2 {
            -quarter(bits - PI)
        } else {
            -quarter(TWO_PI - bits)
        };
        Fx::from_bits(value)
    }
}

/// `sin(x)` for `x` in `0..=pi/2`, interpolated between table entries.
const fn quarter(bits: i32) -> i32 {
    let span = PI_OVER_2 as i64;
    let scaled = bits as i64 * 64;
    let index = (scaled / span) as usize;
    if index >= 64 {
        return SIN_QUARTER[64];
    }
    let lo = SIN_QUARTER[index] as i64;
    let hi = SIN_QUARTER[index + 1] as i64;
    (lo + (hi - lo) * (scaled % span) / span) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `1e-3` in Q16.16, comfortably above the table's interpolation error.
    const TOL: i32 = 66;

    fn close(a: Fx, b: Fx) -> bool {
        (a.bits() - b.bits()).abs() <= TOL
    }

    #[test]
    fn sin_hits_the_quadrant_anchors() {
        assert_eq!(Fx::zero().sin(), Fx::zero());
        assert!(close(Fx::from_bits(PI_OVER_2).sin(), Fx::one()));
        assert!(close(Fx::from_bits(PI).sin(), Fx::zero()));
        assert!(close(Fx::from_bits(PI + PI_OVER_2).sin(), Fx::from_int(-1)));
        assert!(close(Fx::from_bits(TWO_PI).sin(), Fx::zero()));
    }

    #[test]
    fn sin_matches_a_few_known_values() {
        // sin(0.5) = 0.479426, sin(1) = 0.841471, sin(2) = 0.909297.
        assert!(close(Fx::from_ratio(1, 2).sin(), Fx::from_bits(31_420)));
        assert!(close(Fx::from_int(1).sin(), Fx::from_bits(55_145)));
        assert!(close(Fx::from_int(2).sin(), Fx::from_bits(59_591)));
    }

    #[test]
    fn sin_is_odd_and_periodic() {
        for i in 1..40 {
            let x = Fx::from_ratio(i, 4);
            assert!(close((-x).sin(), -x.sin()), "odd at x = {i}/4");
            assert!(
                close((x + Fx::from_bits(TWO_PI)).sin(), x.sin()),
                "periodic at x = {i}/4"
            );
        }
    }

    #[test]
    fn sin_is_monotone_on_the_first_quadrant() {
        let mut prev = Fx::from_int(-1);
        for i in 0..=64 {
            let value = Fx::from_bits(i * PI_OVER_2 / 64).sin();
            assert!(value >= prev, "not monotone at step {i}");
            prev = value;
        }
        assert!(close(prev, Fx::one()));
    }
}
