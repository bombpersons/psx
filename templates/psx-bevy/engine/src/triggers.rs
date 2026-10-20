//! Triggers and actions: level data says *when* (a volume, an action id and
//! its arguments); the game registers *what* (one handler per action id).
//!
//! Handlers are one-shot systems taking the [`ActionCall`]. Unknown action
//! ids are logged and counted in [`UnknownActions`], never ignored or mapped
//! to something else.
//!
//! [`TriggerVolume`] is a placeholder detector: an axis-aligned box that fires
//! when a [`TriggerActivator`] enters it. Replace it with whatever the
//! original's trigger model is (floor-data triggers, zones, scripted events);
//! keep the action registry.
use crate::schedule::EngineSet;
use bevy::ecs::system::SystemId;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An action id, as level data stores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Reflect)]
pub struct ActionId(pub u32);

/// One request to run an action.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Reflect)]
pub struct ActionCall {
    pub action: ActionId,
    /// Arguments whose meaning belongs to the action.
    #[serde(default)]
    pub args: Vec<i32>,
    /// The entity that caused it (the trigger volume), if any.
    #[serde(skip)]
    pub source: Option<Entity>,
}

/// Write this to run an action during [`EngineSet::Triggers`].
#[derive(Message, Clone, Debug)]
pub struct FireAction(pub ActionCall);

/// A registered action handler.
#[derive(Clone)]
pub struct ActionInfo {
    pub name: &'static str,
    pub handler: SystemId<In<ActionCall>>,
}

/// Every registered action, by id.
#[derive(Resource, Default)]
pub struct Actions(pub BTreeMap<ActionId, ActionInfo>);

/// Calls whose action id nothing registered, with how many were seen.
#[derive(Resource, Default, Debug, Reflect)]
#[reflect(Resource)]
pub struct UnknownActions(pub BTreeMap<u32, u32>);

pub trait ActionAppExt {
    /// Registers `handler` for `id`. Panics if `id` is already registered.
    fn register_action<M>(
        &mut self,
        id: ActionId,
        name: &'static str,
        handler: impl IntoSystem<In<ActionCall>, (), M> + 'static,
    ) -> &mut Self;
    /// Replaces a registered action's handler, for a mod that changes what
    /// an existing action does. Panics if `id` is not registered yet (add the
    /// base game's plugins first).
    fn replace_action<M>(
        &mut self,
        id: ActionId,
        name: &'static str,
        handler: impl IntoSystem<In<ActionCall>, (), M> + 'static,
    ) -> &mut Self;
}

impl ActionAppExt for App {
    fn register_action<M>(
        &mut self,
        id: ActionId,
        name: &'static str,
        handler: impl IntoSystem<In<ActionCall>, (), M> + 'static,
    ) -> &mut Self {
        let handler = self.world_mut().register_system(handler);
        let mut actions = self.world_mut().get_resource_or_init::<Actions>();
        if let Some(old) = actions.0.insert(id, ActionInfo { name, handler }) {
            panic!("action {} registered twice (first as {:?})", id.0, old.name);
        }
        self
    }

    fn replace_action<M>(
        &mut self,
        id: ActionId,
        name: &'static str,
        handler: impl IntoSystem<In<ActionCall>, (), M> + 'static,
    ) -> &mut Self {
        if !self.world().get_resource::<Actions>().is_some_and(|a| a.0.contains_key(&id)) {
            panic!("action {} replaced before it was registered (as {name:?})", id.0);
        }
        let handler = self.world_mut().register_system(handler);
        let old = self.world_mut().resource_mut::<Actions>().0.insert(id, ActionInfo { name, handler });
        if let Some(old) = old {
            let _ = self.world_mut().unregister_system(old.handler);
        }
        self
    }
}

/// Fires its call when a [`TriggerActivator`] enters the box.
#[derive(Component, Clone, Debug, Serialize, Deserialize, Reflect)]
#[reflect(Component)]
pub struct TriggerVolume {
    pub half_extents: Vec3,
    pub call: ActionCall,
    /// Fire only the first time.
    #[serde(default)]
    pub once: bool,
    /// Runtime state: an activator was inside last tick / it has fired.
    #[serde(skip)]
    pub inside: bool,
    #[serde(skip)]
    pub fired: bool,
}

/// Entities that set off trigger volumes (usually the player).
#[derive(Component, Default, Clone, Copy, Debug, Reflect)]
#[reflect(Component)]
pub struct TriggerActivator;

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TriggerSystems {
    Detect,
    Dispatch,
}

pub struct TriggersPlugin;

impl Plugin for TriggersPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Actions>()
            .init_resource::<UnknownActions>()
            .add_message::<FireAction>()
            .configure_sets(
                FixedUpdate,
                (TriggerSystems::Detect, TriggerSystems::Dispatch).chain().in_set(EngineSet::Triggers),
            )
            .add_systems(FixedUpdate, detect.in_set(TriggerSystems::Detect))
            .add_systems(FixedUpdate, dispatch.in_set(TriggerSystems::Dispatch));
    }
}

/// Uses `Transform`, not `GlobalTransform`: rules run headless in tests,
/// where transforms are not propagated, so volumes and activators are
/// top-level entities in world space.
fn detect(
    mut volumes: Query<(Entity, &Transform, &mut TriggerVolume)>,
    activators: Query<&Transform, With<TriggerActivator>>,
    mut fire: MessageWriter<FireAction>,
) {
    for (entity, at, mut volume) in &mut volumes {
        let centre = at.translation;
        let inside = activators.iter().any(|a| ((a.translation - centre).abs()).cmple(volume.half_extents).all());
        if inside && !volume.inside && !(volume.once && volume.fired) {
            fire.write(FireAction(ActionCall { source: Some(entity), ..volume.call.clone() }));
            volume.fired = true;
        }
        volume.inside = inside;
    }
}

/// Runs each fired action's handler, in the order they were fired.
fn dispatch(world: &mut World) {
    let calls: Vec<ActionCall> = world.resource_mut::<Messages<FireAction>>().drain().map(|m| m.0).collect();
    for call in calls {
        let handler = world.resource::<Actions>().0.get(&call.action).map(|a| a.handler);
        match handler {
            Some(handler) => {
                if let Err(e) = world.run_system_with(handler, call) {
                    error!("action handler failed: {e}");
                }
            }
            None => {
                warn!("no action {} registered; call {:?} ignored", call.action.0, call.args);
                *world.resource_mut::<UnknownActions>().0.entry(call.action.0).or_default() += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_bevy::controller::Button;
    use psx_bevy::prelude::{Region, TickRate};
    use psx_bevy::testing::{TickApp, headless_app};

    #[derive(Resource, Default)]
    struct Log(Vec<Vec<i32>>);

    #[test]
    fn entering_a_volume_runs_its_action_once_per_entry() {
        let mut app = headless_app(TickRate::every_field(Region::Ntsc));
        app.add_plugins(crate::EnginePlugin).init_resource::<Log>().register_action(
            ActionId(3),
            "log",
            |In(call): In<ActionCall>, mut log: ResMut<Log>| log.0.push(call.args),
        );
        let call = |action| ActionCall { action: ActionId(action), args: vec![action as i32], source: None };
        app.world_mut().spawn((
            Transform::default(),
            TriggerVolume { half_extents: Vec3::ONE, call: call(3), once: false, inside: false, fired: false },
        ));
        app.world_mut().spawn((
            Transform::from_xyz(5.0, 0.0, 0.0),
            TriggerVolume { half_extents: Vec3::ONE, call: call(4), once: true, inside: false, fired: false },
        ));
        let player = app.world_mut().spawn((Transform::default(), TriggerActivator)).id();
        app.run_ticks(3, &[] as &[Button]);
        assert_eq!(app.world().resource::<Log>().0, vec![vec![3]]);
        app.world_mut().get_mut::<Transform>(player).unwrap().translation.x = 5.0;
        app.run_ticks(2, &[]);
        assert_eq!(app.world().resource::<UnknownActions>().0.get(&4), Some(&1));
        app.world_mut().get_mut::<Transform>(player).unwrap().translation.x = 0.0;
        app.run_ticks(1, &[]);
        assert_eq!(app.world().resource::<Log>().0, vec![vec![3], vec![3]]);
    }

    #[test]
    fn replacing_an_action_runs_the_new_handler() {
        let mut app = headless_app(TickRate::every_field(Region::Ntsc));
        app.add_plugins(crate::EnginePlugin)
            .init_resource::<Log>()
            .register_action(ActionId(3), "log", |In(call): In<ActionCall>, mut log: ResMut<Log>| log.0.push(call.args))
            .replace_action(ActionId(3), "log twice", |In(call): In<ActionCall>, mut log: ResMut<Log>| {
                log.0.push(call.args.clone());
                log.0.push(call.args);
            });
        app.world_mut().spawn((
            Transform::default(),
            TriggerVolume {
                half_extents: Vec3::ONE,
                call: ActionCall { action: ActionId(3), args: vec![1], source: None },
                once: true,
                inside: false,
                fired: false,
            },
        ));
        app.world_mut().spawn((Transform::default(), TriggerActivator));
        app.run_ticks(2, &[] as &[Button]);
        assert_eq!(app.world().resource::<Log>().0, vec![vec![1], vec![1]]);
        assert_eq!(app.world().resource::<Actions>().0[&ActionId(3)].name, "log twice");
    }
}
