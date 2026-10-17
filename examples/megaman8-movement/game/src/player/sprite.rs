//! Mega Man's look. Uses the standing frame from a user's own capture when one
//! is present under game/assets/ (see README), otherwise a flat-colour placeholder.
use super::{Facing, Player};
use crate::stage::user_asset;
use bevy::prelude::*;

/// Texture page (320,0) 4-bit through CLUT (128,480), as written by `psx textures`
/// for the captured frame. The game uploads the current animation frame into
/// u 0..96, v 192..208 each tick, so one capture holds one pose.
const PAGE: &str = "captures/mm8/page_320_0_4bpp_clut_128_480.png";

/// The six 16×16 pieces of the standing pose, from the capture's GP0 quads
/// (gpu_frame.json commands 668–673): screen offset of the piece's top-left
/// from the sprite origin, width, and first texel column. The game draws them
/// mirrored (u decreasing left to right); the PS1 samples a mirrored quad from
/// u_right - 1 down to u_left, so each piece shows texels u0..u0+w flipped.
const PIECES: [(f32, f32, f32, f32); 6] = [
    (4.0, 8.0, 15.0, 1.0),
    (-12.0, 8.0, 16.0, 16.0),
    (-28.0, 8.0, 16.0, 32.0),
    (4.0, -8.0, 16.0, 48.0),
    (-12.0, -8.0, 16.0, 64.0),
    (-4.0, -24.0, 16.0, 80.0),
];

#[derive(Component)]
struct Rig;

pub struct PlayerSpritePlugin;

impl Plugin for PlayerSpritePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (dress_player, face));
    }
}

fn dress_player(mut commands: Commands, players: Query<Entity, Added<Player>>, assets: Res<AssetServer>) {
    for player in &players {
        let rig = commands.spawn((Rig, Transform::default(), Visibility::default(), ChildOf(player))).id();
        if user_asset(PAGE) {
            let page: Handle<Image> = assets.load(PAGE);
            for (dx, dy, w, u0) in PIECES {
                commands.spawn((
                    Sprite {
                        image: page.clone(),
                        rect: Some(Rect::new(u0, 192.0, u0 + w, 208.0)),
                        flip_x: true,
                        ..default()
                    },
                    Transform::from_xyz(dx + w / 2.0, -(dy + 8.0), 0.0),
                    ChildOf(rig),
                ));
            }
        } else {
            // Placeholder: helmet, body and boots in roughly the right place.
            let blue = Color::srgb_u8(0, 112, 236);
            let cyan = Color::srgb_u8(0, 200, 255);
            for (size, offset, color) in [
                (Vec2::new(18.0, 16.0), Vec2::new(2.0, 14.0), blue),
                (Vec2::new(20.0, 16.0), Vec2::new(-2.0, 0.0), cyan),
                (Vec2::new(28.0, 14.0), Vec2::new(-4.0, -16.0), blue),
            ] {
                commands.spawn((
                    Sprite::from_color(color, size),
                    Transform::from_translation(offset.extend(0.0)),
                    ChildOf(rig),
                ));
            }
        }
    }
}

type TurnedPlayer = (With<Player>, Changed<Facing>);

/// Mirror the whole rig for facing left (the captured pose faces right).
/// Mirroring about the origin is an assumption; it was not captured.
fn face(players: Query<(&Facing, &Children), TurnedPlayer>, mut rigs: Query<&mut Transform, With<Rig>>) {
    for (facing, children) in &players {
        for child in children.iter() {
            if let Ok(mut t) = rigs.get_mut(child) {
                t.scale.x = facing.sign();
            }
        }
    }
}
