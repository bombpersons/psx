//! UNR reciprocal division (RTPS/RTPT) and the LZCR leading-bit counter.

/// The 257-entry reciprocal seed table:
/// `max(0, (0x40000 / (i + 0x100) + 1) / 2 - 0x101)`.
pub const UNR_TABLE: [u8; 257] = {
    let mut t = [0u8; 257];
    let mut i = 0;
    while i < 257 {
        let v = (0x40000 / (i as i32 + 0x100) + 1) / 2 - 0x101;
        t[i] = if v < 0 { 0 } else { v as u8 };
        i += 1;
    }
    t
};

/// GTE perspective division `H / SZ3` as an unsigned `0.1.16` fixed-point
/// value (`0x10000` = 1.0), using the hardware's Newton-Raphson reciprocal.
///
/// Returns `(quotient, overflow)`. When `h >= 2 * sz3` (including `sz3 == 0`)
/// the result is `0x1FFFF` and `overflow` is set (FLAG bit 17). The result is
/// within ±3 of the ideal `h * 0x10000 / sz3`, but bit-exact to hardware.
pub fn unr_divide(h: u16, sz3: u16) -> (u32, bool) {
    if h as u32 >= (sz3 as u32) * 2 {
        return (0x1FFFF, true);
    }
    // Normalise so bit 15 of the divisor is set.
    let z = sz3.leading_zeros();
    let n = (h as u64) << z;
    let d = (sz3 as i64) << z;
    let u = UNR_TABLE[((d - 0x7FC0) >> 7) as usize] as i64 + 0x101;
    let d = (0x200_0080 - d * u) >> 8; // always 0..0x20000 for d in 0x8000..=0xFFFF
    let d = (0x80 + d * u) >> 8;
    let q = (n * d as u64 + 0x8000) >> 16;
    (q.min(0x1FFFF) as u32, false)
}

/// LZCR: leading zero count if `lzcs` is non-negative, otherwise leading one
/// count. Range 1..=32.
pub const fn lzcr(lzcs: u32) -> u32 {
    if (lzcs as i32) < 0 {
        lzcs.leading_ones()
    } else {
        lzcs.leading_zeros()
    }
}
