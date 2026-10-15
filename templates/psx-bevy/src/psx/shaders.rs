//! The PS1 look's shaders, added once the Bevy shader libraries they import
//! have loaded.
//!
//! Bevy loads its WESL libraries (`bevy_pbr::render::view_transformations`
//! and so on) asynchronously at startup. A shader compiled before one of its
//! imports arrives logs "Shader ... has an unresolved import" and is retried.
//! That is harmless, but a WARN at startup looks like a real fault, so these
//! shaders are inserted only when every module they import (transitively) is
//! present. After a few seconds they are inserted anyway, so a genuinely
//! missing import still shows Bevy's warning.
use bevy::{
    asset::uuid_handle,
    prelude::*,
    shader::{Shader, ShaderImport},
};
use std::collections::HashSet;

pub const MATERIAL: Handle<Shader> = uuid_handle!("6a1d3f0e-6f1b-4f7e-9d38-6e2b8f4f6a01");
pub const MATERIAL2D: Handle<Shader> = uuid_handle!("6a1d3f0e-6f1b-4f7e-9d38-6e2b8f4f6a02");
pub const SPRITE: Handle<Shader> = uuid_handle!("6a1d3f0e-6f1b-4f7e-9d38-6e2b8f4f6a03");
pub const SCREEN: Handle<Shader> = uuid_handle!("6a1d3f0e-6f1b-4f7e-9d38-6e2b8f4f6a04");

/// Frames to wait for imports before inserting regardless.
const MAX_WAIT_FRAMES: u32 = 600;

/// Shaders not yet inserted into `Assets<Shader>`.
#[derive(Resource)]
struct Pending(Vec<(Handle<Shader>, Shader)>);

pub(super) fn plugin(app: &mut App) {
    let wesl = |source: &'static str, name: &str| {
        Shader::from_wesl(source, format!("embedded://{}/psx/shaders/{name}", env!("CARGO_CRATE_NAME")))
    };
    app.insert_resource(Pending(vec![
        (MATERIAL, wesl(include_str!("shaders/psx_material.wesl"), "psx_material.wesl")),
        (MATERIAL2D, wesl(include_str!("shaders/psx_material2d.wesl"), "psx_material2d.wesl")),
        (SPRITE, wesl(include_str!("shaders/psx_sprite.wesl"), "psx_sprite.wesl")),
        (SCREEN, wesl(include_str!("shaders/psx_screen.wesl"), "psx_screen.wesl")),
    ]))
    .add_systems(PreUpdate, insert_when_imports_loaded.run_if(resource_exists::<Pending>));
}

fn insert_when_imports_loaded(
    mut commands: Commands,
    mut pending: ResMut<Pending>,
    mut shaders: ResMut<Assets<Shader>>,
    mut waited: Local<u32>,
) {
    *waited += 1;
    let modules = loaded_modules(&shaders);
    let give_up = *waited > MAX_WAIT_FRAMES;
    let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut pending.0)
        .into_iter()
        .partition(|(_, shader)| give_up || imports_loaded(shader, &shaders, &modules));
    for (handle, shader) in ready {
        debug!("adding {} after {} frames", shader.path, *waited);
        let _ = shaders.insert(handle.id(), shader);
    }
    pending.0 = waiting;
    if pending.0.is_empty() {
        commands.remove_resource::<Pending>();
    }
}

/// Module paths (`bevy_pbr::render::mesh_functions`) of every loaded shader.
fn loaded_modules(shaders: &Assets<Shader>) -> HashSet<String> {
    shaders
        .iter()
        .filter_map(|(_, shader)| match &shader.import_path {
            ShaderImport::Custom(path) => Some(path.clone()),
            ShaderImport::AssetPath(_) => None,
        })
        .collect()
}

/// True when every module `shader` imports, and everything those import, is loaded.
fn imports_loaded(shader: &Shader, shaders: &Assets<Shader>, modules: &HashSet<String>) -> bool {
    let module_of =
        |path: &str| shaders.iter().find(|(_, s)| matches!(&s.import_path, ShaderImport::Custom(p) if p == path));
    let mut stack: Vec<&Shader> = vec![shader];
    let mut seen = HashSet::new();
    while let Some(shader) = stack.pop() {
        for import in &shader.imports {
            let ShaderImport::Custom(path) = import else { continue };
            // WESL imports are listed both as modules and as items in them
            // (`a::b` and `a::b::item`), and parents may be bare namespaces
            // (`bevy_pbr::render`). Accept a loaded module, an item of one,
            // or a namespace with loaded modules under it.
            let parent = path.rsplit_once("::").map(|(parent, _)| parent);
            let module = if modules.contains(path) {
                path.as_str()
            } else if let Some(parent) = parent.filter(|p| modules.contains(*p)) {
                parent
            } else if modules.iter().any(|m| m.strip_prefix(path.as_str()).is_some_and(|rest| rest.starts_with("::"))) {
                continue;
            } else {
                return false;
            };
            if seen.insert(module.to_string())
                && let Some((_, library)) = module_of(module)
            {
                stack.push(library);
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(path: &str, source: &'static str) -> Shader {
        Shader::from_wesl(source, format!("embedded://{path}.wesl"))
    }

    #[test]
    fn waits_for_transitive_imports() {
        let main = library("game/main", "import lib::a::{f};\nimport lib::b;\nfn g() -> f32 { return f() + b::h(); }");
        let a = library("lib/a", "import lib::c::k;\nfn f() -> f32 { return k(); }");
        let b = library("lib/b", "fn h() -> f32 { return 1.0; }");
        let c = library("lib/c", "fn k() -> f32 { return 2.0; }");
        let mut shaders = Assets::<Shader>::default();
        let check = |shaders: &Assets<Shader>| imports_loaded(&main, shaders, &loaded_modules(shaders));
        assert!(!check(&shaders));
        shaders.add(a);
        shaders.add(b);
        assert!(!check(&shaders), "lib::c, imported by lib::a, is still missing");
        shaders.add(c);
        assert!(check(&shaders));
    }

    #[test]
    fn psx_material_waits_for_bevy_libraries() {
        let material = library("game/psx/shaders/psx_material", include_str!("shaders/psx_material.wesl"));
        let shaders = Assets::<Shader>::default();
        assert!(!imports_loaded(&material, &shaders, &loaded_modules(&shaders)));
    }
}
