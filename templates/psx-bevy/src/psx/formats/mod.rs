//! Original PlayStation file formats. `tim.rs` and `tmd.rs` are copies of the
//! psx toolkit's `crates/psx-assets/src/` files (kept identical by a test
//! there); edit them in the toolkit and copy them over.
#![allow(dead_code)] // complete format APIs; games use what they need

pub mod tim;
pub mod tmd;
