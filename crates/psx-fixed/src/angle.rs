//! [`Angle`]: a fraction of a turn that wraps.

use std::cmp::Ordering;
use std::fmt;
use std::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};

use crate::mode::Mode;
use crate::num::{Num, Real};
use crate::word::Word;

/// An angle the original stores in word `W`, one turn being 2^`BITS` units
/// (the PlayStation's libgte: 4096, `BITS = 12`). In [`Exact`](crate::Exact)
/// it is the raw word; in [`Float`](crate::Float), turns as `f32`.
///
/// Adding and subtracting wrap at `W`'s width, as the original's do; a
/// whole turn is only folded away by [`normalized`](Angle::normalized) or
/// [`normalized_signed`](Angle::normalized_signed), where the original masks.
pub struct Angle<M: Mode, W: Word, const BITS: u32>(Num<M, W, BITS>);

impl<M: Mode, W: Word, const BITS: u32> Angle<M, W, BITS> {
    /// `units` of the original's angle (2^`BITS` to a turn).
    pub fn from_units(units: i64) -> Self {
        Angle(Num::from_raw(units))
    }

    /// `d` degrees. Panics in [`Exact`](crate::Exact) when it is not a whole
    /// number of units.
    pub fn degrees(d: f64) -> Self {
        Self::try_degrees(d).unwrap_or_else(|e| panic!("{e}"))
    }

    /// `d` degrees, or why it is not a whole number of units.
    pub fn try_degrees(d: f64) -> Result<Self, String> {
        Num::try_real(d / 360.0)
            .map(Angle)
            .map_err(|_| format!("{d}° is not a whole number of 1/{} turns", 1u64 << BITS))
    }

    /// As a fraction of a turn.
    pub fn from_turns(t: Num<M, W, BITS>) -> Self {
        Angle(t)
    }

    /// As a fraction of a turn.
    pub fn turns(self) -> Num<M, W, BITS> {
        self.0
    }

    pub fn to_degrees(self) -> f64 {
        self.0.to_f64() * 360.0
    }

    /// In radians, for rendering.
    pub fn to_radians_f32(self) -> f32 {
        (self.0.to_f64() * std::f64::consts::TAU) as f32
    }

    /// Within one turn, 0 up to a turn (the original's `& (2^BITS - 1)`).
    pub fn normalized(self) -> Self {
        Angle(Num(M::wrap_unit::<W, BITS>(self.0.0)))
    }

    /// Within half a turn either way, -1/2 up to 1/2 (the original's
    /// sign-extension of the low `BITS` bits): the way to compare headings.
    pub fn normalized_signed(self) -> Self {
        Angle(Num(M::wrap_unit_signed::<W, BITS>(self.0.0)))
    }

    /// Sine from a quarter-wave table (`table[i]` = sin(i/(len-1) quarter
    /// turns) with `FO` fraction bits, len - 1 = 2^`BITS` / 4), as the
    /// original looks it up; `f32::sin` in [`Float`](crate::Float).
    pub fn sin_quarter<const FO: u32>(self, table: &[i16]) -> Num<M, i32, FO> {
        Num(M::sin_quarter::<W, BITS, FO>(self.0.0, table))
    }

    /// Cosine from the same table: the sine a quarter turn on.
    pub fn cos_quarter<const FO: u32>(self, table: &[i16]) -> Num<M, i32, FO> {
        (self + Self::from_units(1 << (BITS - 2))).sin_quarter(table)
    }
}

impl<M: Mode, W: Word, const BITS: u32> Clone for Angle<M, W, BITS> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M: Mode, W: Word, const BITS: u32> Copy for Angle<M, W, BITS> {}

impl<M: Mode, W: Word, const BITS: u32> Default for Angle<M, W, BITS> {
    fn default() -> Self {
        Angle(Num::zero())
    }
}

impl<M: Mode, W: Word, const BITS: u32> PartialEq for Angle<M, W, BITS> {
    fn eq(&self, b: &Self) -> bool {
        self.0 == b.0
    }
}

impl<M: Mode, W: Word, const BITS: u32> PartialOrd for Angle<M, W, BITS> {
    fn partial_cmp(&self, b: &Self) -> Option<Ordering> {
        self.0.partial_cmp(&b.0)
    }
}

impl<M: Mode, W: Word, const BITS: u32> fmt::Debug for Angle<M, W, BITS> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}°", self.to_degrees())
    }
}

impl<M: Mode, W: Word, const BITS: u32> Add for Angle<M, W, BITS> {
    type Output = Self;
    fn add(self, b: Self) -> Self {
        Angle(self.0 + b.0)
    }
}

impl<M: Mode, W: Word, const BITS: u32> Sub for Angle<M, W, BITS> {
    type Output = Self;
    fn sub(self, b: Self) -> Self {
        Angle(self.0 - b.0)
    }
}

impl<M: Mode, W: Word, const BITS: u32> Neg for Angle<M, W, BITS> {
    type Output = Self;
    fn neg(self) -> Self {
        Angle(-self.0)
    }
}

impl<M: Mode, W: Word, const BITS: u32> AddAssign for Angle<M, W, BITS> {
    fn add_assign(&mut self, b: Self) {
        *self = *self + b;
    }
}

impl<M: Mode, W: Word, const BITS: u32> SubAssign for Angle<M, W, BITS> {
    fn sub_assign(&mut self, b: Self) {
        *self = *self - b;
    }
}

/// Scaled: `(a * b) >> G` in units, like [`Num`]'s product.
impl<M: Mode, W: Word, const BITS: u32, W2: Word, const G: u32> Mul<Num<M, W2, G>> for Angle<M, W, BITS> {
    type Output = Self;
    fn mul(self, b: Num<M, W2, G>) -> Self {
        Angle(self.0 * b)
    }
}

/// Times a plain integer.
impl<M: Mode, W: Word, const BITS: u32> Mul<i32> for Angle<M, W, BITS> {
    type Output = Self;
    fn mul(self, n: i32) -> Self {
        Angle(self.0 * n)
    }
}

/// Written as degrees (`90.0`).
impl<M: Mode, W: Word, const BITS: u32> serde::Serialize for Angle<M, W, BITS> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if M::EXACT { s.serialize_f64(self.to_degrees()) } else { s.serialize_f32(self.to_degrees() as f32) }
    }
}

/// Read from degrees; in [`Exact`](crate::Exact), refused when not a whole
/// number of units.
impl<'de, M: Mode, W: Word, const BITS: u32> serde::Deserialize<'de> for Angle<M, W, BITS> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let x = d.deserialize_f64(Real)?;
        Self::try_degrees(x).map_err(serde::de::Error::custom)
    }
}
