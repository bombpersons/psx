# A native port of a PlayStation game

A standalone Rust workspace for a native port, made with the psx toolkit's
`psx port init`. The template holds only what connects the port to psx:

```sh
cargo test --workspace                   # the scenarios and the importers' tests
cargo test -p game --test scenarios      # the port against the original's recordings
cargo run -p game -- --replay scenarios/<id>/<name>.jsonl --frames DIR   # a recording as the port draws it
cargo run -p import -- DISC [OUT]        # the import by hand, into game/assets/imported
```

| Path | Owner | What |
|---|---|---|
| `game/` | this project | The port: `Game` implements `psx_scenario::Port` ([AGENTS.md](AGENTS.md)) |
| `tools/import/` | this project | Importers: the player's disc → the port's formats, with provenance |
| `glossary/` | this project | The quantities scenarios compare |
| `vendor/` | the template | psx's crates, vendored: `psx port update` replaces them; fix bugs upstream |
| `addons/` | this project | Crates `psx port add` wrote: a window, a renderer, the disc import, menus |

## The original's data

The repository never holds the original's data. The importers in
`tools/import` turn the player's disc into the port's own formats; the
port reads only those.

## Template updates

```sh
psx port update --dry-run
psx port update             # replace unmodified vendor/ files
```

Files under `vendor/` that you haven't changed are replaced; one you changed
gets the new version beside it as `<file>.psx-new` to merge. Files the
template seeded (`game/`, `tools/`, `glossary/`, this README) are the
project's: a newer template version of one is copied under
`.psx/template-update/` and reported, never written over yours.
