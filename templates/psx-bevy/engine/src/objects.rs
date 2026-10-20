//! Object types: the registry that turns placements (data) into entities run
//! by per-type controllers (code).
//!
//! Most PS1 games keep a type table: a type id per placed object, and a
//! control function per type. The engine owns the table's shape; the game
//! registers each type's spawn hook and adds its controller systems (in an
//! [`crate::schedule::EngineSet`]), filtered by the marker component its
//! spawn hook inserts. Adding a type means registering it, never editing a
//! central `match`.
//!
//! Each entry also carries metadata (a name and an optional reflected tuning
//! resource) so tools such as an editor can list and inspect types without
//! knowing the game.
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An object type id, as the level data stores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Reflect)]
pub struct ObjectTypeId(pub u32);

/// The type an entity was spawned as.
#[derive(Component, Clone, Copy, Debug, Reflect)]
#[reflect(Component)]
pub struct ObjectType(pub ObjectTypeId);

/// One placed object in level data. `params` are per-placement values whose
/// meaning belongs to the type (the original's spare placement fields).
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize, Reflect)]
#[reflect(Component)]
pub struct Placement {
    pub type_id: ObjectTypeId,
    pub position: Vec3,
    /// Rotation about +Y, radians.
    #[serde(default)]
    pub yaw: f32,
    #[serde(default)]
    pub params: Vec<i32>,
}

/// A registered type.
#[derive(Clone)]
pub struct ObjectTypeInfo {
    pub name: &'static str,
    /// Adds the type's components to a freshly spawned entity, which already
    /// has [`ObjectType`], the [`Placement`] and its `Transform`.
    pub spawn: fn(&mut EntityWorldMut, &Placement),
    /// Type path of the reflected tuning resource the controller reads, if any.
    pub tuning: Option<&'static str>,
}

impl ObjectTypeInfo {
    pub fn new(name: &'static str, spawn: fn(&mut EntityWorldMut, &Placement)) -> Self {
        Self { name, spawn, tuning: None }
    }

    /// Records `T` as this type's tuning resource.
    pub fn with_tuning<T: Resource + TypePath>(mut self) -> Self {
        self.tuning = Some(T::type_path());
        self
    }
}

/// Every registered type, by id.
#[derive(Resource, Default)]
pub struct ObjectTypes(pub BTreeMap<ObjectTypeId, ObjectTypeInfo>);

/// Placements whose type id nothing registered, with how many were seen.
/// Reported, never silently dropped or mapped to another type.
#[derive(Resource, Default, Debug, Reflect)]
#[reflect(Resource)]
pub struct UnknownObjectTypes(pub BTreeMap<u32, u32>);

pub trait ObjectTypeAppExt {
    /// Registers a type. Panics if `id` is already registered.
    fn register_object_type(&mut self, id: ObjectTypeId, info: ObjectTypeInfo) -> &mut Self;
    /// Replaces a registered type's entry, for a mod that swaps an existing
    /// type's controller. Panics if `id` is not registered yet (add the base
    /// game's plugins first).
    fn replace_object_type(&mut self, id: ObjectTypeId, info: ObjectTypeInfo) -> &mut Self;
}

impl ObjectTypeAppExt for App {
    fn register_object_type(&mut self, id: ObjectTypeId, info: ObjectTypeInfo) -> &mut Self {
        let mut types = self.world_mut().get_resource_or_init::<ObjectTypes>();
        if let Some(old) = types.0.insert(id, info) {
            panic!("object type {} registered twice (first as {:?})", id.0, old.name);
        }
        self
    }

    fn replace_object_type(&mut self, id: ObjectTypeId, info: ObjectTypeInfo) -> &mut Self {
        let mut types = self.world_mut().get_resource_or_init::<ObjectTypes>();
        match types.0.get_mut(&id) {
            Some(entry) => *entry = info,
            None => panic!("object type {} replaced before it was registered (as {:?})", id.0, info.name),
        }
        self
    }
}

/// Spawns one placement through the registry. Returns `None` (and records
/// the id in [`UnknownObjectTypes`]) when its type is not registered.
pub fn spawn_placement(world: &mut World, placement: &Placement) -> Option<Entity> {
    let Some(info) = world.get_resource::<ObjectTypes>().and_then(|t| t.0.get(&placement.type_id)).cloned() else {
        warn!("no object type {} registered; placement at {} skipped", placement.type_id.0, placement.position);
        *world.get_resource_or_init::<UnknownObjectTypes>().0.entry(placement.type_id.0).or_default() += 1;
        return None;
    };
    let mut entity = world.spawn((
        ObjectType(placement.type_id),
        placement.clone(),
        Name::new(info.name),
        Transform::from_translation(placement.position).with_rotation(Quat::from_rotation_y(placement.yaw)),
    ));
    (info.spawn)(&mut entity, placement);
    Some(entity.id())
}

pub struct ObjectsPlugin;

impl Plugin for ObjectsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ObjectTypes>().init_resource::<UnknownObjectTypes>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Component)]
    struct Crate;

    #[test]
    fn spawns_registered_types_and_reports_unknown_ones() {
        let mut app = App::new();
        app.add_plugins(ObjectsPlugin).register_object_type(
            ObjectTypeId(7),
            ObjectTypeInfo::new("crate", |e, _| {
                e.insert(Crate);
            }),
        );
        let at = |id, x| Placement { type_id: ObjectTypeId(id), position: Vec3::X * x, yaw: 0.0, params: vec![] };
        let world = app.world_mut();
        let spawned = spawn_placement(world, &at(7, 2.0)).unwrap();
        assert!(world.get::<Crate>(spawned).is_some());
        assert_eq!(world.get::<Transform>(spawned).unwrap().translation, Vec3::X * 2.0);
        assert!(spawn_placement(world, &at(9, 0.0)).is_none());
        assert!(spawn_placement(world, &at(9, 1.0)).is_none());
        assert_eq!(world.resource::<UnknownObjectTypes>().0.get(&9), Some(&2));
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn duplicate_ids_panic() {
        let info = ObjectTypeInfo::new("a", |_, _| {});
        App::new().register_object_type(ObjectTypeId(1), info.clone()).register_object_type(ObjectTypeId(1), info);
    }

    #[test]
    fn replacing_swaps_a_types_spawn_hook() {
        let mut app = App::new();
        app.add_plugins(ObjectsPlugin)
            .register_object_type(ObjectTypeId(7), ObjectTypeInfo::new("crate", |_, _| {}))
            .replace_object_type(
                ObjectTypeId(7),
                ObjectTypeInfo::new("modded crate", |e, _| {
                    e.insert(Crate);
                }),
            );
        let placement = Placement { type_id: ObjectTypeId(7), position: Vec3::ZERO, yaw: 0.0, params: vec![] };
        let spawned = spawn_placement(app.world_mut(), &placement).unwrap();
        assert!(app.world().get::<Crate>(spawned).is_some());
        assert_eq!(app.world().get::<Name>(spawned).unwrap().as_str(), "modded crate");
    }

    #[test]
    #[should_panic(expected = "before it was registered")]
    fn replacing_an_unregistered_type_panics() {
        App::new().replace_object_type(ObjectTypeId(1), ObjectTypeInfo::new("a", |_, _| {}));
    }
}
