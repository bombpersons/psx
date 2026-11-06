//! Draws a scene headless in both looks and checks the pixels. Needs a
//! graphics adapter; a software one (Mesa's lavapipe) is enough. Without one
//! the tests say so and pass.
use renderer::*;

fn renderer(display: Display) -> Option<Renderer> {
    match Renderer::headless(Options { game: [320, 240], display, software: true }) {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("skipped: {e}");
            None
        }
    }
}

fn pixel(frame: &Frame, x: u32, y: u32) -> [u8; 3] {
    let at = ((y * frame.width + x) * 4) as usize;
    [frame.rgba[at], frame.rgba[at + 1], frame.rgba[at + 2]]
}

/// A red cube in front of the camera on blue, and a white sprite top left.
fn scene(r: &mut Renderer) -> Scene {
    let cube = r.model(&cube(2.0, [[1.0, 0.0, 0.0]; 6]));
    let mut scene = Scene { clear: [0.0, 0.0, 1.0], ..Default::default() };
    scene.camera.view = glam::camera::rh::view::look_at_mat4(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
    scene.models.push(Instance::new(cube, Mat4::IDENTITY));
    let white = r.white();
    scene.rect(white, [0.0, 0.0, 10.0, 10.0], [1.0; 4]);
    scene
}

#[test]
fn both_looks_draw_the_scene() {
    for look in [Look::Ps1, Look::Modern] {
        let Some(mut r) = renderer(Display { look, ..Default::default() }) else { return };
        let scene = scene(&mut r);
        let frame = r.capture(&scene).unwrap();
        assert_eq!([frame.width, frame.height], [320, 240], "{look:?}");
        assert_eq!(pixel(&frame, 160, 120), [255, 0, 0], "{look:?}: the cube in the middle");
        assert_eq!(pixel(&frame, 5, 120), [0, 0, 255], "{look:?}: the clear colour at the side");
        assert_eq!(pixel(&frame, 5, 5), [255, 255, 255], "{look:?}: the sprite");
    }
}

#[test]
fn the_window_aspect_widens_the_screen_and_the_view() {
    let Some(mut r) = renderer(Display { aspect: Aspect::Window, ..Default::default() }) else { return };
    r.resize(1920, 1080);
    assert_eq!(r.layout_size(), [426, 240]);
    let scene = scene(&mut r);
    let frame = r.capture(&scene).unwrap();
    assert_eq!([frame.width, frame.height], [426, 240]);
    // Same height, so the cube is as tall as at 4:3, centred.
    assert_eq!(pixel(&frame, 213, 120), [255, 0, 0]);
    assert_eq!(pixel(&frame, 420, 120), [0, 0, 255]);
}

#[test]
fn half_blending_averages_on_15_bit_values() {
    let Some(mut r) = renderer(Display::default()) else { return };
    let white = r.white();
    let mut scene = Scene { clear: [0.0, 0.0, 0.0], ..Default::default() };
    scene.rect(white, [0.0, 0.0, 320.0, 240.0], [1.0, 0.0, 0.0, 1.0]);
    scene.sprites.push(Sprite {
        texture: white,
        src: [0.0, 0.0, 1.0, 1.0],
        dst: [0.0, 0.0, 320.0, 240.0],
        color: [0.0, 1.0, 0.0, 1.0],
        blend: Blend::Half,
    });
    let frame = r.capture(&scene).unwrap();
    // (31 + 0) >> 1 = 15 in 5 bits, red and green alike.
    assert_eq!(pixel(&frame, 100, 100).map(|c| c >> 3), [15, 15, 0]);
}
