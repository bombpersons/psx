//! The integers a fixed-point number is stored in.

use std::fmt::Debug;
use std::hash::Hash;

/// An integer word the original stores a number in: its width and sign.
pub trait Word: Copy + Default + Debug + PartialEq + Eq + PartialOrd + Ord + Hash + Send + Sync + 'static {
    const BITS: u32;
    const SIGNED: bool;
    const MIN: i64;
    const MAX: i64;
    /// The value.
    fn get(self) -> i64;
    /// The low `BITS` bits of `v`, as a store to this width keeps them.
    fn wrap(v: i64) -> Self;
}

macro_rules! word {
    ($($t:ty),*) => {$(
        impl Word for $t {
            const BITS: u32 = <$t>::BITS;
            const SIGNED: bool = <$t>::MIN != 0;
            const MIN: i64 = <$t>::MIN as i64;
            const MAX: i64 = <$t>::MAX as i64;
            fn get(self) -> i64 {
                self as i64
            }
            fn wrap(v: i64) -> Self {
                v as $t
            }
        }
    )*};
}

word!(i8, u8, i16, u16, i32, u32);

/// C does arithmetic on two operands as `unsigned int` when one of them is
/// a `u32`, and as `int` otherwise (narrower words are promoted).
pub(crate) fn unsigned<A: Word, B: Word>() -> bool {
    (A::BITS == 32 && !A::SIGNED) || (B::BITS == 32 && !B::SIGNED)
}

/// `(a * b) >> shift` as compiled C does it: the low 32 bits of the product,
/// then an arithmetic shift (`int`) or a logical one (`unsigned`).
pub(crate) fn c_mul(a: i64, b: i64, unsigned: bool, shift: u32) -> i64 {
    if unsigned {
        ((a as u32).wrapping_mul(b as u32) >> shift) as i64
    } else {
        ((a as i32).wrapping_mul(b as i32) >> shift) as i64
    }
}

/// `(a << shift) / b` as compiled C does it on the R3000: the shift keeps
/// 32 bits, the quotient truncates toward zero, and a zero divisor gives
/// what `div` and `divu` leave in LO.
pub(crate) fn c_div(a: i64, b: i64, unsigned: bool, shift: u32) -> i64 {
    if unsigned {
        let (n, d) = ((a as u32) << shift, b as u32);
        n.checked_div(d).unwrap_or(u32::MAX) as i64
    } else {
        let (n, d) = ((a as i32) << shift, b as i32);
        (if d == 0 { if n >= 0 { -1 } else { 1 } } else { n.wrapping_div(d) }) as i64
    }
}
