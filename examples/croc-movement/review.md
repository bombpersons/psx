+++
commit = "27038d0d11144ac834cb6a1725b353e0bfd229ab"
+++

## The character's body is welded to the player: split a generic `character` module from `croc`

`Position`, `Facing`, `Vertical`, `Ground`, `Clip` and the systems that
drive them (`animate`, `move_vertically`, `follow_floor`, `collide`) are
written as Croc's: they live in `croc/mod.rs` and `croc/vertical.rs`, every
query is `With<Croc>`, and their order is `CrocStep`. None of them is
specific to the player. An enemy that jumps, a falling crate or a second
playable character would need the same rise, fall, floor snap and
animation clock, and today they would have to be copied or they would have
to carry the `Croc` marker.

I would have a `character` (or `body`) module that owns these components
and a `Character` marker (or no marker: the systems query the components
they need), with its own `CharacterPlugin`. `Croc` then becomes a player
marker that `#[require]`s `Character` plus the player-only parts (`Motion`,
the jump and stomp state, the action), and `croc/` keeps only what is the
player's: reading the pad into running, the jump test, the stomp. The
world's `collide` and `find_floor` query `Ground` on any character, not on
Croc. `Observed` stays Croc's (it filters on the player marker).

**Answer:** Done. `game/src/character/` owns `Character` (requiring `Position`, `Facing`, `Vertical`, `Ground`, `Clip`), `BodyTuning` and the systems `animate`, `move_vertically` and `follow_floor`, which query components with no player marker. `Croc` requires `Character` plus his own parts (`Action`, `Motion`, `JumpState`). `collide` and `find_floor` work on any `Ground`, and the body's tunings moved to `assets/character/`. The vertical motion no longer reads the pad. Whether a rise is still held is a `RiseHeld` component, which the jump sets in `TickPhase::Input`. A body with no controller keeps the default, held.

## `CrocStep` names every feature: make the tick phases generic and let features order themselves inside them

`CrocStep` is one central enum with a variant per ability (`Move`,
`JumpTest`, `StompTest`, `Turn`) next to the general phases (`Animate`,
`Vertical`, `Collide`). Adding an ability (the tail swipe, swimming, a
ledge grab) means editing this enum and its `configure_sets` chain in
`CrocPlugin`, which is the central list the rest of the code otherwise
avoids. The names also mirror the original's script layout (`Prelude`,
`Switch`, "the jump test (script sub 0x1803)") rather than describing what
runs there.

I would keep a short engine-level chain of phases named for what they do,
for example `Animate -> Physics -> Reactions -> Control -> Actions ->
Timers -> Collide` (the floor snap and the switch latch in their own early
phase), and let each ability module own a `SystemSet` of its own inside
`Control`, ordered relative to the others with `.after(...)` where the
original's order matters (stomp test after jump test, turn after move).
The order is unchanged; only who declares it moves, so a new ability is a
new module and a plugin, not an edit to `croc/mod.rs`.

**Answer:** Done. `game/src/tick.rs` has `TickPhase` (`Input, Animate, Physics, Reactions, Ground, Latch, Control, Actions, Timers, Collide`), named for what runs there. Each ability declares its own set inside `Control`: `RunningSet::{Walk, Turn}`; `JumpTest` after `Walk` and before `Turn`; `StompTest` after `JumpTest` and before `Turn`. A new ability is now a module plus a set, with no change to a central enum. The order is unchanged and the recordings pass.

## `Vertical` is a bag of flags: make the state an enum and move the jump's fields to the jump

`Vertical` holds `motion: VerticalMotion` plus `on_ground: bool`, whose
combination encodes four states (standing; rising; falling; hanging is
"neither, off the ground"), and the docs have to explain which pairs mean
what (`rising_or_falling` versus `!on_ground`). It also carries
`variable_height` and `from_jump`, which only the jump and the stomp read,
and `landed`, a one-tick event stored as a field that `move_vertically`
clears first thing each tick.

I would write the state as one enum, `Vertical::{Grounded, Rising {
speed, height_left }, Falling { speed }, Hanging }`, so impossible
combinations cannot be written and `jump`, `fall`, `hang` are plain
transitions; move `variable_height` and `from_jump` into a jump-owned
component (`JumpState`) next to the jump's timers; and make the landing
an explicit one-tick signal (a `Landed` marker component inserted by the
fall and removed at the start of the next physics phase, or a Bevy message
handled in `Reactions`). Handlers of the landing (`jump::land`, the
stomp's drop) then say what they react to instead of polling a field.

**Answer:** Done, in part. The state is one enum, `VerticalState::{Grounded, Rising, Falling, Hanging}`, so the impossible combinations can't be written. `variable_height` and `from_jump` moved to the jump's `JumpState`. The landing is a `Landed` marker, inserted by the fall and removed by the next tick's physics. `jump::land` queries `With<Landed>`. Kept: `speed` and `height_left` stay as fields beside the state, not inside its variants. The original keeps both across states, and the recordings observe them there: a hang keeps the speed it stopped at, and the stomp's drop and landing keep the jump's height left (`croc_jump_height_left`). Moving them into variants would drop values the scenarios compare.

## `Timers` and the shared countdown belong to the jump

`Timers` lives in the Croc core with fields named for the jump
(`jump_lockout`, `jump_buffer`), and `count_down` in `croc/mod.rs` runs
them down. It is a "script locals" bucket shaped by the original rather
than a component any one feature owns, and the next countdown (the attack
lockout the stomp notes) would be added to the same bucket.

I would move it to `jump.rs` as `JumpTimers` (or fold it into the
`JumpState` above), counted down by `JumpPlugin` in the `Timers` phase.
The stomp clearing the buffered press then reads as the stomp using the
jump's component, which is the real dependency.

**Answer:** Done. The countdowns are fields of `JumpState` in `croc/jump.rs`, and `JumpPlugin` counts them down in `TickPhase::Timers`. The stomp clearing the kept press now reads as the stomp using the jump's component.

## Action and `ThisTick` as two components, a central enum and a numbering

`Action` is a closed enum of every action the player has, `ThisTick` is a
copy of it taken by `switch`, and the stomp's progress is a separate
component `StompStep` that every Croc carries whether or not he is
stomping (registered as a required component). `Action::number()` puts the
glossary's numbering (1, 4) in the type itself.

I would make the action one component holding both halves of the latch,
`Action { current, next }` (or `Action` plus a `PendingAction` that the
latch applies), so the "starts from the next tick" rule is visible in one
type and one system. The action's own data should travel with it: either
`ActionKind::Stomp(StompStep)`, so a step exists only while stomping, or
one component per action (`Stomping { step }`) inserted when it starts,
which also lets a new action be added from its own module without editing
a central enum. The glossary number belongs in the observe closure, where
`Gait` is already mapped to `glossary::Gait`.

**Answer:** Done, in part. `Action { current, next }` is one component. `Action::start` sets `next`, and the `latch` system in `TickPhase::Latch` applies it, so the next-tick rule lives in one type and one system. The stomp's step is a `Stomp` component, present only while Croc stomps: inserted by the stomp test and removed when the stomp ends. The glossary numbers (1, 4) moved into the observe closure. Kept: `ActionKind` stays a closed enum in `croc/`. It is Croc's own switch (his script's global 0), and one action at a time is the rule it expresses. Another character would have its own controller and its own enum. An open registry would only be worth it if mods added actions to Croc.

## The stomp body reads as a transcription: write the step machine as a `match`

`stomp` checks its steps with a descending sequence of `if *step == X`
blocks, so that a step changed in one block is seen by a later block in
the same tick. Which transitions may chain within a tick (only settle ->
hang -> drop, as the module doc explains) is implicit in the block order;
a reader has to simulate the cascade to see it, and reordering the blocks
silently changes behaviour. `Stomper` and `StompTrigger` are two
near-identical `QueryData` structs.

I would write one `match *step` that computes the next step, with the one
chaining case written out explicitly (the settle that goes straight into a
second stomp falls through to the hang's check in code, with a comment
that it is the only one). `start_stomp` can take a plain tuple query; one
named `QueryData` is enough if it is wanted at all.

**Answer:** Done. `stomp` is one `match` on `Stomp` that returns the next step. The one step that moves twice in a tick (settle, jump held, still high) is its own arm. It goes straight to the drop, with a comment. `start_stomp` takes a plain tuple query; `Stomper` remains as the one named `QueryData`, because the tuple form trips clippy's `type_complexity`.

## Hidden coupling between the Croc plugins

`RunningPlugin` and `StompPlugin` both `try_register_required_components::
<Croc, Motion>().ok()` because either may come first; `Croc`'s
`#[require]` lists components that the features own (`Timers`, `Clip`,
`Action`); `CrocPlugin` adds `VerticalPlugin` itself while `LogicPlugins`
adds running, jump and stomp separately. Stomp needs running's `Motion`
and jump's timers but nothing says so, and removing `RunningPlugin` from
the group would leave the stomp's `motion.stop()` working on a component
nothing else updates.

I would make `CrocPlugin` a `PluginGroup` (or one plugin) that adds its
abilities, with each component registered once by the module that owns it
(`Motion` by running; the stomp declares it needs running, for example by
asserting `is_plugin_added::<RunningPlugin>()` or by adding it). The
general "stop dead" belongs with the body (see the first finding), so the
stomp does not reach into running at all.

**Answer:** Done, in part. `CrocPlugin` adds `RunningPlugin`, `JumpPlugin` and `StompPlugin`, and `LogicPlugins` lists only `TickPlugin`, `WorldPlugin`, `CharacterPlugin` and `CrocPlugin`. Each component is required once, by `Croc` (`Motion`, `JumpState`, `Action`) or by `Character`, and the `try_register_required_components` calls are gone. The stomp's dependencies are explicit imports and set orderings (`after(JumpTest)`, `before(RunningSet::Turn)`). Kept: the stomp still calls `Motion::stop`. Opcode 0xA0 zeroes the whole ground-movement state (speeds, gait, walk count, turn rate), which is running's state, not the body's. Croc's abilities are added together, so stomp without running isn't a configuration.

## Tuning is one global resource per type, loaded around the asset server

`TuningPlugin<T>` reads RON synchronously with `std::fs` into a single
`Resource` per tuning type, and each tuning type hardcodes its own path
constant. Every character reads `Res<RunningTuning>`, so two characters
cannot have different tunings (an enemy that jumps lower, a second player
with other speeds), and the files bypass Bevy's `AssetServer`, so there is
no hot reload while tuning and mods only layer through
`psx_bevy::assets::find`.

I would make tunings assets (`Asset` + a RON loader, or
`bevy_common_assets`), referenced per character by a handle component
(`TuningHandle<RunningTuning>`) set when the character is spawned from its
placement or archetype file. Headless tests can still load them
synchronously (a blocking load helper in tests), and validation moves into
the loader. With one player this costs little now and avoids a rewrite of
every system signature when the second character arrives.

**Answer:** Kept. A tuning is per object type, which is how the original holds these numbers (constants in Croc's script and opcodes) and how AGENTS.md asks for them: "a per-type config asset". A second character type would have its own controller and tuning type. Another instance of Croc's type shares Croc's numbers, as it would in the original. The body's own tunings (step, rise and fall) moved to `assets/character/` with the body. Loading synchronously is deliberate. The scenarios build the world in a launcher and compare from tick 0, so an asynchronous `AssetServer` load would need a wait state in every launcher and test. `config.rs` notes the move to asset loaders for when content needs streaming or hot reload.

## The level is a resource that spawns the player: make it a level description with placements and a floor query

`FlatLevel` is a struct of `floor_y, start_x, start_z, start_angle`;
`build` inserts a `FlatFloor` resource and spawns Croc directly. Collision
is "set every `Ground` to the floor's height", written twice (`collide`
and the `find_floor` observer). The level format names the player, holds
its start as loose fields, and has no room for a second object; the world
module imports `Croc` to spawn it. `dev_mcp.rs` also repeats the start
position from `start.ron` as a literal.

I would describe a level as geometry plus a list of placements
(`placements: [(kind: "player", position: (x, y, z), facing: 94)]`), with
the game registering what each kind spawns, so the world module does not
know about Croc. Collision would be a query the world offers
(`Floor::height_under(position) -> Option<i32>`, or a `Collision` resource
with that method), implemented by the flat floor now and by real geometry
later; `collide` and `find_floor` both call it. The test can read the
start from the level file.

**Answer:** Done. A `Level` is a `Floor` plus `placements: [(kind, position, facing)]` (`assets/levels/start.ron`; the facing is in degrees, through `Angle`'s serde). `PlacementApp::register_placement` maps a kind to a spawn function: Croc registers `"player"`, so `world.rs` no longer imports Croc, and an unknown kind is an error. The floor is an entity. Collision asks `Floors::height_under(position)`, which `collide` and `find_floor` share. The `flat_floor` scenario launcher turns its parameters into a one-placement `Level`. `tests/dev_mcp.rs` reads Croc's start from the level file, and the new `tests/level.rs` checks headless that the start level places him.

## `Facing` re-implements the angle type psx-bevy already has

`Facing(u16)` masks with `& 0xFFF` by hand in `new` and `turned`, and
`scene::to_rotation` converts it with its own `TAU / 4096.0`; 4096 to the
turn is restated in comments in `croc/mod.rs`, `running.rs`, `world.rs`
and `scene.rs`. `psx_bevy::num` already has `Angle` (wrapping add,
`normalized`, `to_radians_f32`, serde).

I would make `Facing` wrap `Angle<Exact, u16, 12>` (or alias it) and let
the level file and the scene use its conversions. The same holds for the
renderer's unit conversion: `METRES_PER_UNIT` and `to_metres` would sit
with the world's unit definition rather than in the scene, so another view
(a minimap, the camera rules) does not import the scene module for it.

**Answer:** Done. `Facing` wraps `Heading = Angle<Exact, u16, 12>`. `turned` adds and normalizes, and `to_rotation` uses `to_radians_f32`. `METRES_PER_UNIT` and `to_metres` moved to `world.rs`, with the world units. libgte's `rsin`/`rcos` still take the raw units (`Facing::units`). The angle can't be reflected, so `Facing`'s field is `#[reflect(ignore)]`.

## The scene polls and hardcodes what it dresses

`dress_floor` runs every frame and checks a `FloorDressed` marker to
spawn once; the floor's size and offset (400, 48) and the camera's offset
(4 behind, 2 up) are literals; `follow_camera` is a stand-in camera inside
the scene's chain. These are small, but they are where the next level and
the original's camera will land.

I would dress the floor when the level is built (an observer on the level
or floor being added, sized from the level's extent), and give the camera
its own module and component (`FollowCamera { distance, height }`) so the
original's camera replaces it without touching the dressing systems.

**Answer:** Done. The floor and Croc are dressed by observers on `Add<Floor>` and `Add<Croc>`, not by polling. The stand-in camera is `camera.rs`: a `FollowCamera { distance, height, look_height }` that follows a `CameraTarget`, and the original's camera will replace it there. The floor's drawn size and centre are named constants. A flat floor has no extent to size from; real level geometry will bring one.

## The disc-import machinery is generic but lives in the game

`game/src/disc.rs` (414 lines: the `GameData` state, disc search, the
import thread, the retry, and the progress screens) contains nothing
specific to this game; a second game on the same base would copy it
whole. Meanwhile `tools/import` is still the template: `GAME = "the
original game"`, `SERIALS = &[]` accepts any PlayStation disc as this one,
and `import` writes only `disc.ron`, which nothing reads, while the
tuning numbers read from the executable are hand-copied into RON.

I would move the state machine and screens into psx-bevy (a
`DiscImportPlugin` taking the import crate's `open`, `accepted` and
`import` functions and a game name), leaving the game to name its
importer. In `tools/import`, set `GAME` and `SERIALS` to this release
(SLES-00593) so another disc is refused with a message instead of being
"imported"; and either drop the import from start-up until it produces
something the game uses, or have it produce the data the game loads.

**Answer:** Done, in part. `tools/import` now names the game and accepts only the PAL release (`SERIALS = ["SLES_005.93"]`), and `tests/disc.rs` checks that another disc is refused. Kept: `game/src/disc.rs` stays where it is. It is the psx template's file, and the template ships it in `game/`, so moving it into `psx-bevy` belongs upstream in psx (noted in `notes/psx-feedback.md`). The import keeps running at start-up and still writes only `disc.ron`, because the protocol is in place for the animations F-0004 needs. The tuning RON stays hand-written by design: it is the modder-editable layer, and an importer would overwrite edits to it.

## The stomp is stated twice: `ActionKind::Stomp` and the `Stomp` component, kept in step by hand

Answering the action finding, the stomp now lives in two places: `ActionKind::Stomp` in `Action { current, next }`, and a `Stomp` component that `start_stomp` inserts and `stomp` removes. Each change has to touch both (`action.start(ActionKind::Stomp)` plus `insert(Stomp::Begin)`; `action.start(ActionKind::Free)` plus `remove::<Stomp>()`), and between those points they disagree by design. For the rest of the tick a stomp starts in, Croc carries `Stomp` while `current` is `Free`. On the tick it ends, `current` is `Stomp` and the component is gone. `stomp` therefore has to filter on both. The last `Recover` arm writes a value into a component that is being removed, and the `waiting => waiting` arm hides which steps wait. The landing is also tested two ways. `jump::land` reacts to `Landed`, as the answer to the `Vertical` finding says, but the stomp's `Drop` step still polls `!v.rising_or_falling()`.

I would give the step to the action itself, `ActionKind::Stomp(Stomp)`, so that `next = Stomp(Begin)` is the one write that starts a stomp. `latch` carries it into `current`, and `stomp` matches `action.current` and writes back the next `ActionKind`: `Free` when the recovery ends, with nothing to remove. The rule "an action starts on the next tick" still lives only in `Action` and `latch`, and there is one source of truth. `croc_action`'s mapping in the observe closure stays the same with `Stomp(_)`. If the separate component is kept so that other systems can filter `With<Stomp>`, let the latch insert and remove it from `current`, so only one system does it. Either way, make the drop's landing arm test `Has<Landed>`, the same signal `jump::land` uses.

**Answer:** Done. The step is part of the action, `ActionKind::Stomp(Stomp)`, and `Stomp` is no longer a component. The stomp test starts `Stomp(Begin)`. Each tick the body works on `action.current` and starts the step it moves to as the next action, so `latch` carries it and there is nothing to insert or remove. The drop's landing tests `Has<Landed>`, like `jump::land`. `Action::is(kind)` became `is_free()`, which is all the free action's control asks. The recordings pass unchanged.

## Two names for the player's placement kind, and the world still spawns the player

`world.rs` defines `pub const PLAYER: &str = "player"` for its `flat_floor` scenario launcher, and `croc/mod.rs` defines `pub const PLACEMENT: &str = "player"`, which is what `register_placement` actually uses. Nothing ties the two together, so renaming one breaks the scenarios at run time ("nothing registered") and not at compile time. The world module also still decides that a `flat_floor` scenario places "the player". That is the same coupling the placement registry was added to remove, now through a string. The tests disagree too: `tests/dev_mcp.rs` reads Croc's start from `start.ron` through `croc::PLACEMENT`, while `tests/level.rs` hardcodes `12288, -225, 12288` and `94`.

I would keep one name, owned by whoever registers it, `croc::PLACEMENT`, and either move the `flat_floor` launcher next to it in `croc/` (it builds a level whose only placement is Croc's), or give the launcher's parameters a `kind` so the world module stays generic. `tests/level.rs` should read the expected start from the level file the way `dev_mcp.rs` does, or simply assert that the `"player"` placement became a `Croc` at that placement's position and facing.

**Answer:** Done. There is one constant, `croc::PLACEMENT`, and `world::PLAYER` is gone. The `flat_floor` scenario launcher moved into `croc/mod.rs`, which builds a `Level` with Croc's placement, so `world.rs` knows of no placement kind. `tests/level.rs` reads the expected position, facing and floor from `levels/start.ron`, as `tests/dev_mcp.rs` does.

## Advancing a step is now a request for the next action: keep the step in `current` and make `next` a pending change

The changes do what the answer says: `ActionKind::Stomp(Stomp)` carries the step, nothing is inserted or removed, and the drop tests `Has<Landed>`. But to carry the step, `stomp` now calls `action.start(ActionKind::Stomp(next))` on every tick of a stomp, including the ticks where it only waits. "Start an action from the next tick" and "my step moved on" have become the same write, and `next` is last-writer-wins by schedule order. Any system earlier in the tick that starts another action (a hit or a death reaction in `Reactions`, the next ability that interrupts a stomp) has its request silently overwritten by `stomp` in `Actions`. Whichever runs last wins, and nothing in the types says which should. The match also leaves the loop with `continue` from inside one arm so that it can write `Free` instead of a step, and `waiting => waiting` still hides which steps wait (`Hang` while the clip plays, `Drop` until landing, `Settle` while the clip plays).

I would let an action's body update its own progress in place, in `current` (`ActionKind::Stomp(step)` borrowed mutably), and keep `next` for changes only: `next: Option<ActionKind>`, set by `start` and taken by `latch` (`if let Some(k) = next.take() { current = k }`). Then a stomp's step lives in one place, a change of action is a distinct, rare event, and a conflict between two requests in one tick is something `start` can see and resolve (or assert on) rather than lose. `stomp` would write `*step = …` in each arm and call `action.start(ActionKind::Free)` in the recovery's last arm, with no `continue`. Write the three waits as their own arms (`Stomp::Hang | Stomp::Drop | Stomp::Settle => {}`, or one arm each) so that adding a step makes the compiler ask what it does. The observe closure's `croc_action` becomes `action.next.unwrap_or(action.current)`.

**Answer:** Done. The step is updated in place in `action.current`, and `next` is an `Option<ActionKind>` set only when the action changes: the stomp test, or the end of recovery. `latch` takes it, and the observation reads `Action::upcoming()`. Another system's request for a change is no longer overwritten. The match lists the waiting steps explicitly (`Hang | Drop | Settle => {}`) and no longer uses `continue` inside an arm.

## The tests parse the start level by hand: use `config::load_ron`

`tests/level.rs` and `tests/dev_mcp.rs` each build the path `CARGO_MANIFEST_DIR/assets/<Level::START>`, read it with `std::fs` and parse it with `ron::from_str`, the same five lines twice. `game::config::load_ron` already does this, and it is what `world::start` uses to load the level the tests compare against. The copies can drift from the game's loading: they ignore `BEVY_ASSET_ROOT` and layered asset folders, so with a mod layered on top the test would check the base file while the game loaded the mod's.

I would call `game::config::load_ron::<Level>(Level::START)` in both tests. If a test needs to pin the crate's own folder, set `BEVY_ASSET_ROOT` once in a shared test helper. A small `Level::start()` constructor beside `Level::START` would serve `world::start` and both tests.

**Answer:** Done. `Level::start()` reads the start level through `config::load_ron`, which also honours layered asset folders, and is shared by the game's `start` system, `tests/level.rs` and `tests/dev_mcp.rs`.
