//! PS1-style games in Bevy: the PS1 look ([`psx`]), logic ticks and
//! input ([`shell`], [`controller`]), numbers for exact mechanics ([`num`])
//! and the GTE ([`gte`]), the controls and display menus
//! ([`controls`], [`display`]), headless testing ([`testing`]), the capture
//! viewer, the developer MCP (feature `dev-mcp`), layered asset
//! folders ([`assets`]) and the [`launcher`] that wires them into an app.
//!
//! A library, used by the psx toolkit's game template and by any Bevy game.
//! Nothing here knows about a particular game.

// A binary built with `dynamic` needs Bevy's shared library beside it, which
// only `cargo run` and `cargo test` find. Shipping builds link it statically.
#[cfg(all(feature = "dynamic", not(debug_assertions)))]
compile_error!("release builds link Bevy statically: cargo build --release --no-default-features");
pub mod assets;
pub mod controller;
pub mod controls;
#[cfg(feature = "dev-mcp")]
pub mod dev_mcp;
pub mod display;
pub mod gte;
pub mod launcher;
/// Numbers for exact mechanics: `vendor/psx-fixed`.
pub use psx_fixed as num;
pub mod pointer;
pub mod psx;
pub mod replay;
pub mod settings;
pub mod shell;
pub mod testing;
pub mod viewer;

/// What game code usually needs from psx-bevy.
pub mod prelude {
    pub use crate::controller::{Button, ControllerState};
    pub use crate::gte::GteAngle;
    // Not the vectors: Bevy's prelude has its own Vec2 and Vec3.
    pub use crate::num::{Angle, Exact, Fixed, Float, Mode, Num};
    pub use crate::psx::Region;
    pub use crate::shell::{GameClock, GameSystems, TickRate};
}
