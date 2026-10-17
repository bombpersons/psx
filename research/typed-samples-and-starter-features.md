# Typed samples and starter feature alignment, 2026-10-17

The last two interface items from the
[Cyber Egg review](cyber-egg-feedback-review.md#remaining-interface-work).

## Typed sample descriptors

The study reported that `ADDR:4` works for watches but was rejected for
samples, an easy mistake to make over MCP. Two changes:

- `run`'s `sample` entries accept a typed `{"addr", "format", "name"}` object
  next to the existing `ADDR:FMT[:NAME]` text. `format` defaults to `u32`;
  unknown fields are refused with the field named. The MCP schema publishes both
  forms (`SampleSpec`: string or `TypedSample`).
- A watch-style byte width (`1`, `2`, `4`, as text or a JSON number in the typed
  form) reads unsigned, so `ADDR:4` now samples as `u32`. The default column name
  shows the resolved format (`0x8015E8A8:u16`).

Both forms go through the same address, format, memory-map and name checks.

Validation:

- Unit tests cover typed/text parity, defaults, scratchpad addresses, numeric
  widths, an invalid width (3), unknown fields, wrong JSON types, unknown formats
  and names containing commas.
- On the Mega Man 8 s3900 checkpoint, a 20-frame MCP run with two typed samples
  (one with `"format": 1`) and one text sample wrote a `samples.csv`
  byte-identical to the CLI run with `--sample 8015E8AC:fx16.16:y --sample
  8015E89E:u8:state --sample 8015E8B4:fx16.16:vy`.
- Tool definitions stay inside the 50 KB budget test.

## Starter dependency features

`cargo tree -e features` with and without `--features dev-mcp` showed that the
MCP crates enable features on dependencies Bevy shares:

| Crate | Enabled by | Feature |
|---|---|---|
| serde, serde_core | rmcp | `rc` |
| serde_json | schemars | `alloc` |
| futures-util, futures-channel, futures-task | rmcp (`futures`) | `std`, `io`, `sink`, `channel` |
| futures-sink | tokio-util | `default` |
| base64 0.23 | rmcp | `default`, `simd-unsafe` |

A different feature set changes a crate's build hash, so that crate and every
crate above it recompile. serde sits under most of Bevy, and futures-util under
`bevy_asset`. The template now enables these features in every build
(`serde/rc`, `serde_json/alloc`, `futures/std`, `futures-sink`, and its own
`base64` moved from an optional 0.22 to 0.23). Afterwards the only shared-graph
difference is the `futures` facade's own features, which nothing in Bevy uses.

Measured on fresh copies of the starter, each with its own target directory
(12 threads, `cargo build --offline`):

| Step | Before | After |
|---|---|---|
| Cold default build | 365 crates, 216 s | 368 crates, 241 s |
| First switch to `--features dev-mcp` | 135 crates (41 Bevy), 169 s | 24 crates (0 Bevy), 36 s |
| Back to default, then dev-mcp again | 0 crates | 0 crates |

The 24 are rmcp, tokio, schemars, darling and their helpers, host copies of
serde_core/serde_json for those proc-macros, and the game. Cold-build time varies
between runs; the three extra default-build crates are futures, futures-channel
and futures-sink. Cargo keeps both builds once made, so the reported cost was
always the first switch. The study's 9 m 34 s switch listed the same kind of
cascade (bevy_ecs, bevy_asset, image and mesh crates). Release builds remain a
separate profile.

There is no automated test of the alignment: it needs the template's registry
dependencies resolved, which the toolkit test suite does not require. The
README says to recheck `cargo tree -e features` when adding dependencies.
