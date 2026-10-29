//! PsyQ libgte-style fixed-point helpers (angles: 4096 = 360°, `1.3.12`).
//!
//! These are pure functions; they do not touch a [`crate::Gte`].
//!
//! Exactness notes:
//! * [`rsin`]/[`rcos`] use `round(4096 * sin(2π·a/4096))`. This matches
//!   libgte's quarter-wave table entry for entry: all 1025 entries of the
//!   table that PsyQ 4.0 `rsin` reads in Croc's PAL `CROC.EXE` (at
//!   0x8006D5D4 in RAM) equal it (research/case-study-croc.md).
//! * [`mul_matrix`] and [`apply_matrix_sv`] reproduce what libgte's
//!   GTE-based `MulMatrix0`/`ApplyMatrixSV` compute (MVMVA, sf=1, lm=0),
//!   so they are bit-exact given the same inputs.
//! * [`rot_matrix`] composes `Rx · Ry · Rz` via [`mul_matrix`]; element
//!   rounding may differ from libgte `RotMatrix` by ±1 LSB.

use crate::types::{Matrix, Vec3i16, Vec3i32};

/// Quarter-wave sine table: `round(4096 * sin(i * π/2 / 1024))`, i in 0..=1024.
pub const SIN_QUARTER: [i16; 1025] = {
    const HALF_PI: f64 = core::f64::consts::FRAC_PI_2;
    let mut t = [0i16; 1025];
    let mut i = 0;
    while i <= 1024 {
        // Taylor series; x <= π/2 so 15 terms are far below f64 epsilon.
        let x = i as f64 * HALF_PI / 1024.0;
        let mut term = x;
        let mut sum = x;
        let mut n = 1;
        while n < 15 {
            term = -term * x * x / ((2 * n) as f64 * (2 * n + 1) as f64);
            sum += term;
            n += 1;
        }
        t[i] = (sum * 4096.0 + 0.5) as i16;
        i += 1;
    }
    t
};

/// Sine of `angle` (4096 = 360°), result `1.3.12`. Like libgte `rsin`.
pub const fn rsin(angle: i32) -> i32 {
    let a = (angle & 0xFFF) as usize;
    (match a >> 10 {
        0 => SIN_QUARTER[a],
        1 => SIN_QUARTER[2048 - a],
        2 => -SIN_QUARTER[a - 2048],
        _ => -SIN_QUARTER[4096 - a],
    }) as i32
}

/// Cosine of `angle` (4096 = 360°), result `1.3.12`. Like libgte `rcos`.
pub const fn rcos(angle: i32) -> i32 {
    rsin(angle.wrapping_add(1024))
}

/// Rotation about X: `[1 0 0; 0 c -s; 0 s c]`.
pub const fn rot_x(angle: i32) -> Matrix {
    let (s, c) = (rsin(angle) as i16, rcos(angle) as i16);
    Matrix::new([[0x1000, 0, 0], [0, c, -s], [0, s, c]])
}

/// Rotation about Y: `[c 0 s; 0 1 0; -s 0 c]`.
pub const fn rot_y(angle: i32) -> Matrix {
    let (s, c) = (rsin(angle) as i16, rcos(angle) as i16);
    Matrix::new([[c, 0, s], [0, 0x1000, 0], [-s, 0, c]])
}

/// Rotation about Z: `[c -s 0; s c 0; 0 0 1]`.
pub const fn rot_z(angle: i32) -> Matrix {
    let (s, c) = (rsin(angle) as i16, rcos(angle) as i16);
    Matrix::new([[c, -s, 0], [s, c, 0], [0, 0, 0x1000]])
}

/// Rotation matrix from angles (libgte `RotMatrix` convention:
/// `M = Rx(r.x) · Ry(r.y) · Rz(r.z)`, so a vector is rotated about Z first).
/// See the module docs for exactness.
pub fn rot_matrix(r: Vec3i16) -> Matrix {
    let yz = mul_matrix(&rot_y(r.y as i32), &rot_z(r.z as i32));
    mul_matrix(&rot_x(r.x as i32), &yz)
}

/// Saturate like an IR register with lm=0.
fn sat16(v: i64) -> i16 {
    v.clamp(-0x8000, 0x7FFF) as i16
}

/// `a · b`, each element `(Σ a·b) >> 12` saturated to i16 (libgte `MulMatrix0`).
pub fn mul_matrix(a: &Matrix, b: &Matrix) -> Matrix {
    let mut out = Matrix::ZERO;
    for r in 0..3 {
        for c in 0..3 {
            let s: i64 = (0..3).map(|k| a.m[r][k] as i64 * b.m[k][c] as i64).sum();
            out.m[r][c] = sat16(s >> 12);
        }
    }
    out
}

/// `(m · v) >> 12` without saturation (libgte `ApplyMatrix`, MAC1-3).
pub fn apply_matrix(m: &Matrix, v: Vec3i16) -> Vec3i32 {
    let v = v.to_array();
    let row = |r: usize| ((0..3).map(|k| m.m[r][k] as i64 * v[k] as i64).sum::<i64>() >> 12) as i32;
    Vec3i32::new(row(0), row(1), row(2))
}

/// `(m · v) >> 12` saturated to i16 (libgte `ApplyMatrixSV`, IR1-3).
pub fn apply_matrix_sv(m: &Matrix, v: Vec3i16) -> Vec3i16 {
    let r = apply_matrix(m, v);
    Vec3i16::new(sat16(r.x as i64), sat16(r.y as i64), sat16(r.z as i64))
}
