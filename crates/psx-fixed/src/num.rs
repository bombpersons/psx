//! [`Num`]: a number of the original's format, in a [`Mode`].

use std::cmp::Ordering;
use std::fmt;
use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

use crate::mode::{Exact, Mode};
use crate::word::Word;

/// A number the original stores in word `W` with `F` fraction bits: a
/// `Fixed<W, F>` in [`Exact`], an `f32` in [`Float`](crate::Float).
pub struct Num<M: Mode, W: Word, const F: u32>(pub(crate) M::Repr<W, F>);

/// The original's fixed point: `F` fraction bits in word `W`.
pub type Fixed<W, const F: u32> = Num<Exact, W, F>;

impl<W: Word, const F: u32> Fixed<W, F> {
    /// The raw word.
    pub const fn from_bits(bits: W) -> Self {
        Num(bits)
    }

    /// The raw word.
    pub const fn bits(self) -> W {
        self.0
    }
}

impl<M: Mode, W: Word, const F: u32> Num<M, W, F> {
    /// Zero.
    pub fn zero() -> Self {
        Num(M::Repr::<W, F>::default())
    }

    /// From the original's raw value (the real value × 2^`F`), kept to `W`.
    /// The trace adapter's way in: traces hold raw values.
    pub fn from_raw(raw: i64) -> Self {
        Num(M::from_raw::<W, F>(raw))
    }

    /// `n` whole units.
    pub fn from_int(n: i32) -> Self {
        Self::from_raw((n as i64) << F)
    }

    /// The real value `x`. Panics in [`Exact`] when `F` fraction bits in `W`
    /// cannot hold it: write constants as numbers the format holds.
    pub fn real(x: f64) -> Self {
        Self::try_real(x).unwrap_or_else(|e| panic!("{e}"))
    }

    /// The real value `x`, or why the format cannot hold it exactly.
    pub fn try_real(x: f64) -> Result<Self, String> {
        M::from_real::<W, F>(x).map(Num)
    }

    /// The real value.
    pub fn to_f64(self) -> f64 {
        M::to_f64::<W, F>(self.0)
    }

    /// The real value as `f32`, for rendering.
    pub fn to_f32(self) -> f32 {
        self.to_f64() as f32
    }

    /// In another format: shifted to `G` fraction bits (rounding toward
    /// minus infinity when it loses bits) and kept to `W2`, as the original's
    /// shift and store do. The same value in [`Float`](crate::Float).
    pub fn rescale<W2: Word, const G: u32>(self) -> Num<M, W2, G> {
        Num(M::rescale::<W, F, W2, G>(self.0))
    }

    /// `self * b` from the whole 64-bit product (`mfhi:mflo`), shifted by
    /// `b`'s fraction bits: for code that keeps the high word.
    pub fn mul_wide<W2: Word, const G: u32>(self, b: Num<M, W2, G>) -> Self {
        Num(M::mul_wide::<W, F, W2, G>(self.0, b.0))
    }

    /// Divided by 2^`k`, rounding toward minus infinity: the original's
    /// arithmetic shift right (`x >> k`).
    pub fn div_pow2_floor(self, k: u32) -> Self {
        Num(M::div_pow2_floor::<W, F>(self.0, k))
    }

    /// Divided by 2^`k`, truncating toward zero: the original's `x / 2^k`.
    pub fn div_pow2_trunc(self, k: u32) -> Self {
        Num(M::div_pow2_trunc::<W, F>(self.0, k))
    }

    /// Zero once it is below the format's smallest step: a value the
    /// original's truncation brings to rest. [`Exact`] already has; `f32`
    /// stops it there instead of approaching zero forever.
    pub fn settle_to_zero(self) -> Self {
        Num(M::settle_to_zero::<W, F>(self.0))
    }

    /// Only the integer part, rounding toward minus infinity (the original
    /// masks the fraction off).
    pub fn snap_to_unit(self) -> Self {
        Num(M::snap_to_unit::<W, F>(self.0))
    }

    /// The integer part, rounding toward minus infinity (`x >> F`).
    pub fn floor_int(self) -> i32 {
        M::floor_int::<W, F>(self.0)
    }

    /// The integer part, truncating toward zero (`x / 2^F`).
    pub fn trunc_int(self) -> i32 {
        M::trunc_int::<W, F>(self.0)
    }

    /// The magnitude, kept to `W` (the most negative value stays itself).
    pub fn abs(self) -> Self {
        Num(M::abs::<W, F>(self.0))
    }

    pub fn min(self, b: Self) -> Self {
        if b < self { b } else { self }
    }

    pub fn max(self, b: Self) -> Self {
        if b > self { b } else { self }
    }

    pub fn clamp(self, lo: Self, hi: Self) -> Self {
        self.max(lo).min(hi)
    }

    pub fn is_negative(self) -> bool {
        self < Self::zero()
    }
}

impl<M: Mode, W: Word, const F: u32> Clone for Num<M, W, F> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M: Mode, W: Word, const F: u32> Copy for Num<M, W, F> {}

impl<M: Mode, W: Word, const F: u32> Default for Num<M, W, F> {
    fn default() -> Self {
        Self::zero()
    }
}

impl<M: Mode, W: Word, const F: u32> PartialEq for Num<M, W, F> {
    fn eq(&self, b: &Self) -> bool {
        self.0 == b.0
    }
}

impl<M: Mode, W: Word, const F: u32> PartialOrd for Num<M, W, F> {
    fn partial_cmp(&self, b: &Self) -> Option<Ordering> {
        self.0.partial_cmp(&b.0)
    }
}

impl<M: Mode, W: Word, const F: u32> fmt::Debug for Num<M, W, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)?;
        if M::EXACT { write!(f, " ({})", self.to_f64()) } else { Ok(()) }
    }
}

impl<M: Mode, W: Word, const F: u32> fmt::Display for Num<M, W, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.to_f64(), f)
    }
}

impl<M: Mode, W: Word, const F: u32> Add for Num<M, W, F> {
    type Output = Self;
    fn add(self, b: Self) -> Self {
        Num(M::add::<W, F>(self.0, b.0))
    }
}

impl<M: Mode, W: Word, const F: u32> Sub for Num<M, W, F> {
    type Output = Self;
    fn sub(self, b: Self) -> Self {
        Num(M::sub::<W, F>(self.0, b.0))
    }
}

impl<M: Mode, W: Word, const F: u32> Neg for Num<M, W, F> {
    type Output = Self;
    fn neg(self) -> Self {
        Num(M::neg::<W, F>(self.0))
    }
}

/// `(a * b) >> G`, keeping `a`'s format (crate documentation).
impl<M: Mode, W: Word, const F: u32, W2: Word, const G: u32> Mul<Num<M, W2, G>> for Num<M, W, F> {
    type Output = Self;
    fn mul(self, b: Num<M, W2, G>) -> Self {
        Num(M::mul::<W, F, W2, G>(self.0, b.0))
    }
}

/// `(a << G) / b`, keeping `a`'s format (crate documentation).
impl<M: Mode, W: Word, const F: u32, W2: Word, const G: u32> Div<Num<M, W2, G>> for Num<M, W, F> {
    type Output = Self;
    fn div(self, b: Num<M, W2, G>) -> Self {
        Num(M::div::<W, F, W2, G>(self.0, b.0))
    }
}

/// Times a plain integer.
impl<M: Mode, W: Word, const F: u32> Mul<i32> for Num<M, W, F> {
    type Output = Self;
    fn mul(self, n: i32) -> Self {
        Num(M::mul_int::<W, F>(self.0, n))
    }
}

/// Divided by a plain integer, truncating toward zero.
impl<M: Mode, W: Word, const F: u32> Div<i32> for Num<M, W, F> {
    type Output = Self;
    fn div(self, n: i32) -> Self {
        Num(M::div_int::<W, F>(self.0, n))
    }
}

impl<M: Mode, W: Word, const F: u32> AddAssign for Num<M, W, F> {
    fn add_assign(&mut self, b: Self) {
        *self = *self + b;
    }
}

impl<M: Mode, W: Word, const F: u32> SubAssign for Num<M, W, F> {
    fn sub_assign(&mut self, b: Self) {
        *self = *self - b;
    }
}

impl<M: Mode, W: Word, const F: u32, W2: Word, const G: u32> MulAssign<Num<M, W2, G>> for Num<M, W, F> {
    fn mul_assign(&mut self, b: Num<M, W2, G>) {
        *self = *self * b;
    }
}

impl<M: Mode, W: Word, const F: u32, W2: Word, const G: u32> DivAssign<Num<M, W2, G>> for Num<M, W, F> {
    fn div_assign(&mut self, b: Num<M, W2, G>) {
        *self = *self / b;
    }
}

impl<M: Mode, W: Word, const F: u32> MulAssign<i32> for Num<M, W, F> {
    fn mul_assign(&mut self, n: i32) {
        *self = *self * n;
    }
}

impl<M: Mode, W: Word, const F: u32> DivAssign<i32> for Num<M, W, F> {
    fn div_assign(&mut self, n: i32) {
        *self = *self / n;
    }
}

/// Written as its real value (`1.375`).
impl<M: Mode, W: Word, const F: u32> serde::Serialize for Num<M, W, F> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if M::EXACT { s.serialize_f64(self.to_f64()) } else { s.serialize_f32(self.to_f32()) }
    }
}

/// Read from a real value; in [`Exact`], refused when the format cannot
/// hold it exactly.
impl<'de, M: Mode, W: Word, const F: u32> serde::Deserialize<'de> for Num<M, W, F> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let x = d.deserialize_f64(Real)?;
        Self::try_real(x).map_err(serde::de::Error::custom)
    }
}

/// A real number from an asset, written with or without a fraction.
pub(crate) struct Real;

impl serde::de::Visitor<'_> for Real {
    type Value = f64;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a real number")
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<f64, E> {
        Ok(v)
    }
    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<f64, E> {
        Ok(v as f64)
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<f64, E> {
        Ok(v as f64)
    }
}
