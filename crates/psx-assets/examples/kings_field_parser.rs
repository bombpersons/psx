//! Worked external parser. Build with cargo build -p psx-assets --example
//! kings_field_parser, then use target/debug/examples/kings_field_parser with
//! psx assets scan/extract --model-parser. No game data is included.

#[path = "kings_field/parser.rs"]
mod parser;

use psx_assets::model_document::ModelDocument;
use std::io::{Read, Write};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("source exceeds 64 MiB".into());
    }
    let normalized = parser::normalize_kings_field(&bytes)?;
    // Optional standalone normalization: printf/cat source bytes into this binary
    // with --tmd-out PATH. Normal toolkit invocations only need JSON on stdout.
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !args.is_empty() {
        if args.len() != 2 || args[0] != "--tmd-out" {
            return Err("usage: kings_field_parser [--tmd-out PATH]".into());
        }
        std::fs::write(&args[1], &normalized.bytes)?;
    }
    serde_json::to_writer(
        std::io::stdout().lock(),
        &ModelDocument::from(&normalized.model),
    )?;
    std::io::stdout().flush()?;
    Ok(())
}
