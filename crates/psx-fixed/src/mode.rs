//! What a number is: the original's fixed point ([`Exact`]) or `f32`
//! ([`Float`]).

use std::fmt::Debug;

use crate::word::{Word, c_div, c_mul, unsigned};

/// How a mechanic's numbers compute. A mechanic generic over `M: Mode` runs
/// exactly as the original with [`Exact`], and in `f32` with [`Float`].
///
/// The functions take each number's format: its word `W` and fraction bits
/// `F` (and `W2`, `G` for a second operand). Mechanics use them through
/// [`Num`](crate::Num) and [`Angle`](crate::Angle).
pub trait Mode: Copy + Default + Debug + PartialEq + Send + Sync + 'static {
    /// A number of format `W`, `F`.
    type Repr<W: Word, const F: u32>: Copy + Default + Debug + PartialEq + PartialOrd + Send + Sync + 'static;
    /// Whether numbers are the original's fixed point.
    const EXACT: bool;

    /// From the original's raw value (the real value × 2^`F`).
    fn from_raw<W: Word, const F: u32>(raw: i64) -> Self::Repr<W, F>;
    /// From a real value; an error when the format cannot hold it exactly.
    fn from_real<W: Word, const F: u32>(x: f64) -> Result<Self::Repr<W, F>, String>;
    fn to_f64<W: Word, const F: u32>(a: Self::Repr<W, F>) -> f64;

    fn add<W: Word, const F: u32>(a: Self::Repr<W, F>, b: Self::Repr<W, F>) -> Self::Repr<W, F>;
    fn sub<W: Word, const F: u32>(a: Self::Repr<W, F>, b: Self::Repr<W, F>) -> Self::Repr<W, F>;
    fn neg<W: Word, const F: u32>(a: Self::Repr<W, F>) -> Self::Repr<W, F>;
    fn mul<W: Word, const F: u32, W2: Word, const G: u32>(
        a: Self::Repr<W, F>,
        b: Self::Repr<W2, G>,
    ) -> Self::Repr<W, F>;
    fn mul_wide<W: Word, const F: u32, W2: Word, const G: u32>(
        a: Self::Repr<W, F>,
        b: Self::Repr<W2, G>,
    ) -> Self::Repr<W, F>;
    fn div<W: Word, const F: u32, W2: Word, const G: u32>(
        a: Self::Repr<W, F>,
        b: Self::Repr<W2, G>,
    ) -> Self::Repr<W, F>;
    fn mul_int<W: Word, const F: u32>(a: Self::Repr<W, F>, n: i32) -> Self::Repr<W, F>;
    fn div_int<W: Word, const F: u32>(a: Self::Repr<W, F>, n: i32) -> Self::Repr<W, F>;
    fn rescale<W: Word, const F: u32, W2: Word, const G: u32>(a: Self::Repr<W, F>) -> Self::Repr<W2, G>;
    fn div_pow2_floor<W: Word, const F: u32>(a: Self::Repr<W, F>, k: u32) -> Self::Repr<W, F>;
    fn div_pow2_trunc<W: Word, const F: u32>(a: Self::Repr<W, F>, k: u32) -> Self::Repr<W, F>;
    fn settle_to_zero<W: Word, const F: u32>(a: Self::Repr<W, F>) -> Self::Repr<W, F>;
    fn snap_to_unit<W: Word, const F: u32>(a: Self::Repr<W, F>) -> Self::Repr<W, F>;
    fn floor_int<W: Word, const F: u32>(a: Self::Repr<W, F>) -> i32;
    fn trunc_int<W: Word, const F: u32>(a: Self::Repr<W, F>) -> i32;
    fn abs<W: Word, const F: u32>(a: Self::Repr<W, F>) -> Self::Repr<W, F>;
    /// The fraction of a turn: 0 to 1.
    fn wrap_unit<W: Word, const F: u32>(a: Self::Repr<W, F>) -> Self::Repr<W, F>;
    /// The fraction of a turn: -1/2 to 1/2.
    fn wrap_unit_signed<W: Word, const F: u32>(a: Self::Repr<W, F>) -> Self::Repr<W, F>;
    /// Sine of `a` turns from a quarter-wave table (`table[i]` is the sine
    /// of `i` quarter-turns / (len - 1), with `FO` fraction bits).
    fn sin_quarter<W: Word, const F: u32, const FO: u32>(a: Self::Repr<W, F>, table: &[i16]) -> Self::Repr<i32, FO>;
}

/// The original's fixed point: the raw word, computed as its compiled C
/// does (crate documentation).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Exact;

/// `f32`, holding the real value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Float;

const fn one<const F: u32>() -> i64 {
    assert!(F < 32, "a format has at most 31 fraction bits");
    1 << F
}

/// The table's value at `a` (any integer; one turn is 4 × (len - 1)).
fn quarter_wave(a: i64, table: &[i16]) -> i64 {
    let q = table.len() as i64 - 1;
    let a = a.rem_euclid(4 * q);
    let t = |i: i64| table[i as usize] as i64;
    match a / q {
        0 => t(a),
        1 => t(2 * q - a),
        2 => -t(a - 2 * q),
        _ => -t(4 * q - a),
    }
}

impl Mode for Exact {
    type Repr<W: Word, const F: u32> = W;
    const EXACT: bool = true;

    fn from_raw<W: Word, const F: u32>(raw: i64) -> W {
        W::wrap(raw)
    }

    fn from_real<W: Word, const F: u32>(x: f64) -> Result<W, String> {
        let one = one::<F>() as f64;
        let v = x * one;
        if !v.is_finite() || v.fract() != 0.0 {
            return Err(format!("{x} is not a multiple of 1/{one}"));
        }
        if v < W::MIN as f64 || v > W::MAX as f64 {
            return Err(format!("{x} is outside {} to {}", W::MIN as f64 / one, W::MAX as f64 / one));
        }
        Ok(W::wrap(v as i64))
    }

    fn to_f64<W: Word, const F: u32>(a: W) -> f64 {
        a.get() as f64 / one::<F>() as f64
    }

    fn add<W: Word, const F: u32>(a: W, b: W) -> W {
        W::wrap(a.get() + b.get())
    }

    fn sub<W: Word, const F: u32>(a: W, b: W) -> W {
        W::wrap(a.get() - b.get())
    }

    fn neg<W: Word, const F: u32>(a: W) -> W {
        W::wrap(-a.get())
    }

    fn mul<W: Word, const F: u32, W2: Word, const G: u32>(a: W, b: W2) -> W {
        let _ = one::<G>();
        W::wrap(c_mul(a.get(), b.get(), unsigned::<W, W2>(), G))
    }

    fn mul_wide<W: Word, const F: u32, W2: Word, const G: u32>(a: W, b: W2) -> W {
        let _ = one::<G>();
        // The whole 64-bit product (mfhi:mflo), shifted.
        W::wrap(((a.get() as i128 * b.get() as i128) >> G) as i64)
    }

    fn div<W: Word, const F: u32, W2: Word, const G: u32>(a: W, b: W2) -> W {
        let _ = one::<G>();
        W::wrap(c_div(a.get(), b.get(), unsigned::<W, W2>(), G))
    }

    fn mul_int<W: Word, const F: u32>(a: W, n: i32) -> W {
        W::wrap(c_mul(a.get(), n as i64, unsigned::<W, i32>(), 0))
    }

    fn div_int<W: Word, const F: u32>(a: W, n: i32) -> W {
        W::wrap(c_div(a.get(), n as i64, unsigned::<W, i32>(), 0))
    }

    fn rescale<W: Word, const F: u32, W2: Word, const G: u32>(a: W) -> W2 {
        let (_, _) = (one::<F>(), one::<G>());
        let v = a.get();
        W2::wrap(if G >= F { v << (G - F) } else { v >> (F - G) })
    }

    fn div_pow2_floor<W: Word, const F: u32>(a: W, k: u32) -> W {
        W::wrap(a.get() >> k)
    }

    fn div_pow2_trunc<W: Word, const F: u32>(a: W, k: u32) -> W {
        W::wrap(a.get() / (1i64 << k))
    }

    fn settle_to_zero<W: Word, const F: u32>(a: W) -> W {
        // The original's truncation has already settled it.
        a
    }

    fn snap_to_unit<W: Word, const F: u32>(a: W) -> W {
        W::wrap(a.get() & !(one::<F>() - 1))
    }

    fn floor_int<W: Word, const F: u32>(a: W) -> i32 {
        (a.get() >> F) as i32
    }

    fn trunc_int<W: Word, const F: u32>(a: W) -> i32 {
        (a.get() / one::<F>()) as i32
    }

    fn abs<W: Word, const F: u32>(a: W) -> W {
        W::wrap(a.get().abs())
    }

    fn wrap_unit<W: Word, const F: u32>(a: W) -> W {
        W::wrap(a.get() & (one::<F>() - 1))
    }

    fn wrap_unit_signed<W: Word, const F: u32>(a: W) -> W {
        let s = 64 - F;
        W::wrap((a.get() << s) >> s)
    }

    fn sin_quarter<W: Word, const F: u32, const FO: u32>(a: W, table: &[i16]) -> i32 {
        assert_eq!((table.len() as i64 - 1) * 4, one::<F>(), "the table's four quarters make one turn of the angle");
        quarter_wave(a.get(), table) as i32
    }
}

impl Mode for Float {
    type Repr<W: Word, const F: u32> = f32;
    const EXACT: bool = false;

    fn from_raw<W: Word, const F: u32>(raw: i64) -> f32 {
        (raw as f64 / one::<F>() as f64) as f32
    }

    fn from_real<W: Word, const F: u32>(x: f64) -> Result<f32, String> {
        Ok(x as f32)
    }

    fn to_f64<W: Word, const F: u32>(a: f32) -> f64 {
        a as f64
    }

    fn add<W: Word, const F: u32>(a: f32, b: f32) -> f32 {
        a + b
    }

    fn sub<W: Word, const F: u32>(a: f32, b: f32) -> f32 {
        a - b
    }

    fn neg<W: Word, const F: u32>(a: f32) -> f32 {
        -a
    }

    fn mul<W: Word, const F: u32, W2: Word, const G: u32>(a: f32, b: f32) -> f32 {
        a * b
    }

    fn mul_wide<W: Word, const F: u32, W2: Word, const G: u32>(a: f32, b: f32) -> f32 {
        a * b
    }

    fn div<W: Word, const F: u32, W2: Word, const G: u32>(a: f32, b: f32) -> f32 {
        a / b
    }

    fn mul_int<W: Word, const F: u32>(a: f32, n: i32) -> f32 {
        a * n as f32
    }

    fn div_int<W: Word, const F: u32>(a: f32, n: i32) -> f32 {
        a / n as f32
    }

    fn rescale<W: Word, const F: u32, W2: Word, const G: u32>(a: f32) -> f32 {
        a
    }

    fn div_pow2_floor<W: Word, const F: u32>(a: f32, k: u32) -> f32 {
        a / (1u64 << k) as f32
    }

    fn div_pow2_trunc<W: Word, const F: u32>(a: f32, k: u32) -> f32 {
        a / (1u64 << k) as f32
    }

    fn settle_to_zero<W: Word, const F: u32>(a: f32) -> f32 {
        // Below the original's smallest step, where its truncation stops.
        if a.abs() < 1.0 / one::<F>() as f32 { 0.0 } else { a }
    }

    fn snap_to_unit<W: Word, const F: u32>(a: f32) -> f32 {
        a.floor()
    }

    fn floor_int<W: Word, const F: u32>(a: f32) -> i32 {
        a.floor() as i32
    }

    fn trunc_int<W: Word, const F: u32>(a: f32) -> i32 {
        a.trunc() as i32
    }

    fn abs<W: Word, const F: u32>(a: f32) -> f32 {
        a.abs()
    }

    fn wrap_unit<W: Word, const F: u32>(a: f32) -> f32 {
        a.rem_euclid(1.0)
    }

    fn wrap_unit_signed<W: Word, const F: u32>(a: f32) -> f32 {
        (a + 0.5).rem_euclid(1.0) - 0.5
    }

    fn sin_quarter<W: Word, const F: u32, const FO: u32>(a: f32, _: &[i16]) -> f32 {
        (a * std::f32::consts::TAU).sin()
    }
}
