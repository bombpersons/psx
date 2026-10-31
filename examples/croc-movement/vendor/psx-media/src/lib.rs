//! Static PlayStation media inspection: CUE/BIN, ISO and CHD disc images, CD-ROM sectors,
//! ISO9660 file systems, `SYSTEM.CNF` and PS-X EXE executables.
//!
//! Nothing here executes code. Results describe bytes in the supplied files;
//! what a game's loader does with them at runtime needs emulator evidence.

mod chd;
pub mod cue;
pub mod disc;
pub mod error;
pub mod exe;
pub mod iso9660;
mod le;
pub mod memmap;
pub mod source;
pub mod str_video;
pub mod subq;
pub mod system_cnf;

pub use disc::Disc;
pub use error::{MediaError, Result};
pub use exe::PsxExe;
