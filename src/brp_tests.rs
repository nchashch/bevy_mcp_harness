//! Regression tests for the BRP game tools (`super` = `crate::brp`).
//!
//! Two tiers, per the 0.4.0 test-suite decision:
//!
//! - **Tier 1 — pure functions**: hand-maintained name tables (a bevy rename/addition fails
//!   the exhaustive-variant matches at compile time, not in a live session), the crop/downscale
//!   math that had two coordinate-space bugs, the alignment block, the UI filter and the
//!   unchanged-suppression hash.
//! - **Tier 2 — `no_render` test scenes**: the render-less composition (UI layout and picking
//!   are render-free logic) booted for real, then `ui_dump_snapshot`/`entities_on_screen_data`
//!   asserted over constructed trees and component states. The `Interaction`/`Hovered`/
//!   `Pressed` projection is not a state machine in our code — it is a pure projection of
//!   component state — so tests drive it by inserting components directly.
//!
//! Not covered here (deliberately): the GPU capture pipeline (deferred debug-view runner,
//! camera retargeting — live verification on real hardware is the check) and the MCP HTTP
//! layer (the `headless` example is the smoke test).

use bevy::app::TaskPoolPlugin;
use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::{InheritedVisibility, ViewVisibility};
use bevy::diagnostic::FrameCountPlugin;
use bevy::ecs::system::In;
use bevy::input::gamepad::{GamepadAxis, GamepadButton};
use bevy::input::mouse::MouseButton;
use bevy::math::{Mat4, UVec2, Vec3, Vec3A};
use bevy::picking::backend::HitData;
use bevy::picking::hover::HoverMap;
use bevy::prelude::*;
use bevy::time::TimePlugin;
use bevy::ui::Pressed;
use bevy::window::ExitCondition;
use serde_json::json;
use std::sync::atomic::{AtomicU16, Ordering};

// ---------------------------------------------------------------------------
// Tier 1: name tables — exhaustive-variant matches fail to COMPILE when bevy
// gains a variant the parse table doesn't know about (the migration guard).
// ---------------------------------------------------------------------------

/// Every `GamepadButton` variant except `Other` — the exact set the mock must serve. Written as
/// an exhaustive match WITHOUT a wildcard: a new bevy variant is a compile error here, which
/// forces the `parse_gamepad_button` table update instead of leaving a button unreachable.
fn gamepad_button_name(button: &GamepadButton) -> &str {
    match button {
        GamepadButton::South => "South",
        GamepadButton::East => "East",
        GamepadButton::North => "North",
        GamepadButton::West => "West",
        GamepadButton::C => "C",
        GamepadButton::Z => "Z",
        GamepadButton::LeftTrigger => "LeftTrigger",
        GamepadButton::LeftTrigger2 => "LeftTrigger2",
        GamepadButton::RightTrigger => "RightTrigger",
        GamepadButton::RightTrigger2 => "RightTrigger2",
        GamepadButton::Select => "Select",
        GamepadButton::Start => "Start",
        GamepadButton::Mode => "Mode",
        GamepadButton::LeftThumb => "LeftThumb",
        GamepadButton::RightThumb => "RightThumb",
        GamepadButton::DPadUp => "DPadUp",
        GamepadButton::DPadDown => "DPadDown",
        GamepadButton::DPadLeft => "DPadLeft",
        GamepadButton::DPadRight => "DPadRight",
        GamepadButton::Other(_) => unreachable!("Other buttons are not mockable by name"),
    }
}

#[test]
fn gamepad_button_table_covers_every_variant() {
    let all = [
        GamepadButton::South,
        GamepadButton::East,
        GamepadButton::North,
        GamepadButton::West,
        GamepadButton::C,
        GamepadButton::Z,
        GamepadButton::LeftTrigger,
        GamepadButton::LeftTrigger2,
        GamepadButton::RightTrigger,
        GamepadButton::RightTrigger2,
        GamepadButton::Select,
        GamepadButton::Start,
        GamepadButton::Mode,
        GamepadButton::LeftThumb,
        GamepadButton::RightThumb,
        GamepadButton::DPadUp,
        GamepadButton::DPadDown,
        GamepadButton::DPadLeft,
        GamepadButton::DPadRight,
    ];
    assert_eq!(all.len(), 19, "the mock's documented button count");
    for button in all {
        assert_eq!(
            super::parse_gamepad_button(gamepad_button_name(&button))
                .map(|parsed| parsed == button),
            Some(true),
            "{}",
            gamepad_button_name(&button)
        );
    }
    assert_eq!(super::parse_gamepad_button("Nonsense"), None);
    assert_eq!(super::parse_gamepad_button(""), None);
}

#[test]
fn gamepad_axis_table_covers_every_variant() {
    let all = [
        (GamepadAxis::LeftStickX, "LeftStickX"),
        (GamepadAxis::LeftStickY, "LeftStickY"),
        (GamepadAxis::LeftZ, "LeftZ"),
        (GamepadAxis::RightStickX, "RightStickX"),
        (GamepadAxis::RightStickY, "RightStickY"),
        (GamepadAxis::RightZ, "RightZ"),
    ];
    assert_eq!(all.len(), 6, "the mock's documented axis count");
    for (axis, name) in all {
        assert_eq!(
            super::parse_gamepad_axis(name).map(|parsed| parsed == axis),
            Some(true)
        );
    }
    assert_eq!(super::parse_gamepad_axis("Nonsense"), None);
}

#[test]
fn mouse_button_table_matches_documented_names() {
    let all = [
        (MouseButton::Left, "Left"),
        (MouseButton::Right, "Right"),
        (MouseButton::Middle, "Middle"),
        (MouseButton::Back, "Back"),
        (MouseButton::Forward, "Forward"),
    ];
    for (button, name) in all {
        assert_eq!(
            super::parse_mouse_button(name).map(|parsed| parsed == button),
            Some(true)
        );
    }
    // `Other` devices exist in bevy but are unreachable by name — documented.
    assert_eq!(super::parse_mouse_button("Other"), None);
    // The three click-capable buttons drive bevy_picking's pointer buttons; the rest don't.
    assert_eq!(
        super::mouse_button_to_pointer_button(MouseButton::Left),
        Some(bevy::picking::pointer::PointerButton::Primary)
    );
    assert_eq!(
        super::mouse_button_to_pointer_button(MouseButton::Right),
        Some(bevy::picking::pointer::PointerButton::Secondary)
    );
    assert_eq!(
        super::mouse_button_to_pointer_button(MouseButton::Middle),
        Some(bevy::picking::pointer::PointerButton::Middle)
    );
    assert_eq!(
        super::mouse_button_to_pointer_button(MouseButton::Back),
        None
    );
    assert_eq!(
        super::mouse_button_to_pointer_button(MouseButton::Other(9)),
        None
    );
}

// Feature-gated: CI's `cargo build --all-targets` runs WITHOUT --all-features, and the
// render_debug module (plus bevy_dev_tools) doesn't exist there.
#[cfg(feature = "render_debug")]
#[test]
fn render_debug_mode_table_round_trips_every_mode() {
    use super::render_debug;
    use bevy_dev_tools::render_debug::RenderDebugMode;
    // Exhaustive (no wildcard): a new bevy_dev_tools mode is a compile error here.
    let all: Vec<(&str, RenderDebugMode)> = vec![
        ("depth", RenderDebugMode::Depth),
        ("normals", RenderDebugMode::Normal),
        ("motion_vectors", RenderDebugMode::MotionVectors),
        ("deferred", RenderDebugMode::Deferred),
        ("deferred_base_color", RenderDebugMode::DeferredBaseColor),
        ("deferred_emissive", RenderDebugMode::DeferredEmissive),
        (
            "deferred_metallic_roughness",
            RenderDebugMode::DeferredMetallicRoughness,
        ),
        (
            "depth_pyramid",
            RenderDebugMode::DepthPyramid { mip_level: 0 },
        ),
    ];
    for (name, mode) in all {
        assert_eq!(render_debug::parse_mode(name), Ok(mode), "{name}");
    }
    assert!(render_debug::parse_mode("Nonsense").is_err());
    // `"wireframe"`/`"physics"` are handled by their own branches — never overlay modes.
    assert!(render_debug::parse_mode("wireframe").is_err());
    assert!(render_debug::parse_mode("physics").is_err());

    // Prepass requirements per mode (the deferred-runner's warm-up contract).
    assert!(render_debug::required_prepasses(&RenderDebugMode::Depth)
        .contains(&"DepthPrepass"));
    assert!(
        render_debug::required_prepasses(&RenderDebugMode::Normal).contains(&"NormalPrepass")
    );
    assert!(render_debug::required_prepasses(&RenderDebugMode::MotionVectors)
        .contains(&"MotionVectorPrepass"));
    assert!(render_debug::required_prepasses(&RenderDebugMode::Deferred)
        .contains(&"DeferredPrepass"));
    assert!(
        render_debug::required_prepasses(&RenderDebugMode::DepthPyramid { mip_level: 2 })
            .contains(&"DepthPrepass")
    );
}

// ---------------------------------------------------------------------------
// Tier 1: parse_crop — clamped-intersection contract at the parse boundary.
// ---------------------------------------------------------------------------

#[test]
fn parse_crop_accepts_valid_rects_and_rounds() {
    assert_eq!(super::parse_crop(&json!([10, 20, 300, 400])), Some([10, 20, 300, 400]));
    // Floats round to pixels.
    assert_eq!(super::parse_crop(&json!([10.4, 20.6, 0.5, 1.5])), Some([10, 21, 1, 2]));
    assert_eq!(super::parse_crop(&json!([0, 0, 0, 0])), Some([0, 0, 0, 0]));
}

#[test]
fn parse_crop_rejects_malformed_input() {
    assert_eq!(super::parse_crop(&json!([1, 2, 3])), None, "wrong length");
    assert_eq!(super::parse_crop(&json!([1, 2, 3, 4, 5])), None, "too long");
    assert_eq!(super::parse_crop(&json!([-1, 0, 0, 0])), None, "negative");
    assert_eq!(super::parse_crop(&json!([0, 0, 0, "x"])), None, "non-numeric");
    // Non-finite floats are rejected (a NaN crop would poison every later comparison).
    assert_eq!(super::parse_crop(&json!([0, 0, 0, f64::NAN])), None);
    assert_eq!(super::parse_crop(&json!([0, 0, f64::INFINITY, 1])), None);
    assert_eq!(super::parse_crop(&json!("not an array")), None);
    assert_eq!(super::parse_crop(&json!(null)), None);
}

// ---------------------------------------------------------------------------
// Tier 1: encode_served_view — the crop-then-downscale math (the persistent
// record stays full-resolution; this is only what the agent is served).
// ---------------------------------------------------------------------------

/// A 4x2 PNG of distinct per-pixel colors (deterministic content for crop assertions).
pub(crate) fn tiny_png(width: u32, height: u32) -> Vec<u8> {
    let mut img = image::RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let r = (x * 40) as u8;
            let g = (y * 100 + 20) as u8;
            let b = (x * 10 + y * 30 + 5) as u8;
            img.put_pixel(x, y, image::Rgba([r, g, b, 255]));
        }
    }
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn decode_dims(png: &[u8]) -> (u32, u32) {
    let img = image::load_from_memory(png).unwrap();
    (img.width(), img.height())
}

#[test]
fn served_view_without_params_is_identity() {
    let png = tiny_png(4, 2);
    let (bytes, size) = super::encode_served_view(&png, None, None).unwrap();
    assert_eq!(size, [4, 2]);
    assert_eq!(decode_dims(&bytes), (4, 2));
    // Re-encoding is deterministic (same pixels → same bytes → unchanged-suppression works).
    let (bytes2, _) = super::encode_served_view(&png, None, None).unwrap();
    assert_eq!(bytes, bytes2);
}

#[test]
fn served_view_crop_takes_the_sub_rect() {
    let png = tiny_png(4, 2);
    // Crop [2, 0, 2, 2]: right half. The crop is applied before any resize, in pixel space.
    let (bytes, size) = super::encode_served_view(&png, Some([2, 0, 2, 2]), None).unwrap();
    assert_eq!(size, [2, 2]);
    let img = image::load_from_memory(&bytes).unwrap().to_rgb8();
    let source = image::load_from_memory(&png).unwrap().to_rgb8();
    for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        assert_eq!(
            img.get_pixel(x, y),
            source.get_pixel(x + 2, y),
            "crop must preserve pixel identity (crop before resize)"
        );
    }
}

#[test]
fn served_view_crop_clamps_out_of_range_rects() {
    let png = tiny_png(4, 2);
    // A rect hanging off the frame degrades to the intersection, not a panic.
    let (_, size) = super::encode_served_view(&png, Some([2, 1, 100, 100]), None).unwrap();
    assert_eq!(size, [2, 1], "intersection of [2,1,100,100] with 4x2");
    let (_, size) = super::encode_served_view(&png, Some([100, 100, 10, 10]), None).unwrap();
    assert_eq!(size, [1, 1], "fully out of range degrades to a 1px pixel");
}

#[test]
fn served_view_downscale_fits_the_long_edge_preserving_aspect() {
    let png = tiny_png(320, 160);
    let (bytes, size) = super::encode_served_view(&png, None, Some(64)).unwrap();
    let (w, h) = decode_dims(&bytes);
    assert_eq!(size, [w, h]);
    assert!(w.max(h) <= 64, "long edge fits max_dimension");
    assert_eq!(w, 64);
    assert_eq!(h, 32, "aspect preserved within rounding");
    // At-or-below the limit: no upscale.
    let (_, size) = super::encode_served_view(&png, None, Some(1000)).unwrap();
    assert_eq!(size, [320, 160]);
}

#[test]
fn served_view_applies_crop_before_resize() {
    let png = tiny_png(320, 160);
    // Cropping a 160x160 region then asking for 64: the crop's long edge (160) is what scales,
    // not the frame's — a cropped region keeps full effective resolution.
    let (bytes, size) =
        super::encode_served_view(&png, Some([0, 0, 160, 160]), Some(64)).unwrap();
    let (w, h) = decode_dims(&bytes);
    assert_eq!((w, h), (64, 64));
    assert_eq!(size, [64, 64]);
}

#[test]
fn served_view_rejects_non_png_gracefully() {
    assert!(super::encode_served_view(b"not a png", None, None).is_none());
}

// ---------------------------------------------------------------------------
// Tier 1: screenshot_get_alignment — the coordinate-mapping block.
// ---------------------------------------------------------------------------

#[test]
fn alignment_unknowable_scale_without_capture_size() {
    let mut world = World::new();
    let png = tiny_png(1280, 800);
    let alignment = super::screenshot_get_alignment(&mut world, &png, Some([640, 400]), None, None);
    // A downscaled view with no capture-size resource and no crop: the coordinate space is
    // unknowable — the harness reports null rather than guessing 1.0.
    assert_eq!(alignment["png_size"], json!([1280, 800]));
    assert_eq!(alignment["capture_size"], json!(null), "no CaptureTarget resource");
    assert_eq!(alignment["view"]["size"], json!([640, 400]));
    assert_eq!(alignment["view"]["crop"], json!(null));
    assert_eq!(alignment["view"]["coordinate_scale"], json!(null));
}

#[test]
fn alignment_reports_downscale_and_crop_scales() {
    let mut world = World::new();
    let png = tiny_png(640, 400);
    let alignment =
        super::screenshot_get_alignment(&mut world, &png, Some([640, 400]), None, Some(640));
    assert_eq!(alignment["view"]["max_dimension"], json!(640));
    // capture_size unknown (no resource) → the scale is genuinely unknowable → null.
    assert_eq!(alignment["view"]["coordinate_scale"], json!(null));

    // With a crop: scale = crop.w / view.w (crop applies before resize).
    let alignment = super::screenshot_get_alignment(
        &mut world,
        &png,
        Some([200, 150]),
        Some([200, 150, 400, 300]),
        Some(200),
    );
    assert_eq!(alignment["view"]["coordinate_scale"], json!(2.0));

    // With a capture target present feeds the scale even without a crop.
    let mut world = World::new();
    world.insert_resource(super::CaptureTarget(Handle::default()));
    let mut images = bevy::asset::Assets::default();
    images
        .insert(
            world.resource::<super::CaptureTarget>().0.id(),
            bevy::image::Image::new(
                bevy::render::render_resource::Extent3d {
                    width: 1280,
                    height: 800,
                    depth_or_array_layers: 1,
                },
                bevy::render::render_resource::TextureDimension::D2,
                vec![0; 1280 * 800 * 4],
                bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
                bevy::asset::RenderAssetUsages::default(),
            ),
        )
        .unwrap();
    world.insert_resource(images);
    let png = tiny_png(640, 400);
    let alignment = super::screenshot_get_alignment(&mut world, &png, Some([640, 400]), None, None);
    assert_eq!(alignment["capture_size"], json!([1280, 800]));
    assert_eq!(alignment["view"]["coordinate_scale"], json!(2.0));
}

#[test]
fn alignment_parses_png_ihdr_and_rejects_garbage() {
    let mut world = World::new();
    let png = tiny_png(32, 16);
    let alignment = super::screenshot_get_alignment(&mut world, &png, Some([32, 16]), None, None);
    assert_eq!(alignment["png_size"], json!([32, 16]));
    // Garbage bytes: no IHDR to read; the served view is the raw bytes.
    let alignment = super::screenshot_get_alignment(&mut world, b"junk", None, None, None);
    assert_eq!(alignment["png_size"], json!(null));
    assert_eq!(alignment["view"]["size"], json!(null));
}

// ---------------------------------------------------------------------------
// Tier 1: UiFilter and the unchanged-suppression hash.
// ---------------------------------------------------------------------------

#[test]
fn ui_filter_parses_params() {
    let f = super::UiFilter::from_params(Some(&json!({"clickable_only": true, "text_contains": "  Play  "})));
    assert!(f.clickable_only);
    assert_eq!(f.text_contains.as_deref(), Some("play"), "trimmed and lowercased");
    let f = super::UiFilter::from_params(Some(&json!({"text_contains": ""})));
    assert!(!f.clickable_only);
    assert_eq!(f.text_contains, None, "empty string means no filter");
    let f = super::UiFilter::from_params(Some(&json!({})));
    assert!(!f.clickable_only);
    assert_eq!(f.text_contains, None);
    let f = super::UiFilter::from_params(None);
    assert!(!f.clickable_only);
    assert_eq!(f.text_contains, None);
    let f = super::UiFilter::from_params(Some(&json!({"clickable_only": "yes"})));
    assert!(!f.clickable_only, "non-bool is not a filter");
}

#[test]
fn ui_filter_keeps_semantics() {
    let all = super::UiFilter::from_params(None);
    assert!(all.keeps(Some("anything"), false));
    assert!(all.keeps(None, false));

    let clickable = super::UiFilter::from_params(Some(&json!({"clickable_only": true})));
    assert!(clickable.keeps(None, true));
    assert!(!clickable.keeps(Some("Play"), false), "non-clickable dropped");

    let text = super::UiFilter::from_params(Some(&json!({"text_contains": "play"})));
    assert!(text.keeps(Some("PLAY"), false), "case-insensitive");
    assert!(text.keeps(Some("Replay"), false), "substring");
    assert!(!text.keeps(Some("Pause"), false));
    assert!(!text.keeps(None, false), "no text, no match");
    assert!(!text.keeps(None, true), "clickable without text still filtered");

    let both = super::UiFilter::from_params(Some(&json!({"clickable_only": true, "text_contains": "play"})));
    assert!(both.keeps(Some("Play"), true), "both conditions met");
    assert!(!both.keeps(Some("Play"), false), "text matches but not clickable");
    assert!(!both.keeps(Some("Pause"), true));
}

#[test]
fn hash_value_is_deterministic_and_sensitive() {
    let a = json!({"nodes": [{"text": "Play", "rect": [1, 2, 3, 4]}]});
    // Deterministic across calls (serde_json objects are BTreeMaps — key order insensitive).
    assert_eq!(super::hash_value(&a), super::hash_value(&a.clone()));
    let reordered = json!({"nodes": [{"rect": [1, 2, 3, 4], "text": "Play"}]});
    assert_eq!(
        super::hash_value(&a),
        super::hash_value(&reordered),
        "object key order must not change the hash (suppress identical re-reads)"
    );
    // Any node change → different hash (a state change must re-serve the dump).
    let changed = json!({"nodes": [{"text": "Pause", "rect": [1, 2, 3, 4]}]});
    assert_ne!(super::hash_value(&a), super::hash_value(&changed));
    let rect_moved = json!({"nodes": [{"text": "Play", "rect": [2, 2, 3, 4]}]});
    assert_ne!(super::hash_value(&a), super::hash_value(&rect_moved));
    // Array order matters (back-to-front render order is meaningful).
    let swapped = json!({"nodes": [{"text": "Pause", "rect": [1, 2, 3, 4]}, {"text": "Play", "rect": [1, 2, 3, 4]}]});
    assert_ne!(super::hash_value(&a), super::hash_value(&swapped));
}

// ---------------------------------------------------------------------------
// Tier 2: the no_render test scene — the render-less composition booted for
// real, the dump asserted over constructed trees and component states.
// ---------------------------------------------------------------------------

/// Unique-ish ports so parallel tests don't fight over 15702/15710.
fn next_test_ports() -> (u16, u16) {
    static NEXT: AtomicU16 = AtomicU16::new(18000);
    let base = NEXT.fetch_add(10, Ordering::SeqCst);
    (base, base + 1)
}

/// The render-less composition from `examples/headless.rs` (UI layout and picking are
/// render-free logic): booted here so tests exercise the REAL dump path — `UiStack`,
/// `ComputedNode`, the bootstrap UI camera, the shim — not a mock world.
pub(crate) fn no_render_app() -> App {
    let (brp_port, mcp_port) = next_test_ports();
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        FrameCountPlugin,
        TimePlugin,
        bevy::window::WindowPlugin {
            primary_window: None,
            exit_condition: ExitCondition::DontExit,
            ..default()
        },
        bevy::asset::AssetPlugin::default(),
        bevy::text::TextPlugin,
        bevy::ui::UiPlugin,
        // The widgets' text-input systems read `InputFocus`.
        bevy::input_focus::InputFocusPlugin,
        bevy::ui_widgets::UiWidgetsPlugins,
        bevy::input::InputPlugin,
        bevy::picking::PickingPlugin,
        bevy::picking::input::PointerInputPlugin,
        bevy::picking::InteractionPlugin,
        crate::BevyMcpHarnessPlugin {
            config: crate::McpHarnessConfig {
                offscreen: crate::OffscreenMode::Owned(crate::DEFAULT_OFFSCREEN_SIZE),
                no_render: true,
                brp_port,
                mcp_port,
                screenshots_dir: std::env::temp_dir().join("bevy_mcp_harness_tests"),
                ..Default::default()
            },
        },
    ));
    // `UiPlugin`'s Image-widget sizing systems read `Assets<TextureAtlasLayout>`; a render-side
    // plugin normally registers it (see examples/headless.rs for the same init).
    app.init_asset::<bevy::image::TextureAtlasLayout>();
    app
}

/// One interactive widget button with a text label child, at a known position. Carries the
/// legacy `Interaction` (the harness's default `clickable` convention) so the row is kept;
/// state tests layer the bevy-0.20 components on top.
fn spawn_button(commands: &mut Commands) -> (Entity, Entity) {
    let button = commands
        .spawn((
            #[expect(deprecated)]
            bevy::ui::Interaction::None,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(560.0),
                top: Val::Px(360.0),
                width: Val::Px(160.0),
                height: Val::Px(80.0),
                ..default()
            },
            BackgroundColor(Color::srgb(0.2, 0.4, 0.8)),
        ))
        .id();
    let label = commands
        .spawn((
            Node::default(),
            ChildOf(button),
            Text::new("Play"),
            TextFont {
                font_size: FontSize::Px(32.0),
                ..default()
            },
        ))
        .id();
    (button, label)
}

/// The button and its label, resolved through the real hierarchy (ChildOf) — the label is the
/// only (Node, Text) entity; the button is its parent. (A `(Entity, Entity)` query with
/// `With<Node> + With<Text>` returns the LABEL twice — both tuple elements are the same
/// entity — which would silently aim every state insert at the label.)
fn button_and_label(world: &mut World) -> (Entity, Entity) {
    world
        .query_filtered::<(Entity, &ChildOf), (With<Text>, With<Node>)>()
        .single(world)
        .map(|(label, child_of)| (child_of.0, label))
        .expect("the button + label pair")
}

/// The dump through the real `ui_dump_method` (params, filters, suppression).
fn dump_ui(world: &mut World, params: serde_json::Value) -> serde_json::Value {
    super::ui_dump_method(In(Some(params)), world).unwrap()
}

fn rows(dump: &serde_json::Value) -> Vec<&serde_json::Value> {
    dump["nodes"].as_array().map(|nodes| nodes.iter().collect()).unwrap_or_default()
}

fn row<'a>(dump: &'a serde_json::Value, text: &str) -> &'a serde_json::Value {
    rows(dump)
        .into_iter()
        .find(|row| row["text"] == json!(text))
        .unwrap_or_else(|| panic!("no row with text {text:?} in {}", dump["nodes"]))
}

/// `button_interacts` marks the button (bevy 0.20 widget state, no legacy components).
fn button_row_interactions(dump: &serde_json::Value) -> Vec<String> {
    rows(dump)
        .iter()
        .map(|row| row["interaction"].as_str().unwrap_or("").to_owned())
        .collect()
}

/// Marks `entity` hovered in the hover map (the authoritative hover source in bevy 0.20 —
/// the `Hovered` component is opt-in and never auto-inserted).
fn set_hovered(world: &mut World, hovered: Entity, camera: Entity) {
    let mut hits = bevy::ecs::entity::EntityHashMap::default();
    hits.insert(hovered, HitData::new(camera, 0.0, None, None));
    world.insert_resource(HoverMap(bevy::platform::collections::HashMap::from([(
        bevy::picking::pointer::PointerId::Mouse,
        hits,
    )])));
}

#[test]
fn no_render_scene_boots_and_dumps_a_button() {
    let mut app = no_render_app();
    app.add_systems(Startup, |mut commands: Commands| {
        spawn_button(&mut commands);
    });
    for _ in 0..3 {
        app.update();
    }
    // The canary: the composition boots, lays out, and the dump reads it. The bevy-0.20
    // migration's `InputFocus` panic and the double-plugin-add panic would have died here.
    let dump = dump_ui(app.world_mut(), json!({}));
    let row = row(&dump, "Play");
    assert_eq!(row["clickable"], json!(true), "the button is interactive");
    assert_eq!(row["interaction"], json!("Idle"));
    let rect: Vec<i64> = row["rect"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    assert_eq!(rect, [560, 360, 160, 80], "laid-out rect in pixel space");
}

#[test]
fn dump_interaction_state_machine() {
    let mut app = no_render_app();
    app.add_systems(Startup, |mut commands: Commands| {
        spawn_button(&mut commands);
    });
    for _ in 0..3 {
        app.update();
    }
    let camera = app
        .world_mut()
        .query_filtered::<Entity, With<Camera>>()
        .single(app.world())
        .expect("the harness's bootstrap UI camera");
    let (button, label) = button_and_label(app.world_mut());

    // Idle baseline.
    let dump = dump_ui(app.world_mut(), json!({}));
    assert_eq!(row(&dump, "Play")["interaction"], json!("Idle"));

    // Hover the LABEL child: picking hovers the topmost node, and the kept interactive
    // ancestor (the button) must read Hovered — this is the bevy-0.20 gap that read Idle.
    // (No `app.update()` after the insert: the picking hover systems recompute the map from
    // pointer input, which has no hits here — the inserted map IS the fixture.)
    set_hovered(app.world_mut(), label, camera);
    let dump = dump_ui(app.world_mut(), json!({}));
    assert_eq!(row(&dump, "Play")["interaction"], json!("Hovered"));

    // Press (the widget's `Pressed` component on the button) — beats Hovered.
    app.world_mut().entity_mut(button).insert(Pressed);
    let dump = dump_ui(app.world_mut(), json!({}));
    assert_eq!(row(&dump, "Play")["interaction"], json!("Pressed"));

    // Release: back to Hovered.
    app.world_mut().entity_mut(button).remove::<Pressed>();
    let dump = dump_ui(app.world_mut(), json!({}));
    assert_eq!(row(&dump, "Play")["interaction"], json!("Hovered"));

    // Pointer away: Idle again.
    world_clear_hover(app.world_mut());
    let dump = dump_ui(app.world_mut(), json!({}));
    assert_eq!(row(&dump, "Play")["interaction"], json!("Idle"));
}

fn world_clear_hover(world: &mut World) {
    world.remove_resource::<HoverMap>();
}

/// The legacy convention still reads: `Interaction` (deprecated but maintained) is what
/// non-widget hosts use; its Pressed/Hovered map into the same field.
#[test]
fn dump_reads_legacy_interaction() {
    let mut app = no_render_app();
    app.add_systems(Startup, |mut commands: Commands| {
        spawn_button(&mut commands);
    });
    for _ in 0..3 {
        app.update();
    }
    // Insert AFTER the last update: `ui_focus_system` resets `Interaction` to `None` every
    // frame for nodes the cursor isn't over — same live behavior as a real host.
    let (button, _) = button_and_label(app.world_mut());
    #[expect(deprecated)]
    app.world_mut().entity_mut(button).insert(bevy::ui::Interaction::Hovered);
    let dump = dump_ui(app.world_mut(), json!({}));
    assert_eq!(row(&dump, "Play")["interaction"], json!("Hovered"), "dump: {dump}");
}

/// The fold must not hide a pressed state behind a stronger-looking sibling, and Pressed
/// must beat Hovered regardless of which node carries them.
#[test]
fn dump_pressed_beats_hovered_across_subtree() {
    let mut app = no_render_app();
    app.add_systems(Startup, |mut commands: Commands| {
        spawn_button(&mut commands);
    });
    for _ in 0..3 {
        app.update();
    }
    let camera = app
        .world_mut()
        .query_filtered::<Entity, With<Camera>>()
        .single(app.world())
        .unwrap();
    let (button, label) = button_and_label(app.world_mut());
    // Label hovered, button pressed: the strongest subtree state wins.
    set_hovered(app.world_mut(), label, camera);
    app.world_mut().entity_mut(button).insert(Pressed);
    let dump = dump_ui(app.world_mut(), json!({}));
    assert_eq!(row(&dump, "Play")["interaction"], json!("Pressed"));
}

#[test]
fn dump_filters_and_unchanged_suppression() {
    let mut app = no_render_app();
    app.add_systems(Startup, |mut commands: Commands| {
        spawn_button(&mut commands);
    });
    for _ in 0..3 {
        app.update();
    }
    // clickable_only keeps the interactive row.
    let dump = dump_ui(app.world_mut(), json!({"clickable_only": true}));
    assert_eq!(rows(&dump).len(), 1);
    // text_contains: hit and miss.
    let dump = dump_ui(app.world_mut(), json!({"refresh": true, "text_contains": "play"}));
    assert_eq!(rows(&dump).len(), 1);
    let dump = dump_ui(app.world_mut(), json!({"refresh": true, "text_contains": "pause"}));
    assert_eq!(rows(&dump).len(), 0);

    // Unchanged suppression: an identical re-read omits `nodes`; a state change re-serves.
    let first = dump_ui(app.world_mut(), json!({"refresh": true}));
    assert!(first["nodes"].is_array());
    let second = dump_ui(app.world_mut(), json!({}));
    assert_eq!(second["unchanged"], json!(true));
    assert_eq!(second["node_count"], json!(1));
    assert!(second["nodes"].is_null());
    // A hover changes the interaction field → different hash → full dump.
    let camera = app
        .world_mut()
        .query_filtered::<Entity, With<Camera>>()
        .single(app.world())
        .unwrap();
    let (_, label) = button_and_label(app.world_mut());
    set_hovered(app.world_mut(), label, camera);
    let third = dump_ui(app.world_mut(), json!({}));
    assert_eq!(third["unchanged"], json!(null));
    assert_eq!(row(&third, "Play")["interaction"], json!("Hovered"));
}

#[test]
fn dump_serves_the_clickable_hook() {
    let (brp_port, mcp_port) = next_test_ports();
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        FrameCountPlugin,
        TimePlugin,
        bevy::window::WindowPlugin {
            primary_window: None,
            exit_condition: ExitCondition::DontExit,
            ..default()
        },
        bevy::asset::AssetPlugin::default(),
        bevy::text::TextPlugin,
        bevy::ui::UiPlugin,
        bevy::input_focus::InputFocusPlugin,
        bevy::ui_widgets::UiWidgetsPlugins,
        bevy::input::InputPlugin,
        bevy::picking::PickingPlugin,
        bevy::picking::input::PointerInputPlugin,
        bevy::picking::InteractionPlugin,
        crate::BevyMcpHarnessPlugin {
            config: crate::McpHarnessConfig {
                offscreen: crate::OffscreenMode::Owned(crate::DEFAULT_OFFSCREEN_SIZE),
                no_render: true,
                brp_port,
                mcp_port,
                screenshots_dir: std::env::temp_dir().join("bevy_mcp_harness_tests"),
                // A host convention without `Interaction`: the widget button.
                clickable: Some(std::sync::Arc::new(|world, entity| {
                    world.get::<bevy::ui_widgets::Button>(entity).is_some()
                })),
                ..Default::default()
            },
        },
    ));
    // Same render-side gap as `no_render_app`.
    app.init_asset::<bevy::image::TextureAtlasLayout>();
    app.add_systems(Startup, |mut commands: Commands| {
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(100.0),
                top: Val::Px(100.0),
                width: Val::Px(64.0),
                height: Val::Px(32.0),
                ..default()
            },
            bevy::ui_widgets::Button,
            children![(
                Node::default(),
                Text::new("Start"),
                TextFont {
                    font_size: FontSize::Px(24.0),
                    ..default()
                },
            )],
        ));
    });
    for _ in 0..3 {
        app.update();
    }
    let dump = dump_ui(app.world_mut(), json!({}));
    assert_eq!(row(&dump, "Start")["clickable"], json!(true));
}

// ---------------------------------------------------------------------------
// Tier 2: entities_on_screen projection — the local-space Aabb and depth math
// (two real bugs found in this code path), verified against a hand-computed
// camera (the `computed` fields are exactly what the render app writes).
// ---------------------------------------------------------------------------

/// A synthetic 3D camera at the origin looking down -Z (bevy's RH convention), 90° vertical
/// fov over a 100x100 viewport: a point at view-space (0, 0, -d) projects to the center, and
/// world offset ±d/√2 at that depth lands on the frame edge.
fn projection_camera(world: &mut World) -> Entity {
    let fov: f32 = std::f32::consts::FRAC_PI_2;
    #[expect(deprecated)] // glam renamed the constructor; the matrix shape is what the test needs
    let clip_from_view = Mat4::perspective_rh(fov, 1.0, 0.1, 100.0);
    world
        .spawn((
            Camera {
                computed: bevy::camera::ComputedCameraValues {
                    clip_from_view,
                    target_info: Some(bevy::camera::RenderTargetInfo {
                        physical_size: UVec2::splat(100),
                        scale_factor: 1.0,
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
            bevy::camera::Camera3d::default(),
            GlobalTransform::default(),
        ))
        .id()
}

fn aabb_entity(
    world: &mut World,
    name: &str,
    translation: Vec3,
    center: Vec3,
    half_extents: f32,
) -> Entity {
    world
        .spawn((
            Aabb {
                center: Vec3A::from(center),
                half_extents: Vec3A::splat(half_extents),
            },
            GlobalTransform::from_translation(translation),
            InheritedVisibility::VISIBLE,
            ViewVisibility::VISIBLE,
            Name::new(name.to_string()),
        ))
        .id()
}

fn visible_entities(world: &mut World) -> Vec<(String, [f64; 2], [f64; 4], f64)> {
    super::screenshot_get_entities(world)
        .as_array()
        .expect("entities array")
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap().to_owned(),
                [
                    e["center"][0].as_f64().unwrap(),
                    e["center"][1].as_f64().unwrap(),
                ],
                [
                    e["bounding_box"][0].as_f64().unwrap(),
                    e["bounding_box"][1].as_f64().unwrap(),
                    e["bounding_box"][2].as_f64().unwrap(),
                    e["bounding_box"][3].as_f64().unwrap(),
                ],
                e["depth"].as_f64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn projection_projects_local_aabbs_through_the_world_transform() {
    let mut world = World::new();
    projection_camera(&mut world);
    // An entity whose LOCAL aabb is offset: world center must be local + translation —
    // this is the local-space bug (projecting `aabb.center` directly returned 0 entities
    // / wrong pixels for every translated entity).
    aabb_entity(
        &mut world,
        "offset",
        Vec3::new(0.0, 0.0, -2.0),
        Vec3::new(0.5, 0.0, 0.0),
        1.0,
    );
    let entities = visible_entities(&mut world);
    assert_eq!(entities.len(), 1);
    let (name, center, _, depth) = &entities[0];
    assert_eq!(name, "offset");
    // A 90° fov over 100px: half the viewport height maps to tan(45°)=1 per unit depth.
    // World center (0.5, 0, -2): x_ndc = 0.5/2 → px = 50 + 12.5.
    assert!((center[0] - 62.5).abs() < 2.0, "center {center:?}");
    assert!((center[1] - 50.0).abs() < 2.0, "center {center:?}");
    // Depth is the camera→WORLD-center distance: √(2² + 0.5²) ≈ 2.06 (the local-center bug
    // would have read ≈0.5 — camera distance to the origin-local center).
    assert!((depth - 2.0615528).abs() < 0.02, "depth {depth}");
}

#[test]
fn projection_sorts_nearest_first_and_excludes_behind_camera() {
    let mut world = World::new();
    projection_camera(&mut world);
    aabb_entity(&mut world, "near", Vec3::new(0.0, 0.0, -2.0), Vec3::ZERO, 1.0);
    aabb_entity(&mut world, "far", Vec3::new(0.0, 0.0, -6.0), Vec3::ZERO, 1.0);
    aabb_entity(&mut world, "behind", Vec3::new(0.0, 0.0, 2.0), Vec3::ZERO, 1.0);
    let entities = visible_entities(&mut world);
    assert_eq!(
        entities.iter().map(|e| e.0.as_str()).collect::<Vec<_>>(),
        vec!["near", "far"],
        "behind-camera culled, nearest-first sort"
    );
    let (_, _, _, near_depth) = &entities[0];
    let (_, _, _, far_depth) = &entities[1];
    assert!((near_depth - 2.0).abs() < 0.05, "near {near_depth}");
    assert!((far_depth - 6.0).abs() < 0.05, "far {far_depth}");
    assert!(near_depth < far_depth);
}

#[test]
fn projection_projects_all_corners_for_2d_bbox() {
    let mut world = World::new();
    projection_camera(&mut world);
    // Half extent 0.5 at depth 2, centered: corners span z ∈ [-2.5, -1.5], x/y ∈ ±0.5. The
    // bbox comes from the WIDEST projection — the near face (depth 1.5): ndc ±0.5/1.5 →
    // px 33.3..66.7 on the 100px viewport. (The center projects to 50,50.)
    aabb_entity(&mut world, "cube", Vec3::new(0.0, 0.0, -2.0), Vec3::ZERO, 0.5);
    let entities = visible_entities(&mut world);
    let (_, center, bbox, _) = &entities[0];
    assert!((center[0] - 50.0).abs() < 2.0);
    assert!((center[1] - 50.0).abs() < 2.0);
    assert!(
        (bbox[0] - 33.3).abs() < 2.0 && (bbox[1] - 33.3).abs() < 2.0,
        "bbox origin {bbox:?}"
    );
    assert!(
        (bbox[2] - 33.3).abs() < 2.0 && (bbox[3] - 33.3).abs() < 2.0,
        "bbox size {bbox:?}"
    );
}

#[test]
fn projection_excludes_invisible_entities() {
    let mut world = World::new();
    projection_camera(&mut world);
    let visible = aabb_entity(&mut world, "visible", Vec3::new(0.0, 0.0, -2.0), Vec3::ZERO, 1.0);
    let hidden = aabb_entity(&mut world, "hidden", Vec3::new(0.0, 0.0, -3.0), Vec3::ZERO, 1.0);
    world.entity_mut(hidden).insert(InheritedVisibility::HIDDEN);
    let _ = visible;
    let entities = visible_entities(&mut world);
    assert_eq!(
        entities.iter().map(|e| e.0.as_str()).collect::<Vec<_>>(),
        vec!["visible"]
    );
}

#[test]
fn button_row_interactions_helper_reads_rows() {
    // Guard on the test helper itself: an all-empty dump yields empty interactions.
    let dump = json!({"nodes": []});
    assert!(button_row_interactions(&dump).is_empty());
}
