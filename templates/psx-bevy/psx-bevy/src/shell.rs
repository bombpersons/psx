//! The game loop: fixed-rate logic ticks, controller sampling and the tick clock.
//!
//! Game rules run in `FixedUpdate`, in [`GameSystems::Game`], once per logic
//! tick. The tick rate ([`TickRate`]) belongs to the game, not to the display:
//! many PS1 games run their logic every second field (25 or 30 Hz) while the
//! screen refreshes at 50 or 60 Hz.
use crate::controller::{self, Button, ControllerBindings, ControllerState};
use crate::pointer::{self, Pointer};
use crate::psx::Region;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use std::time::Duration;

/// How often game logic runs, independent of rendering.
///
/// Pick it from evidence (the original's main loop: how many vblanks one game
/// frame waits for), because per-tick constants only mean the same thing at
/// the same rate. Changing the resource at runtime retunes `Time<Fixed>`.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Reflect)]
#[reflect(Resource)]
pub struct TickRate {
    pub hz: f64,
}

impl TickRate {
    /// One logic tick per video field: 60 Hz NTSC, 50 Hz PAL.
    pub fn every_field(region: Region) -> Self {
        Self::every_nth_field(region, 1)
    }

    /// One logic tick per `fields` video fields (vblanks). Croc (PAL) waits
    /// for two vblanks per game frame: `every_nth_field(Region::Pal, 2)` = 25 Hz.
    pub fn every_nth_field(region: Region, fields: u32) -> Self {
        Self { hz: region.hz() / f64::from(fields.max(1)) }
    }

    /// The length of one tick.
    pub fn period(self) -> Duration {
        Duration::from_secs_f64(1.0 / self.hz)
    }
}

impl Default for TickRate {
    fn default() -> Self {
        Self::every_field(Region::default())
    }
}

/// Counts logic ticks. `ticking` is false while the developer MCP holds the
/// simulation paused.
#[derive(Resource, Default, Debug, Reflect)]
#[reflect(Resource)]
pub struct GameClock {
    /// Ticks run so far.
    pub frame: u64,
    pub ticking: bool,
}

/// Order of work inside one tick in `FixedUpdate`. Put gameplay in
/// `GameSystems::Game`; developer stepping and tests count ticks there.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum GameSystems {
    /// Samples the controller into [`ControllerState`].
    Input,
    /// Game rules. Skipped while paused.
    Game,
    /// Advances [`GameClock`]; read final per-tick state here (HUDs).
    Finish,
}

/// Modal interfaces capture raw input before the shell samples gameplay devices.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum PhysicalInputSystems {
    /// Updates [`Pointer`], so menus in `Capture` see this frame's mouse.
    Pointer,
    Capture,
    Sample,
}

/// Shared focus boundary for controls/pause menus. Closing a modal waits for
/// physical input to become neutral, preventing its confirm key becoming a jump.
#[derive(Resource, Default, Debug)]
pub struct InputFocus {
    pub captured: bool,
    pub wait_for_release: bool,
}
impl InputFocus {
    pub fn blocked(&self) -> bool {
        self.captured || self.wait_for_release
    }
}

/// Buttons held on by tools rather than devices: the developer MCP and the
/// [`crate::testing`] helpers write here. OR-ed with physical input.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct InjectedInput {
    pub buttons: u16,
    pub mouse: InjectedMouse,
}

impl InjectedInput {
    pub fn set(&mut self, buttons: &[Button]) {
        self.buttons = Button::mask_all(buttons);
    }
    /// Releases every injected button and the mouse.
    pub fn release(&mut self) {
        *self = Self::default();
    }
}

/// Mouse input held on by tools, merged into [`ControllerState::mouse`] each
/// tick: `buttons` add to the physical ones (and press the actions they are
/// bound to), `position` replaces the cursor when set, and `motion` and
/// `wheel` are added on every tick they are held.
#[derive(Default, Debug, Clone, Copy, PartialEq)]
pub struct InjectedMouse {
    /// [`pointer::mouse_bits`] of the held buttons.
    pub buttons: u8,
    /// Screen pixels, origin top-left, y down.
    pub position: Option<Vec2>,
    pub motion: Vec2,
    pub wheel: f32,
}

impl InjectedMouse {
    /// The JSON form replays and the developer MCP share:
    /// `{"buttons": ["left"], "position": [x, y], "motion": [dx, dy], "wheel": n}`,
    /// every field optional.
    pub fn from_json(value: &serde_json::Value) -> Result<Self, String> {
        let object = value.as_object().ok_or("mouse is an object")?;
        if let Some(key) = object.keys().find(|k| !["buttons", "position", "motion", "wheel"].contains(&k.as_str())) {
            return Err(format!("mouse: unknown field {key:?}"));
        }
        let pair = |name: &str| -> Result<Option<Vec2>, String> {
            let Some(value) = object.get(name) else { return Ok(None) };
            let xy = value.as_array().filter(|a| a.len() == 2).ok_or(format!("mouse {name} is [x, y]"))?;
            let [x, y] = [0, 1].map(|i| xy[i].as_f64().filter(|v| v.is_finite()));
            match (x, y) {
                (Some(x), Some(y)) => Ok(Some(Vec2::new(x as f32, y as f32))),
                _ => Err(format!("mouse {name} needs two finite numbers")),
            }
        };
        let mut buttons = 0;
        for name in object
            .get("buttons")
            .map(|v| v.as_array().ok_or("mouse buttons is an array"))
            .transpose()?
            .into_iter()
            .flatten()
        {
            let name = name.as_str().ok_or("mouse button names are strings")?;
            let button = pointer::mouse_button_named(name).ok_or(format!("no mouse button {name:?}"))?;
            buttons |= pointer::mouse_bit(button);
        }
        let wheel = match object.get("wheel") {
            None => 0.0,
            Some(v) => v.as_f64().filter(|w| w.is_finite()).ok_or("mouse wheel is a finite number")? as f32,
        };
        Ok(Self { buttons, position: pair("position")?, motion: pair("motion")?.unwrap_or_default(), wheel })
    }
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "buttons": pointer::mouse_button_names(self.buttons),
            "position": self.position.map(|p| [p.x, p.y]),
            "motion": [self.motion.x, self.motion.y],
            "wheel": self.wheel,
        })
    }
}

/// Physical input, sampled every rendered frame. `tapped` remembers every
/// button seen held since the last tick, so a tap shorter than one tick (easy
/// at 25 Hz) still reaches the game as one tick of press.
#[derive(Resource, Default, Debug)]
struct DeviceInput {
    held: u16,
    tapped: u16,
    left_stick: Vec2,
    right_stick: Vec2,
    /// Mouse buttons, latched like `tapped`; motion and wheel summed.
    mouse_tapped: u8,
    mouse_position: Option<Vec2>,
    mouse_motion: Vec2,
    wheel: f32,
}

/// Fixed-rate logic, controller input and the tick clock.
#[derive(Default)]
pub struct ShellPlugin {
    pub tick: TickRate,
}

impl Plugin for ShellPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(self.tick)
            .insert_resource(Time::<Fixed>::from_hz(self.tick.hz))
            .init_resource::<GameClock>()
            .init_resource::<ControllerBindings>()
            .init_resource::<ControllerState>()
            .init_resource::<InjectedInput>()
            .init_resource::<DeviceInput>()
            .init_resource::<InputFocus>()
            .init_resource::<Pointer>()
            .configure_sets(
                PreUpdate,
                (PhysicalInputSystems::Pointer, PhysicalInputSystems::Capture, PhysicalInputSystems::Sample)
                    .chain()
                    .after(bevy::input::InputSystems),
            )
            .add_systems(PreUpdate, pointer::track_pointer.in_set(PhysicalInputSystems::Pointer))
            .configure_sets(FixedUpdate, (GameSystems::Input, GameSystems::Game, GameSystems::Finish).chain())
            .configure_sets(FixedUpdate, GameSystems::Game.run_if(|clock: Res<GameClock>| clock.ticking))
            .add_systems(
                PreUpdate,
                sample_devices.in_set(PhysicalInputSystems::Sample).run_if(resource_exists::<ButtonInput<KeyCode>>),
            )
            .add_systems(Update, apply_tick_rate.run_if(resource_changed::<TickRate>))
            .add_systems(FixedUpdate, begin_tick.in_set(GameSystems::Input))
            .add_systems(FixedUpdate, finish_tick.in_set(GameSystems::Finish));
    }
}

fn apply_tick_rate(rate: Res<TickRate>, mut time: ResMut<Time<Fixed>>) {
    time.set_timestep_hz(rate.hz);
}

#[allow(clippy::too_many_arguments)]
fn sample_devices(
    keys: Res<ButtonInput<KeyCode>>,
    pads: Query<(Entity, &Gamepad)>,
    bindings: Res<ControllerBindings>,
    pointer: Res<Pointer>,
    motion: Option<Res<AccumulatedMouseMotion>>,
    mut input: ResMut<DeviceInput>,
    mut focus: ResMut<InputFocus>,
    mut state: ResMut<ControllerState>,
    mut injected: ResMut<InjectedInput>,
) {
    let pad = pads.iter().min_by_key(|(entity, _)| entity.to_bits()).map(|(_, pad)| pad);
    if focus.blocked() {
        *input = DeviceInput::default();
        state.reset();
        injected.release();
        // A click that closed a menu must not reach the game either.
        let neutral = keys.get_pressed().next().is_none()
            && !pointer.any_held()
            && pad.is_none_or(|p| {
                p.get_pressed().next().is_none()
                    && p.left_stick().length() < bindings.stick_deadzone().max(0.01)
                    && p.right_stick().length() < bindings.stick_deadzone().max(0.01)
            });
        if !focus.captured && neutral {
            focus.wait_for_release = false;
        }
        return;
    }
    let (held, left, right) = controller::sample(&keys, pad, pointer.held_bits(), &bindings);
    input.held = held;
    input.tapped |= held;
    input.left_stick = left;
    input.right_stick = right;
    input.mouse_tapped |= pointer.held_bits();
    input.mouse_position = pointer.position;
    input.mouse_motion += motion.map_or(Vec2::ZERO, |m| m.delta);
    input.wheel += pointer.wheel;
}

fn begin_tick(
    mut device: ResMut<DeviceInput>,
    #[cfg(not(feature = "dev-mcp"))] injected: Res<InjectedInput>,
    #[cfg(feature = "dev-mcp")] mut injected: ResMut<InjectedInput>,
    mut state: ResMut<ControllerState>,
    mut clock: ResMut<GameClock>,
    focus: Res<InputFocus>,
    bindings: Res<ControllerBindings>,
    #[cfg(feature = "dev-mcp")] control: Option<ResMut<crate::dev_mcp::Control>>,
) {
    clock.ticking = !focus.blocked();
    #[cfg(feature = "dev-mcp")]
    if let Some(mut control) = control {
        if focus.blocked() {
            control.interrupt_for_input_focus();
            injected.release();
        }
        if control.discard_cancelled_step() {
            injected.release();
        }
        clock.ticking &= !control.paused || control.has_step();
    }
    if clock.ticking {
        let mouse = injected.mouse;
        let buttons =
            device.held | device.tapped | injected.buttons | controller::mouse_actions(mouse.buttons, &bindings);
        state.update(buttons, device.left_stick, device.right_stick);
        state.mouse.update(
            device.mouse_tapped | mouse.buttons,
            mouse.position.or(device.mouse_position),
            device.mouse_motion + mouse.motion,
            device.wheel + mouse.wheel,
        );
    }
    device.tapped = 0;
    device.mouse_tapped = 0;
    device.mouse_motion = Vec2::ZERO;
    device.wheel = 0.0;
}

fn finish_tick(
    mut clock: ResMut<GameClock>,
    #[cfg(feature = "dev-mcp")] control: Option<ResMut<crate::dev_mcp::Control>>,
    #[cfg(feature = "dev-mcp")] mut injected: ResMut<InjectedInput>,
) {
    if !clock.ticking {
        return;
    }
    clock.frame += 1;
    #[cfg(feature = "dev-mcp")]
    if let Some(mut control) = control
        && control.finish_tick(clock.frame)
    {
        injected.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::time::TimeUpdateStrategy;

    /// Logic at 25 Hz, rendering four times per tick: a key tapped between
    /// two ticks must still reach the next tick as a press.
    #[test]
    fn taps_between_ticks_are_not_lost() {
        let tick = TickRate::every_nth_field(Region::Pal, 2);
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, ShellPlugin { tick }))
            .insert_resource(TimeUpdateStrategy::ManualDuration(tick.period() / 4))
            .init_resource::<ButtonInput<KeyCode>>();
        let frame = |app: &App| app.world().resource::<GameClock>().frame;
        while frame(&app) == 0 {
            app.update();
        }
        let start = frame(&app);
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::KeyX);
        app.update();
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().release(KeyCode::KeyX);
        while frame(&app) == start {
            app.update();
        }
        assert!(app.world().resource::<ControllerState>().pressed(Button::Cross));
        app.update();
        while frame(&app) == start + 1 {
            app.update();
        }
        assert!(app.world().resource::<ControllerState>().released(Button::Cross));
    }

    /// The mouse follows the pad's rules: a click between ticks reaches the
    /// next tick, and motion adds up over the tick's frames.
    #[test]
    fn mouse_clicks_and_motion_reach_the_next_tick() {
        let tick = TickRate::every_nth_field(Region::Pal, 2);
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, ShellPlugin { tick }))
            .insert_resource(TimeUpdateStrategy::ManualDuration(tick.period() / 4))
            .insert_resource(crate::psx::PsxScreen::default())
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<AccumulatedMouseMotion>();
        let mut window = Window { resolution: (960, 720).into(), ..default() };
        window.set_cursor_position(Some(Vec2::new(480.0, 30.0)));
        app.world_mut().spawn((window, bevy::window::PrimaryWindow));
        let frame = |app: &App| app.world().resource::<GameClock>().frame;
        while frame(&app) == 0 {
            app.update();
        }
        let start = frame(&app);
        app.world_mut().resource_mut::<ButtonInput<MouseButton>>().press(MouseButton::Left);
        app.world_mut().resource_mut::<AccumulatedMouseMotion>().delta = Vec2::new(2.0, 1.0);
        app.update();
        app.world_mut().resource_mut::<ButtonInput<MouseButton>>().release(MouseButton::Left);
        app.update();
        app.world_mut().resource_mut::<AccumulatedMouseMotion>().delta = Vec2::ZERO;
        while frame(&app) == start {
            app.update();
        }
        let mouse = app.world().resource::<ControllerState>().mouse;
        assert!(mouse.pressed(MouseButton::Left), "{mouse:?}");
        assert_eq!(mouse.position, Some(Vec2::new(160.0, 10.0)), "window pixels / 3");
        assert!(mouse.motion.x >= 4.0 && mouse.motion.y >= 2.0, "summed over frames: {mouse:?}");
        while frame(&app) == start + 1 {
            app.update();
        }
        let mouse = app.world().resource::<ControllerState>().mouse;
        assert!(mouse.released(MouseButton::Left) && mouse.motion == Vec2::ZERO, "{mouse:?}");
    }
}
