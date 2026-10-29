//! Bit-accurate PlayStation 1 GTE (Geometry Transformation Engine, COP2).
//!
//! Two layers:
//!
//! * [`Gte`] with [`Gte::read_data`], [`Gte::write_data`], [`Gte::read_ctrl`],
//!   [`Gte::write_ctrl`] and [`Gte::execute`] — an emulator-faithful core
//!   (MFC2/MTC2/CFC2/CTC2 semantics, all 22 commands, FLAG bits, UNR division).
//! * Typed helpers on the same struct ([`Gte::set_rotation`], [`Gte::rtps`],
//!   [`Gte::rtpt`], [`Gte::ncds`], ...) plus libgte-style free functions in
//!   [`math`] for native game code.
//!
//! # Fixed-point formats
//!
//! Notation `s.i.f` = sign, integer bits, fraction bits.
//!
//! | Quantity                         | Format          | 1.0 equals |
//! |----------------------------------|-----------------|------------|
//! | Matrix elements (RT, LLM, LCM)   | `1.3.12` (i16)  | `0x1000`   |
//! | Vertices V0-V2                   | `1.15.0` (i16)  | `1`        |
//! | Translation TR                   | `1.31.0` (i32)  | `1`        |
//! | Back color BK, far color FC      | `1.19.12` (i32) | `0x1000`   |
//! | IR1-IR3 (after sf=1 ops)         | `1.3.12` / int  | `0x1000`   |
//! | IR0 (depth-cue factor)           | `1.3.12`, 0..=0x1000 | `0x1000` |
//! | OFX, OFY (screen offset)         | `1.15.16` (i32) | `0x10000`  |
//! | H (projection distance)          | `0.16.0` (u16)  | `1`        |
//! | DQA                              | `1.7.8` (i16)   | `0x100`    |
//! | DQB                              | `1.7.24` (i32)  | `0x1000000`|
//! | ZSF3, ZSF4                       | `1.3.12` (i16)  | `0x1000`   |
//! | SX, SY (screen)                  | i16, saturated to -0x400..=0x3FF |  |
//! | SZ, OTZ                          | u16, saturated to 0..=0xFFFF |    |
//! | RGB colors                       | u8 per channel  | `255` = full |
//!
//! Internally sums are computed in a 44-bit accumulator (MAC); each addition
//! is overflow-checked (FLAG bits 30-25) and wrapped to 44 bits. With `sf=1`
//! the result is shifted right by 12 before landing in MAC1-3 (i32), then
//! saturated into IR1-3 (i16, range `-0x8000..=0x7FFF`, or `0..=0x7FFF`
//! with `lm=1`).
//!
//! # Test vectors for future fixture tests
//!
//! Not yet wired in, but known sources of hardware-verified results:
//! * amidog's `gte_test` (psxtest_gte) — hardware result logs widely used by
//!   mednafen/duckstation/pcsx-redux authors.
//! * JaCzekanski `ps1-tests` (`gte/test-all`, `gte/irgb`, `gte/lzcr`,
//!   `gte/rtps-*`), which ship expected-output logs captured on real hardware.
//! * pcsx-redux `src/mips/tests/gte` (GPL; usable only as a behavioral oracle).

#![forbid(unsafe_code)]

mod gte;
pub mod math;
mod types;
mod unr;

#[cfg(test)]
mod tests;

pub use gte::{Gte, RtpsResult, RtptResult, flag, op};
pub use types::{Matrix, Rgbc, ScreenXY, Vec3i16, Vec3i32};
pub use unr::{UNR_TABLE, lzcr, unr_divide};
