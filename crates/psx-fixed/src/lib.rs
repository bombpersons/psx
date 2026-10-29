//! Numbers for ports: one mechanic, written once, run exactly as the
//! original computed it or in `f32`.
//!
//! A mechanic is generic over a [`Mode`]. Each number names its format, the
//! original's storage word and fraction bits, and the mode decides what it
//! is:
//!
//! ```
//! use psx_fixed::{Angle, Exact, Float, Mode, Num};
//!
//! /// Moves `pos` along `heading` at `speed` (both formats as the original
//! /// stores them: position 20.12, speed 16.16).
//! fn walk<M: Mode>(pos: &mut Num<M, i32, 12>, speed: Num<M, i32, 16>, heading: Angle<M, i16, 12>) {
//!     let step: Num<M, i32, 12> = (speed * heading.cos_quarter::<12>(&TABLE)).rescale();
//!     *pos += step;
//! }
//! # const TABLE: [i16; 1025] = psx_gte::math::SIN_QUARTER;
//!
//! let mut exact = Num::<Exact, i32, 12>::from_int(10);
//! walk(&mut exact, Num::real(1.5), Angle::from_units(0));
//! assert_eq!(exact.to_f64(), 11.5);
//! let mut float = Num::<Float, i32, 12>::from_int(10);
//! walk(&mut float, Num::real(1.5), Angle::from_units(0));
//! assert_eq!(float.to_f64(), 11.5);
//! ```
//!
//! - **[`Exact`]**: [`Num<Exact, W, F>`](Fixed) is the raw word `W` with `F`
//!   fraction bits, and every operation is what the original's compiled C
//!   does: arithmetic in 32-bit `int` (or `unsigned` when an operand is a
//!   `u32`), kept to `W`'s width as a store keeps it. `a * b` is
//!   `(a * b) >> G`: the low 32 bits of the product (`mult; mflo`), shifted
//!   arithmetically (rounding toward minus infinity), where `G` is `b`'s
//!   fraction bits, so the result keeps `a`'s format. `a / b` is
//!   `(a << G) / b`, truncating toward zero, with the R3000's results for a
//!   zero divisor.
//! - **[`Float`]**: every number is an `f32` holding the real value, and
//!   each named operation does what it is for (a stop threshold for
//!   [`Num::settle_to_zero`], `floor` for [`Num::snap_to_unit`]).
//!
//! Integer behaviour that carries meaning is a named operation on [`Num`]
//! or [`Angle`], never raw shifts and masks in a system. Conversion to
//! `f32` for rendering happens once, at the render boundary
//! ([`Num::to_f32`]).
//!
//! Assets hold real numbers: a [`Num`] reads and writes as one (`grip =
//! 1.375`), and in [`Exact`] loading refuses a value its format cannot hold
//! exactly. An [`Angle`] reads and writes as degrees.

#![forbid(unsafe_code)]

mod angle;
mod mode;
mod num;
mod vec;
mod word;

#[cfg(test)]
mod tests;

pub use angle::Angle;
pub use mode::{Exact, Float, Mode};
pub use num::{Fixed, Num};
pub use vec::{Vec2, Vec3};
pub use word::Word;
