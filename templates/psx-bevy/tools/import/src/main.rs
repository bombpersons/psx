//! `cargo run -p import -- DISC [OUT]`: imports from a disc image into OUT
//! (default `game/assets/imported`), as the game does on first start.
//! `cargo run -p import -- provenance <source> <output>` records where a file
//! came from.
use import::{DiscReader, ImportProgress, Provenance};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["provenance", source, output] => {
            let written = Provenance::of(Path::new(source), "provenance")
                .and_then(|p| p.write_beside(Path::new(output)))
                .unwrap_or_else(|e| fail(&e.to_string()));
            println!("{}", written.display());
        }
        [disc] | [disc, _] => {
            let out = Path::new(args.get(1).map_or("game/assets/imported", String::as_str));
            let reader = DiscReader::open(Path::new(disc)).unwrap_or_else(|e| fail(&e));
            if !reader.accepted() {
                fail(&format!("{disc} is {}, not a disc import::SERIALS accepts", reader.serial));
            }
            std::fs::create_dir_all(out).unwrap_or_else(|e| fail(&format!("{}: {e}", out.display())));
            import::import(&reader, out, &ImportProgress::default()).unwrap_or_else(|e| fail(&e));
            println!("imported {} ({}) into {}", disc, reader.serial, out.display());
        }
        _ => fail("usage: import DISC [OUT] | import provenance <source> <output>"),
    }
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2)
}
