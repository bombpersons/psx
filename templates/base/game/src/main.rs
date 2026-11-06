//! The executable. `--psx` serves the port to psx over the port protocol
//! (`psx port check`, psx web's replays), and `--replay REC --frames DIR`
//! draws a scenario recording here (psx_scenario::replay); anything else is
//! the game's own to run.
use std::process::ExitCode;

fn main() -> ExitCode {
    if let Some(code) = psx_scenario::replay::main(std::env::args(), game::Game::headless) {
        return code;
    }
    println!(
        "{} has no window or game loop yet: the port is the library (src/lib.rs), \
         and its scenarios run with `cargo test -p game --test scenarios`. \
         `psx port add full` adds a window, a renderer and the disc import.",
        env!("CARGO_BIN_NAME")
    );
    ExitCode::SUCCESS
}
