//! PS1-style games in Bevy: the PS1 look ([`psx`]), logic ticks and
//! input ([`shell`], [`controller`]), the controls and display menus
//! ([`controls`], [`display`]), headless testing ([`testing`]), the capture
//! viewer, the developer MCP (feature `dev-mcp`), layered asset
//! folders ([`assets`]) and the [`launcher`] that wires them into an app.
//!
//! A library, used by the psx toolkit's game template and by any Bevy game.
//! Nothing here knows about a particular game.
pub mod assets;
pub mod controller;
pub mod controls;
#[cfg(feature = "dev-mcp")]
pub mod dev_mcp;
pub mod display;
pub mod launcher;
pub mod pointer;
pub mod psx;
pub mod settings;
pub mod shell;
pub mod testing;
pub mod viewer;

/// What game code usually needs from psx-bevy.
pub mod prelude {
    pub use crate::controller::{Button, ControllerState};
    pub use crate::psx::Region;
    pub use crate::shell::{GameClock, GameSystems, TickRate};
}
