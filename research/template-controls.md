# Template controls menu (2026-10-16)

The shared Bevy starter includes physical keyboard/gamepad rebinding and a
reusable controls menu, independent of its demo. Gameplay continues to consume
logical `ControllerState` actions. Integration and settings paths are documented
in the [template README](../templates/psx-bevy/README.md#rebinding-and-the-example-controls-menu).

![Controls menu rendered at 320×240](../docs/images/template-controls.png)

The editor keeps a draft until Save succeeds. Conflicts offer an explicit swap;
cancel, clear, restore defaults and discard preserve live mappings until then.
The versioned JSON store validates inputs, bounds reads to 64 KiB and stages
writes beside the destination before replacing it. A failed save keeps the draft
open. Invalid profiles stay untouched until an explicit save repairs them.

The shell samples mapped physical input before fixed ticks and retains short
taps. Menu capture clears physical, injected and latched inputs, suspends ticks
and waits for neutral physical input on close. Capture requires release before
listening, rejects simultaneous candidates and rearms when the selected gamepad
changes. Window focus loss discards the draft. Pending developer steps are
interrupted with an error; input/step requests are rejected while focus is held.

Validation commands (run in `templates/psx-bevy` unless indicated):

```sh
cargo test
cargo test --features dev-mcp
cargo build
cargo build --features dev-mcp
cargo fmt --check
cargo clippy --features dev-mcp --all-targets -- -D warnings
cargo test --features dev-mcp --test dev_mcp controls_menu -- --ignored --nocapture
# At the toolkit root:
cargo test -p psx template::tests
```

The rules tests exercise keyboard edits through persistence/reload, conflict
reject/swap, clearing, restoring custom defaults, discarding, gamepad-only
navigation through save and resumed gameplay, confirmation-button release gates,
ambiguous presses, gamepad reconnects, focus loss, malformed/oversized profiles
and failed-save recovery. The developer build also checks pending-step
interruption. Generator tests verify that every starter file is embedded.

The offscreen software-GPU test launches the actual game with `--controls`,
captures the screen, checks visible text and the unclipped bottom border, verifies MCP input/step errors and
confirms zero simulation ticks and no settings file creation. The screenshot
above was inspected: every row, footer and border fits the 320×240 canvas.
The first render clipped the bottom help line; reducing row/header spacing fixes
it without removing controls.

Scope: digital keys/buttons, including triggers, are rebindable; analog axes keep
their meaning and configurable deadzone. The example reserves Esc and, with
global shortcuts enabled, F1 and Start+Select. Gamepad tests use Bevy gamepad
components rather than physical hardware; the rendered check ran on Linux with a
software adapter. This does not claim physical-device or other-platform testing.
