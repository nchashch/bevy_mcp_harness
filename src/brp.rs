//! The custom BRP methods — the game tools. Handlers run in the main world with `&mut World`
//! access (registered as plain systems into `bevy::remote::RemoteMethods`); the MCP layer
//! proxies to them over loopback HTTP. Ported from PROTOTYPE_19's `dev/tool_api.rs`, minus its
//! game-specific methods (`game/input` action mocks, `game/select`, `game/trigger`,
//! `game/levels`, `game/select_level` — all bound to that prototype's own crates).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bevy::camera::RenderTarget;
use bevy::ecs::query::QueryState;
use bevy::picking::Pickable;
use bevy::picking::hover::Hovered as PickHovered;
use bevy::prelude::*;
use bevy::remote::{BrpError, BrpResult};
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use bevy::text::TextSpan;
use bevy::ui::{ComputedUiTargetCamera, UiGlobalTransform, UiStack};
use serde_json::json;

use crate::headless::OffscreenRenderTarget;
use crate::{McpHarnessConfig, NoRenderMode};

// ---------------------------------------------------------------------------
// game/state
// ---------------------------------------------------------------------------

/// `game/state` — the host-registered snapshot (see [`crate::StateSnapshotFn`]). An empty
/// object when the host registered no hook. Params are ignored.
pub(crate) fn game_state_method(_params: In<Option<serde_json::Value>>, world: &mut World) -> BrpResult {
    Ok(game_state_snapshot(world).into())
}

/// The `game/state` payload as a plain JSON value. Shared by the `game/state` method and
/// [`screenshot_get_method`], which fuses it into every capture response (and writes it to a
/// `.json` sidecar next to the PNG) — a screenshot arrives with its ground-truth state
/// attached, so the agent never has to OCR the HUD or correlate "which call came after which
/// action". Snapshot is taken at *poll* time, i.e. a few hundred ms after the capture started;
/// that is the state the agent wants anyway (the world as it is right after its action), and
/// the capture→poll gap is bounded by the poll loop (~100ms granularity).
fn game_state_snapshot(world: &mut World) -> serde_json::Value {
    let hook = world
        .get_resource::<McpHarnessConfig>()
        .and_then(|config| config.state_snapshot.clone());
    match hook {
        Some(hook) => hook(world),
        None => json!({}),
    }
}

// ---------------------------------------------------------------------------
// game/client_info, game/cameras
// ---------------------------------------------------------------------------

/// `game/client_info` — reports this harness's launch configuration (mode flags + surface
/// ports). Call first on a fresh session: it tells you whether screenshots exist at all,
/// where captures land, and which port each surface is on.
pub(crate) fn client_info_method(_params: In<Option<serde_json::Value>>, world: &mut World) -> BrpResult {
    let Some(config) = world.get_resource::<McpHarnessConfig>() else {
        return Err(BrpError::internal(
            "McpHarnessConfig resource missing (was BevyMcpHarnessPlugin built?)",
        ));
    };
    Ok(json!({
        "no_render": config.no_render,
        "brp_port": config.brp_port,
        "mcp_port": config.mcp_port,
        "screenshots_dir": config.screenshots_dir.display().to_string(),
        "screenshots_available": !config.no_render,
        "rendering": !config.no_render,
        "target_size": world
            .get_resource::<OffscreenRenderTarget>()
            .and_then(|target| {
                world
                    .get_resource::<Assets<Image>>()
                    .and_then(|images| images.get(&target.0))
                    .map(|image| vec![image.size().x, image.size().y])
            }),
    })
    .into())
}

/// `game/cameras` — lists every camera: entity id (usable as `game/screenshot`'s `camera`
/// param), position, look angles, name marker, and whether it's active. In headless mode this
/// is how an agent finds a named camera to aim and render from.
pub(crate) fn cameras_method(_params: In<Option<serde_json::Value>>, world: &mut World) -> BrpResult {
    let mut cameras = world.query_filtered::<(
        Entity,
        &bevy::camera::Camera,
        Option<&Transform>,
        Option<&Name>,
        Option<&bevy::camera::visibility::VisibleEntities>,
    ), ()>();
    let mut rows = Vec::new();
    for (entity, camera, transform, name, _) in cameras.iter(world) {
        let transform = transform.cloned().unwrap_or_default();
        let forward = *transform.forward();
        let yaw = forward.z.atan2(forward.x);
        let pitch = forward.y.asin();
        rows.push(json!({
            "entity": entity,
            "name": name.map(|name| name.as_str()),
            "position": [transform.translation.x, transform.translation.y, transform.translation.z],
            "forward": [forward.x, forward.y, forward.z],
            "yaw": yaw, "pitch": pitch,
            "active": camera.is_active,
        }));
    }
    Ok(json!({
        "note": "Camera entities. entity = u64 id usable as game/screenshot's `camera` param. yaw/pitch in radians (forward = +X at yaw 0). Reposition via world.mutate_components (Transform).",
        "cameras": rows,
    })
    .into())
}

// ---------------------------------------------------------------------------
// game/screenshot + game/screenshot/get
// ---------------------------------------------------------------------------

/// This session's capture directory: the configured [`McpHarnessConfig::screenshots_dir`].
fn screenshots_dir(world: &World) -> Option<PathBuf> {
    world
        .get_resource::<McpHarnessConfig>()
        .map(|config| config.screenshots_dir.clone())
}

/// A new unique capture path: `<utc-millis>-<label>.png`, millisecond-resolution so names sort
/// chronologically. A same-millisecond collision (two captures in one instant) appends a
/// counter suffix.
fn next_screenshot_path(dir: &Path, label: Option<&str>) -> PathBuf {
    let _ = std::fs::create_dir_all(dir);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let label = label.unwrap_or("capture");
    let stem = format!("{millis}-{label}");
    let mut path = dir.join(format!("{stem}.png"));
    let mut disambiguator = 1u32;
    while path.exists() {
        path = dir.join(format!("{stem}-{disambiguator}.png"));
        disambiguator += 1;
    }
    path
}

/// The newest capture currently on disk (what `game/screenshot/get` reports). The filenames are
/// millisecond timestamps, so lexicographic max = chronological max.
fn newest_screenshot(dir: &Path) -> Option<PathBuf> {
    let mut newest: Option<(std::ffi::OsString, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "png") {
            let name = entry.file_name();
            if newest.as_ref().is_none_or(|(best, _)| name > *best) {
                newest = Some((name, path));
            }
        }
    }
    newest.map(|(_, path)| path)
}

/// `game/screenshot` — starts an async capture. The PNG is written into the configured
/// screenshots directory (encoded by [`save_cropped_to_disk`], async); poll
/// `game/screenshot/get` until it reports `ready`. Optional params: `{"label": "..."}` for the
/// filename, `{"crop": [x, y, w, h]}` to save only that sub-rect — in the same screenshot pixel
/// space `game/ui` dumps, so "crop to a button's rect from the dump" just works. A crop costs
/// the model fewer vision tokens (cost is dimension-driven) and, unlike a full frame, a small
/// crop survives the provider's downscale unscaled — full effective resolution on the region
/// of interest. The file PERSISTS (it is the human-browsable record of what the agent saw), so
/// this also returns the path immediately. `{"camera": <entity id>}` (from `game/cameras`)
/// temporarily raises that camera's draw order so the capture is its view.
pub(crate) fn screenshot_start_method(params: In<Option<serde_json::Value>>, world: &mut World) -> BrpResult {
    let label = params
        .0
        .as_ref()
        .and_then(|p| p.get("label"))
        .and_then(serde_json::Value::as_str);
    if world.get_resource::<NoRenderMode>().is_some() {
        return Err(BrpError::internal(
            "no rendering enabled (McpHarnessConfig::no_render): screenshots are unavailable; use \
             game/ui for on-screen content and game/state for ground truth",
        ));
    }
    let crop = match params.0.as_ref().and_then(|p| p.get("crop")) {
        Some(value) => Some(parse_crop(value).ok_or_else(|| {
            BrpError::internal("crop must be [x, y, w, h] — four pixel numbers")
        })?),
        None => None,
    };
    // Optional camera entity (as the u64 id `game/cameras` reports): temporarily retarget that
    // camera into a dedicated capture texture, render, and restore its original target. Only
    // meaningful when a render app exists.
    let capture_camera = match params.0.as_ref().and_then(|p| p.get("camera")) {
        Some(value) => {
            if world.get_resource::<NoRenderMode>().is_some() {
                return Err(BrpError::internal(
                    "camera captures are unavailable with no_render (nothing renders)",
                ));
            }
            let bits = value
                .as_u64()
                .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
                .ok_or_else(|| {
                    BrpError::internal("camera must be the u64 entity id game/cameras reports")
                })?;
            let entity = bevy::ecs::entity::Entity::from_bits(bits);
            if world.get_entity(entity).is_err() {
                return Err(BrpError::internal(&format!(
                    "camera entity {bits} does not exist (use game/cameras to list cameras)"
                )));
            }
            Some(entity)
        }
        None => None,
    };
    let Some(dir) = screenshots_dir(world) else {
        return Err(BrpError::internal(
            "McpHarnessConfig resource missing (was BevyMcpHarnessPlugin built?)",
        ));
    };
    let path = next_screenshot_path(&dir, label);
    // Camera-targeted capture: raise the requested camera's draw order above everything else
    // for this frame, screenshot the shared offscreen texture (which every camera here renders
    // into), and restore order/clear afterwards. Reusing the offscreen texture (instead of a
    // fresh one) matters: the render app only knows textures it has already prepared — a
    // brand-new image makes `Screenshot` warn "Unknown image … skipping" forever.
    if let Some(camera_entity) = capture_camera {
        let (original_order, original_clear) = match world.get::<Camera>(camera_entity) {
            Some(camera) => (camera.order, camera.clear_color),
            None => {
                return Err(BrpError::internal(&format!(
                    "camera entity {camera_entity:?} does not exist (use game/cameras to list cameras)"
                )));
            }
        };
        if let Some(mut camera) = world.get_mut::<Camera>(camera_entity) {
            camera.order = 900_000;
            camera.clear_color = ClearColorConfig::Default;
        }
        world.insert_resource(CameraCaptureRestore {
            entity: camera_entity,
            original_order,
            original_clear,
        });
    }
    // Headless (offscreen target configured): the cameras render into the offscreen texture —
    // capture THAT. Windowed: capture the primary window.
    let capture_target = world
        .get_resource::<OffscreenRenderTarget>()
        .map(|target| Screenshot(bevy::camera::RenderTarget::Image(target.0.clone().into())))
        .unwrap_or_else(Screenshot::primary_window);
    world
        .spawn(capture_target)
        .observe(save_cropped_to_disk(path.clone(), crop))
        .observe(restore_camera_order);
    Ok(json!({
        "status": "capturing",
        "poll": "game/screenshot/get",
        "path": path.display().to_string(),
        "crop": crop,
    })
    .into())
}

/// The camera whose draw order was raised for a camera-targeted capture, and what to put back.
/// Written by `screenshot_start_method`, consumed by [`restore_camera_order`].
#[derive(Resource)]
struct CameraCaptureRestore {
    entity: Entity,
    original_order: isize,
    original_clear: ClearColorConfig,
}

/// Runs on the capture entity when it completes: puts the borrowed camera's original draw
/// order/clear config back. Runs after [`save_cropped_to_disk`] (which only reads the
/// already-transferred image), so the restore never races the encode.
fn restore_camera_order(
    _captured: On<ScreenshotCaptured>,
    restore: Option<Res<CameraCaptureRestore>>,
    mut cameras: Query<&mut Camera>,
    mut commands: Commands,
) {
    if let Some(restore) = restore {
        if let Ok(mut camera) = cameras.get_mut(restore.entity) {
            camera.order = restore.original_order;
            camera.clear_color = restore.original_clear;
        }
        commands.remove_resource::<CameraCaptureRestore>();
    }
}

/// Parses `game/screenshot`'s `crop` param: `[x, y, w, h]`, four finite pixel numbers
/// (floats rounded), all non-negative. `None` on anything else.
fn parse_crop(value: &serde_json::Value) -> Option<[u32; 4]> {
    let array = value.as_array()?;
    if array.len() != 4 {
        return None;
    }
    array
        .iter()
        .map(|v| {
            let n = v.as_f64()?;
            if !n.is_finite() || n < 0.0 {
                return None;
            }
            Some(n.round() as u32)
        })
        .collect::<Option<Vec<_>>>()
        .map(|parts| [parts[0], parts[1], parts[2], parts[3]])
}

/// `Screenshot`'s built-in `save_to_disk`'s crop-capable twin: encodes the captured frame's
/// sub-rect `crop` (or the whole frame when `None`) to `<path>` as PNG, clamping the rect to
/// the frame's bounds so an out-of-range crop degrades to the intersection instead of
/// panicking. Mirrors `save_to_disk`'s HDR-safety (`to_rgb8` drops the alpha channel). The
/// capture itself is always of the full render target — the crop is applied at encode time.
fn save_cropped_to_disk(
    path: impl AsRef<Path>,
    crop: Option<[u32; 4]>,
) -> impl FnMut(On<ScreenshotCaptured>) {
    let path = path.as_ref().to_owned();
    move |captured: On<ScreenshotCaptured>| {
        let Ok(dyn_img) = captured.image.clone().try_into_dynamic() else {
            error!(
                "screenshot crop: unsupported capture format at {}",
                path.display()
            );
            return;
        };
        let rgb = dyn_img.to_rgb8();
        let cropped = match crop {
            None => image::DynamicImage::ImageRgb8(rgb),
            Some([x, y, w, h]) => {
                let x = x.min(rgb.width().saturating_sub(1));
                let y = y.min(rgb.height().saturating_sub(1));
                let w = w.min(rgb.width() - x).max(1);
                let h = h.min(rgb.height() - y).max(1);
                image::DynamicImage::ImageRgb8(image::imageops::crop_imm(&rgb, x, y, w, h).to_image())
            }
        };
        match cropped.save_with_format(&path, image::ImageFormat::Png) {
            Ok(_) => info!("Screenshot saved to {} (crop: {crop:?})", path.display()),
            Err(e) => error!("Cannot save screenshot, IO error: {e}"),
        }
    }
}

/// The pixels of the last capture `game/screenshot/get` served in full (hash of the PNG bytes).
/// If the newest capture's pixels hash identically, the poll answers `unchanged: true` without
/// re-sending the image — agents poll while waiting on state transitions and routinely re-read
/// pixel-identical frames (e.g. a static view while `game/state` changes underneath). Resource
/// rather than `Local` because the handlers are registered as plain systems.
#[derive(Resource, Default)]
pub struct LastServedCapture {
    hash: Option<u64>,
    path: Option<PathBuf>,
}

/// `game/screenshot/get` — polls the newest capture: `{"ready": true, "png_base64": …, "path":
/// …, "state": …}` once a PNG is on disk, `{"ready": false}` while still rendering. If the
/// newest capture's pixels are identical to the last capture served in full, responds
/// `{"ready": true, "unchanged": true, "path", "state"}` WITHOUT `png_base64` — the agent
/// already has this exact image; the fresh `state` is still included since the world can
/// change under a static view. The response embeds the `game/state` ground truth, and the same
/// snapshot is written once to a `.json` sidecar beside the PNG (`<capture>.json`) so the
/// human-browsable record carries state too. The file is NOT consumed — captures persist for
/// human review.
pub(crate) fn screenshot_get_method(_params: In<Option<serde_json::Value>>, world: &mut World) -> BrpResult {
    let Some(dir) = screenshots_dir(world) else {
        return Err(BrpError::internal(
            "McpHarnessConfig resource missing (was BevyMcpHarnessPlugin built?)",
        ));
    };
    let Some(path) = newest_screenshot(&dir) else {
        return Ok(json!({"ready": false}).into());
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(json!({"ready": false}).into()),
    };
    let state = game_state_snapshot(world);

    // Unchanged-frame suppression: PNG bytes are a deterministic function of the frame
    // (same encoder, same pixels → same bytes), so hashing the file hashes the frame.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&bytes, &mut hasher);
    let hash = std::hash::Hasher::finish(&hasher);
    let unchanged = world
        .get_resource::<LastServedCapture>()
        .is_some_and(|last| last.hash == Some(hash));
    if unchanged {
        return Ok(json!({
            "ready": true,
            "unchanged": true,
            "path": path.display().to_string(),
            "state": state,
        })
        .into());
    }
    if let Some(mut last) = world.get_resource_mut::<LastServedCapture>() {
        last.hash = Some(hash);
        last.path = Some(path.clone());
    }

    {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let sidecar = path.with_extension("json");
        if !sidecar.exists() {
            let record = json!({
                "screenshot": path.display().to_string(),
                "state": state,
            });
            if let Ok(text) = serde_json::to_string_pretty(&record) {
                let _ = std::fs::write(&sidecar, text);
            }
        }
        Ok(json!({
            "ready": true,
            "png_base64": encoded,
            "path": path.display().to_string(),
            "state": state,
        })
        .into())
    }
}

// ---------------------------------------------------------------------------
// Agent vision aids — everything here exists so a vision model reads LESS
// off the pixels: `game/ui` gives labeled rects + text instead of OCR, and
// the agent cursor makes the mocked pointer's position/hover observable.
// ---------------------------------------------------------------------------

/// `game/ui` — an accessibility-tree-style dump of the current UI: every laid-out node in
/// back-to-front render order with its rect **in screenshot pixel space** (the same space
/// `game/mouse`'s `move_to`/clicks consume), plus per-node text (a text block's `Text` +
/// `TextSpan` runs), clickability (a `bevy_ui` `Interaction` holder — buttons), pressed/hovered
/// state, bevy_picking's real hovered-entity set, and the mocked pointer's position. Turns
/// "find the button in the image and guess its pixel center" into "read the row, click its
/// rect" — and doubles as ground truth for verifying text actually rendered (a screenshot shows
/// tofu/blank for missing fonts; this shows the string either way, so disagreement between the
/// two localizes the failure).
///
/// Headless caveat: `interaction` mirrors `bevy_ui::Interaction`, and upstream's
/// `ui_focus_system` only updates `Interaction` for cameras rendering to a *window* — on an
/// offscreen target it stays `Idle`. The picking-backed fields (`pointer_hovered`,
/// `hovered_entities`) are the reliable headless hover/press signal; press state is additionally
/// visible through the mocked left button (`game/mouse` "button").
///
/// Node filtering: zero-size (`Display::None`/collapsed), invisible, and rotated nodes are
/// skipped (a rotated node's axis-aligned rect would be a lie). Text rows whose text is already
/// included in a dumped interactive ancestor's subtree (a button's label) are folded into that
/// ancestor and not emitted twice. The agent cursor's own overlay nodes are excluded. In
/// headless mode only nodes targeting the offscreen capture are dumped; windowed dumps
/// everything.
pub(crate) fn ui_dump_method(_params: In<Option<serde_json::Value>>, world: &mut World) -> BrpResult {
    Ok(ui_dump_snapshot(world).into())
}

/// The cursor overlay's root node — zero-size, absolutely positioned, follows the mocked
/// pointer. Bars are its children. Excluded from [`ui_dump_snapshot`].
#[derive(Component)]
pub(crate) struct AgentCursorRoot;

/// One crosshair arm of the [`AgentCursorRoot`] overlay.
#[derive(Component)]
pub(crate) struct AgentCursorBar;

/// Red: idle. Yellow: the pointer is over something (bevy_picking's `HoverMap` non-empty).
/// White: left button held.
const CURSOR_IDLE: Color = Color::srgb(1.0, 0.25, 0.25);
const CURSOR_HOVER: Color = Color::srgb(1.0, 0.85, 0.1);
const CURSOR_PRESSED: Color = Color::srgb(1.0, 1.0, 1.0);
/// Crosshair arm length / thickness, logical px (× the UI scale factor on screen).
const CURSOR_ARM: f32 = 14.0;
const CURSOR_THICK: f32 = 3.0;

/// Lazily spawns the agent cursor overlay iff an offscreen target exists (on a real desktop the
/// OS cursor is already visible and an extra crosshair would be noise), and despawns it if the
/// target goes away. Polling rather than an observer because the resource may be inserted after
/// plugin build, matching the "can appear at any time" precedent.
pub(crate) fn spawn_agent_cursor_if_headless(
    offscreen: Option<Res<OffscreenRenderTarget>>,
    existing: Query<Entity, With<AgentCursorRoot>>,
    mut commands: Commands,
) {
    if offscreen.is_some() {
        if !existing.is_empty() {
            return;
        }
        let bar = |width: f32, height: f32| {
            (
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(-width / 2.0),
                    top: Val::Px(-height / 2.0),
                    width: Val::Px(width),
                    height: Val::Px(height),
                    ..default()
                },
                BackgroundColor(CURSOR_IDLE),
                Outline {
                    width: Val::Px(1.0),
                    offset: Val::ZERO,
                    color: Color::BLACK,
                },
                // The overlay must never be hit by bevy_picking — it would occlude the UI
                // under the pointer and break exactly the hover/click state it exists to show.
                Pickable::IGNORE,
                AgentCursorBar,
            )
        };
        commands
            .spawn((
                AgentCursorRoot,
                GlobalZIndex(i32::MAX),
                Pickable::IGNORE,
                Node {
                    position_type: PositionType::Absolute,
                    // Offscreen until the first follow update knows the pointer position.
                    left: Val::Px(-1000.0),
                    top: Val::Px(-1000.0),
                    width: Val::Px(0.0),
                    height: Val::Px(0.0),
                    ..default()
                },
            ))
            .with_children(|parent| {
                parent.spawn(bar(CURSOR_ARM, CURSOR_THICK));
                parent.spawn(bar(CURSOR_THICK, CURSOR_ARM));
            });
    } else {
        for entity in &existing {
            commands.entity(entity).despawn();
        }
    }
}

/// Repositions the cursor overlay onto the mocked pointer and recolors it by hover/press
/// state. `PointerLocation.position` and `ComputedNode`'s rect math are both in physical
/// render-target pixels, but `Val::Px` resolves through the layout scale factor (target scale
/// × `UiScale`) — hence the round-trip through the node's own `inverse_scale_factor`, the
/// same factor `ui_layout_system` derived, so the overlay lands exactly on the pointer
/// whatever the scale.
pub(crate) fn update_agent_cursor(
    offscreen: Option<Res<OffscreenRenderTarget>>,
    images: Option<Res<Assets<Image>>>,
    ui_scale: Res<UiScale>,
    pointers: Query<(
        &bevy::picking::pointer::PointerId,
        &bevy::picking::pointer::PointerLocation,
    )>,
    hover: Option<Res<bevy::picking::hover::HoverMap>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut roots: Query<
        (&mut Node, Option<&ComputedNode>),
        (With<AgentCursorRoot>, Without<AgentCursorBar>),
    >,
    mut bars: Query<&mut BackgroundColor, With<AgentCursorBar>>,
) {
    if offscreen.is_none() {
        return;
    }
    let Ok((mut root_node, computed)) = roots.single_mut() else {
        return;
    };

    let physical = pointers
        .iter()
        .find(|(id, _)| **id == AGENT_POINTER)
        .and_then(|(_, loc)| loc.location.as_ref())
        .map(|location| location.position)
        // Same fallback `current_pointer_location` uses: the offscreen target's center.
        .unwrap_or_else(|| {
            offscreen
                .as_ref()
                .zip(images.as_ref())
                .and_then(|(target, images)| target.size(images))
                .map(|size| size.as_vec2() / 2.0)
                .unwrap_or(Vec2::new(640.0, 400.0))
        });

    let inverse = computed
        .and_then(|node| (node.inverse_scale_factor > 0.0).then_some(node.inverse_scale_factor))
        .unwrap_or_else(|| ui_scale.0.recip());
    let logical = physical * inverse;
    if root_node.left != Val::Px(logical.x) || root_node.top != Val::Px(logical.y) {
        root_node.left = Val::Px(logical.x);
        root_node.top = Val::Px(logical.y);
    }

    let hovered = hover
        .as_deref()
        .and_then(|map| map.0.get(&AGENT_POINTER))
        .is_some_and(|hits| !hits.is_empty());
    let color = if mouse.pressed(MouseButton::Left) {
        CURSOR_PRESSED
    } else if hovered {
        CURSOR_HOVER
    } else {
        CURSOR_IDLE
    };
    for mut background in &mut bars {
        if background.0 != color {
            background.0 = color;
        }
    }
}

/// The [`ui_dump_method`] payload. See that function's doc comment for the semantics.
fn ui_dump_snapshot(world: &mut World) -> serde_json::Value {
    let offscreen = world
        .get_resource::<OffscreenRenderTarget>()
        .map(|target| target.0.clone());

    // Headless: only nodes whose UI camera renders into the capture target. The vec is empty
    // (== unfiltered) in windowed mode, where every camera's node is fair game.
    let target_cameras: Vec<Entity> = match &offscreen {
        Some(handle) => {
            let mut cameras = world.query::<(Entity, &RenderTarget)>();
            cameras
                .iter(world)
                .filter(|(_, target)| {
                    target
                        .as_image()
                        .is_some_and(|image| image.id() == handle.id())
                })
                .map(|(entity, _)| entity)
                .collect()
        }
        None => Vec::new(),
    };

    let target_size = offscreen.as_ref().and_then(|handle| {
        world
            .get_resource::<Assets<Image>>()
            .and_then(|images| images.get(handle))
            .map(|image| image.size())
    });

    let pointer = pointer_target(world).map(|target| current_pointer_location(world, &target));
    let hovered_entities: Vec<Entity> = world
        .get_resource::<bevy::picking::hover::HoverMap>()
        .and_then(|map| map.0.get(&AGENT_POINTER))
        .map(|hits| hits.keys().copied().collect())
        .unwrap_or_default();

    // UiStack order IS render order (back-to-front) — the dump inherits it, so the model can
    // reason about occlusion ("a later row draws on top of an earlier one").
    let stack = world.resource::<UiStack>().uinodes.clone();

    let mut nodes = world.query_filtered::<(
        &ComputedNode,
        &UiGlobalTransform,
        &ComputedUiTargetCamera,
        Option<&InheritedVisibility>,
        Option<&bevy::ui::Interaction>,
        Option<&PickHovered>,
    ), (
        Without<AgentCursorRoot>,
        Without<AgentCursorBar>,
    )>();
    let mut texts = world.query::<&Text>();
    let mut spans = world.query::<&TextSpan>();
    let mut children = world.query::<&Children>();
    let mut parents = world.query::<&ChildOf>();

    // First pass: raw rows in stack order. A row is interactive iff it carries a `bevy_ui`
    // `Interaction` — the one interaction convention every bevy UI surface uses (the focus
    // system only updates nodes that have it). Pressed/hovered come from `Interaction` itself,
    // hovered also from picking's own `Hovered` component.
    struct Row {
        entity: Entity,
        rect: [i32; 4],
        clickable: bool,
        interaction: Option<String>,
        text: Option<String>,
    }
    let mut rows = Vec::new();
    for &entity in &stack {
        let Ok((node, transform, target, visibility, interaction, hovered)) =
            nodes.get(world, entity)
        else {
            continue;
        };
        let size = node.size();
        if size == Vec2::ZERO {
            continue;
        }
        if visibility.is_some_and(|visibility| !visibility.get()) {
            continue;
        }
        if !target_cameras.is_empty()
            && !target
                .get()
                .is_some_and(|camera| target_cameras.contains(&camera))
        {
            continue;
        }
        // An axis-aligned rect of a rotated node would be a lie.
        let (_, angle, translation) = transform.to_scale_angle_translation();
        if angle.abs() > 0.01 {
            continue;
        }
        let clickable = interaction.is_some();
        let interaction = if matches!(interaction, Some(bevy::ui::Interaction::Pressed)) {
            Some("Pressed".to_owned())
        } else if matches!(interaction, Some(bevy::ui::Interaction::Hovered))
            || hovered.is_some_and(|hovered| hovered.0)
        {
            Some("Hovered".to_owned())
        } else if clickable {
            Some("Idle".to_owned())
        } else {
            None
        };
        // Interactive rows aggregate their subtree text (that's the button's label); plain
        // nodes show only their own text (a `Text` block with its `TextSpan` runs) — otherwise
        // the root panel row would repeat every label on screen.
        let text = if clickable {
            subtree_text(entity, &*world, &mut texts, &mut spans, &mut children)
        } else {
            direct_text(entity, &*world, &mut texts, &mut spans, &mut children)
        };
        rows.push(Row {
            entity,
            rect: [
                (translation.x - size.x / 2.0).round() as i32,
                (translation.y - size.y / 2.0).round() as i32,
                size.x.round() as i32,
                size.y.round() as i32,
            ],
            clickable,
            interaction,
            text,
        });
    }

    // Second pass: fold a non-interactive row away if it sits under a dumped interactive
    // ancestor (the button-label case — its text is already in the button's row).
    let clickable: HashSet<Entity> = rows
        .iter()
        .filter(|row| row.clickable)
        .map(|row| row.entity)
        .collect();
    let mut covered_by_ancestor = |entity: Entity| -> bool {
        let mut current = entity;
        for _ in 0..16 {
            let Ok(parent) = parents.get(world, current) else {
                return false;
            };
            current = parent.0;
            if clickable.contains(&current) {
                return true;
            }
        }
        false
    };
    rows.retain(|row| row.clickable || !covered_by_ancestor(row.entity));

    let nodes_json: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let mut entry = serde_json::Map::new();
            entry.insert("entity".into(), json!(row.entity));
            entry.insert("rect".into(), json!(row.rect));
            if row.clickable {
                entry.insert("clickable".into(), json!(true));
            }
            if let Some(text) = &row.text {
                entry.insert("text".into(), json!(text));
            }
            if let Some(interaction) = &row.interaction {
                entry.insert("interaction".into(), json!(interaction));
            }
            if hovered_entities.contains(&row.entity) {
                entry.insert("pointer_hovered".into(), json!(true));
            }
            serde_json::Value::Object(entry)
        })
        .collect();

    json!({
        "note": "Nodes in back-to-front render order. rect = [x, y, w, h] in screenshot pixel space — the same coordinates game/mouse move_to consumes. `clickable: true` = a real interactive node (safe to click). `text` is the node's own text (buttons: their whole label). `interaction`: Pressed | Hovered | Idle. `pointer_hovered`: the mocked pointer is over this node right now.",
        "target_size": target_size.map(|size| json!([size.x, size.y])),
        "pointer": pointer.map(|location| {
            json!({
                "x": location.position.x.round() as i32,
                "y": location.position.y.round() as i32,
            })
        }),
        "hovered_entities": hovered_entities,
        "nodes": nodes_json,
    })
}

/// The text carried by `entity`'s whole subtree: each text block ([`direct_text`]) in child
/// order, joined with spaces. `None` when the subtree carries no non-empty text.
fn subtree_text(
    entity: Entity,
    world: &World,
    texts: &mut QueryState<&Text>,
    spans: &mut QueryState<&TextSpan>,
    children: &mut QueryState<&Children>,
) -> Option<String> {
    if texts.get(world, entity).is_ok() {
        return direct_text(entity, world, texts, spans, children);
    }
    let mut parts: Vec<String> = Vec::new();
    if let Ok(kids) = children.get(world, entity) {
        for kid in kids.iter() {
            if let Some(more) = subtree_text(kid, world, texts, spans, children) {
                parts.push(more);
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The text of `entity`'s own text block: its `Text` followed by its `TextSpan` runs, runs
/// concatenated as rendered. No descent into other nodes, so a container doesn't repeat every
/// descendant label (buttons aggregate via [`subtree_text`] instead).
fn direct_text(
    entity: Entity,
    world: &World,
    texts: &mut QueryState<&Text>,
    spans: &mut QueryState<&TextSpan>,
    children: &mut QueryState<&Children>,
) -> Option<String> {
    fn span_runs(
        entity: Entity,
        world: &World,
        spans: &mut QueryState<&TextSpan>,
        children: &mut QueryState<&Children>,
        out: &mut String,
    ) {
        let Ok(kids) = children.get(world, entity) else {
            return;
        };
        for kid in kids.iter() {
            if let Ok(span) = spans.get(world, kid) {
                out.push_str(span);
                span_runs(kid, world, spans, children, out);
            }
        }
    }
    let text = texts.get(world, entity).ok()?;
    let mut out = text.0.clone();
    span_runs(entity, world, spans, children, &mut out);
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    (!out.is_empty()).then_some(out)
}

// ---------------------------------------------------------------------------
// Device-level input mocks
// ---------------------------------------------------------------------------

/// Marks the one synthetic gamepad entity `gamepad_method` mocks input on — spawned lazily on
/// first use, not at `Startup`, so a session that never touches `game/gamepad` never pays for
/// it. Distinct from [`Gamepad`] itself only so this entity is unambiguously identifiable
/// (`world.query`-able) as agent-injected rather than a real, `bevy_gilrs`-detected controller —
/// nothing reads this marker at runtime.
#[derive(Component)]
struct AgentVirtualGamepad;

/// `game/gamepad` — mocks a real gamepad's button/axis state directly (`bevy_input::gamepad::
/// Gamepad`'s `analog` field — see the note in the `"button"` match arm below for why buttons
/// go through this too, not the seemingly-obvious `digital`/`ButtonInput` field — on a
/// synthetic, agent-owned gamepad entity). The load-bearing property: the injected state flows
/// through an input crate's *real* binding resolution (dead zones, `Scale` modifiers,
/// shared-device ownership, per-context device selection) exactly like a human's controller
/// does, rather than skipping straight to "this action fired with this value." Confirmed by
/// testing against `bevy_enhanced_input-0.26.0/src/context.rs`'s `GamepadDevice`: contexts that
/// don't explicitly set a `GamepadDevice` component default to `GamepadDevice::Any` ("input
/// will be read from all connected gamepads"), so a bare `Gamepad` component on any entity,
/// real controller or not, is read identically. This is what makes UI navigation actually
/// testable: the exact same button/stick state a human's controller would report drives the
/// exact same bindings and actions.
///
/// Gamepad button/stick state is level-triggered on a real controller (held until physically
/// released) — so this mirrors that instead of introducing an auto-expiry mechanism: every
/// `press`/`release`/`set_axis` call is a direct, persistent state change the caller is
/// responsible for undoing (release what you press). `{"input": "reset"}` clears all button/axis
/// state in one call — cheap insurance against a forgotten release leaving an input stuck for
/// the rest of the session; reach for it between unrelated test scenarios rather than trying to
/// track exactly what's still held.
///
/// Params:
/// - `{"input": "button", "button": "South", "pressed": true}` — press or release one of the 19
///   standard `GamepadButton` variants (see [`parse_gamepad_button`] for the exact names).
/// - `{"input": "axis", "axis": "LeftStickX", "value": 0.8}` — set one of the 6 standard
///   `GamepadAxis` variants to a value in roughly −1.0..1.0 (see [`parse_gamepad_axis`]).
/// - `{"input": "reset"}` — release every button and zero every axis on the virtual gamepad.
pub(crate) fn gamepad_method(params: In<Option<serde_json::Value>>, world: &mut World) -> BrpResult {
    let Some(params) = params.0 else {
        return Err(BrpError::internal("missing params"));
    };
    let input = params
        .get("input")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| BrpError::internal("missing params.input"))?;

    // Lazily find-or-spawn the one virtual gamepad entity. Not `Startup`-spawned: see
    // `AgentVirtualGamepad`'s doc comment.
    let gamepad_entity = {
        let mut query = world.query_filtered::<Entity, With<AgentVirtualGamepad>>();
        match query.single(world) {
            Ok(entity) => entity,
            Err(_) => world.spawn((Gamepad::default(), AgentVirtualGamepad)).id(),
        }
    };
    let mut gamepad = world
        .get_mut::<Gamepad>(gamepad_entity)
        .ok_or_else(|| BrpError::internal("virtual gamepad entity has no Gamepad component"))?;

    match input {
        "button" => {
            let button_name = params
                .get("button")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| BrpError::internal("missing params.button"))?;
            let button = parse_gamepad_button(button_name)
                .ok_or_else(|| BrpError::internal(&format!("unknown button {button_name:?}")))?;
            let pressed = params
                .get("pressed")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true);
            // `bevy_enhanced_input`'s `Binding::GamepadButton` reader reads `Gamepad::get`,
            // i.e. the `analog` map — NOT `digital`/`ButtonInput` — confirmed by testing (a
            // first attempt using `digital_mut().press()` compiled fine, but a
            // gamepad-Start-opens-pause-menu test never opened the menu at all) and by reading
            // `bevy_enhanced_input-0.26.0/src/context/input_reader.rs`'s
            // `Binding::GamepadButton` arm directly. `1.0`/`0.0` here is what a real button
            // reports through this same path — Bevy's own button-axis convention.
            gamepad.analog_mut().set(button, if pressed { 1.0 } else { 0.0 });
            Ok(json!({"gamepad_entity": gamepad_entity, "button": button_name, "pressed": pressed}).into())
        }
        "axis" => {
            let axis_name = params
                .get("axis")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| BrpError::internal("missing params.axis"))?;
            let axis = parse_gamepad_axis(axis_name)
                .ok_or_else(|| BrpError::internal(&format!("unknown axis {axis_name:?}")))?;
            let value = params
                .get("value")
                .and_then(serde_json::Value::as_f64)
                .ok_or_else(|| BrpError::internal("missing params.value"))? as f32;
            gamepad.analog_mut().set(axis, value);
            Ok(json!({"gamepad_entity": gamepad_entity, "axis": axis_name, "value": value}).into())
        }
        "reset" => {
            *gamepad = Gamepad::default();
            Ok(json!({"gamepad_entity": gamepad_entity, "reset": true}).into())
        }
        other => Err(BrpError::internal(&format!(
            "unknown input {other:?} (expected button|axis|reset)"
        ))),
    }
}

fn parse_gamepad_button(name: &str) -> Option<GamepadButton> {
    Some(match name {
        "South" => GamepadButton::South,
        "East" => GamepadButton::East,
        "North" => GamepadButton::North,
        "West" => GamepadButton::West,
        "C" => GamepadButton::C,
        "Z" => GamepadButton::Z,
        "LeftTrigger" => GamepadButton::LeftTrigger,
        "LeftTrigger2" => GamepadButton::LeftTrigger2,
        "RightTrigger" => GamepadButton::RightTrigger,
        "RightTrigger2" => GamepadButton::RightTrigger2,
        "Select" => GamepadButton::Select,
        "Start" => GamepadButton::Start,
        "Mode" => GamepadButton::Mode,
        "LeftThumb" => GamepadButton::LeftThumb,
        "RightThumb" => GamepadButton::RightThumb,
        "DPadUp" => GamepadButton::DPadUp,
        "DPadDown" => GamepadButton::DPadDown,
        "DPadLeft" => GamepadButton::DPadLeft,
        "DPadRight" => GamepadButton::DPadRight,
        _ => return None,
    })
}

fn parse_gamepad_axis(name: &str) -> Option<GamepadAxis> {
    Some(match name {
        "LeftStickX" => GamepadAxis::LeftStickX,
        "LeftStickY" => GamepadAxis::LeftStickY,
        "LeftZ" => GamepadAxis::LeftZ,
        "RightStickX" => GamepadAxis::RightStickX,
        "RightStickY" => GamepadAxis::RightStickY,
        "RightZ" => GamepadAxis::RightZ,
        _ => return None,
    })
}

/// `game/keyboard` — mocks real keyboard key state directly on `ButtonInput<KeyCode>`, the
/// same resource input crates' keyboard bindings read (`keys.pressed(key)` — a plain resource,
/// no per-device entity needed the way `Gamepad` is, so unlike `game/gamepad` there's nothing
/// to lazily spawn here). Level-triggered like a real key: held until released.
///
/// Params:
/// - `{"key": "KeyW", "pressed": true}` — press or release a `KeyCode` by its exact Rust variant
///   name, deserialized directly via `KeyCode`'s own `serde` impl (requires bevy's `serialize`
///   feature, enabled by this crate) rather than a hand-maintained name list — every one of
///   Bevy's 160+ variants works, not just a hand-picked subset (e.g. `KeyA`..`KeyZ`,
///   `Digit0`..`Digit9`, `Escape`, `Space`, `Enter`, `Tab`, `ArrowUp`/`Down`/`Left`/`Right`,
///   `ShiftLeft`/`Right`, `ControlLeft`/`Right`, `AltLeft`/`Right`).
/// - `{"reset": true}` — release every currently-pressed key.
pub(crate) fn keyboard_method(params: In<Option<serde_json::Value>>, world: &mut World) -> BrpResult {
    let Some(params) = params.0 else {
        return Err(BrpError::internal("missing params"));
    };
    let mut keys = world.resource_mut::<ButtonInput<KeyCode>>();
    if params.get("reset").and_then(serde_json::Value::as_bool) == Some(true) {
        keys.release_all();
        return Ok(json!({"reset": true}).into());
    }
    let key_name = params
        .get("key")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| BrpError::internal("missing params.key"))?;
    let key: KeyCode = serde_json::from_value(json!(key_name))
        .map_err(|err| BrpError::internal(&format!("unknown key {key_name:?}: {err}")))?;
    let pressed = params
        .get("pressed")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    if pressed {
        keys.press(key);
    } else {
        keys.release(key);
    }
    Ok(json!({"key": key_name, "pressed": pressed}).into())
}

/// The pointer this whole method drives. **Not a synthetic entity we spawn** — unlike
/// `game/gamepad`'s virtual `Gamepad`, `bevy_picking`'s own `spawn_mouse_pointer` (part of its
/// default plugin set) unconditionally spawns a `PointerId::Mouse` entity at `Startup`
/// regardless of whether a window exists — headless mode already has a real mouse pointer
/// entity, it just never receives real `PointerInput` events (those come from `WindowEvent`s
/// this mode never gets, since there's no window). `PointerId::Custom(Uuid)` exists in
/// `bevy_picking` specifically "for mocking inputs", but reusing the real `PointerId::Mouse` is
/// simpler (no new `uuid` dependency, no lazy-spawn bookkeeping) and arguably more faithful —
/// it *is* the mouse, not a stand-in for it.
const AGENT_POINTER: bevy::picking::pointer::PointerId = bevy::picking::pointer::PointerId::Mouse;

/// The render target the mocked pointer acts on: the offscreen capture target when one exists,
/// otherwise the primary window (when there is one). `None` in a window-less app with no
/// offscreen target — there is nowhere to point.
fn pointer_target(world: &mut World) -> Option<bevy::camera::NormalizedRenderTarget> {
    if let Some(target) = world.get_resource::<OffscreenRenderTarget>() {
        return Some(bevy::camera::NormalizedRenderTarget::Image(
            target.0.clone().into(),
        ));
    }
    let mut windows = world.query_filtered::<Entity, With<bevy::window::PrimaryWindow>>();
    windows
        .single(world)
        .ok()
        .and_then(|entity| {
            bevy::window::WindowRef::Primary.normalize(Some(entity))
        })
        .map(bevy::camera::NormalizedRenderTarget::Window)
}

/// The center of `target` in its own pixel space — the pointer's parking position before
/// anything has moved it. Derived from the actual target rather than a baked constant, so the
/// size and the fallback can't drift apart.
fn target_center(world: &mut World, target: &bevy::camera::NormalizedRenderTarget) -> Vec2 {
    const FALLBACK: Vec2 = Vec2::new(640.0, 400.0);
    match target {
        bevy::camera::NormalizedRenderTarget::Image(handle) => world
            .get_resource::<Assets<Image>>()
            .and_then(|images| images.get(&handle.handle))
            .map(|image| image.size().as_vec2() / 2.0)
            .unwrap_or(FALLBACK),
        bevy::camera::NormalizedRenderTarget::Window(_) => {
            let mut windows =
                world.query_filtered::<&Window, With<bevy::window::PrimaryWindow>>();
            windows
                .single(world)
                .ok()
                .map(|window| window.resolution.physical_size().as_vec2() / 2.0)
                .unwrap_or(FALLBACK)
        }
        _ => FALLBACK,
    }
}

/// The pointer's current position, read back from its own `PointerLocation` component so a
/// relative `"motion"` move (below) computes the right new absolute position — falls back to
/// the target's center if nothing has moved it yet this session.
fn current_pointer_location(
    world: &mut World,
    target: &bevy::camera::NormalizedRenderTarget,
) -> bevy::picking::pointer::Location {
    use bevy::picking::pointer::{Location, PointerLocation};
    let mut query = world.query::<(&bevy::picking::pointer::PointerId, &PointerLocation)>();
    query
        .iter(world)
        .find(|(id, _)| **id == AGENT_POINTER)
        .and_then(|(_, loc)| loc.location.clone())
        .unwrap_or(Location {
            target: target.clone(),
            position: target_center(world, target),
        })
}

/// `game/mouse` — mocks real mouse button/motion/wheel/cursor-position state. Buttons and
/// motion/wheel deltas go through the same plain resources `game/keyboard`'s doc comment
/// describes for keyboard (`ButtonInput<MouseButton>`, `AccumulatedMouseMotion`,
/// `AccumulatedMouseScroll` — all three read directly by input crates' `MouseButton`/
/// `MouseMotion`/`MouseWheel` bindings). Cursor position and clicks go through `bevy_picking`'s
/// real event pipeline instead (`PointerInput`/`PointerAction` — see [`AGENT_POINTER`]'s doc
/// comment) since UI hover/click hit-testing needs a *position*, which the button/motion
/// resources above don't carry — this is the mechanism that makes clicking an actual UI button
/// (as opposed to just a raw mouse-bound gameplay action) possible at all through this tool.
///
/// A real mouse click drives both mechanisms simultaneously in real life (`ButtonInput` for
/// direct mouse-bound gameplay bindings, `PointerInput` for UI hit-testing), so `"button"`
/// below updates both at once for `Left`/`Right`/`Middle` (mapped to `PointerButton`'s
/// `Primary`/`Secondary`/`Middle` — `Back`/`Forward`/`Other` update `ButtonInput` only, since
/// `PointerButton` has no equivalent).
///
/// Params:
/// - `{"input":"button","button":"Left","pressed":true}` — `Left`/`Right`/`Middle`/`Back`/
///   `Forward` (matches `MouseButton`). Level-triggered: held until an explicit
///   `pressed:false` call.
/// - `{"input":"motion","dx":10,"dy":-5}` — a *relative* delta, matching real mouse-motion
///   events: sets this tick's `AccumulatedMouseMotion` (for camera-look-style bindings) and
///   also moves the tracked cursor position by the same delta (for hover), mirroring how a
///   single physical mouse movement feeds both systems in reality regardless of which one a
///   given game state is actually listening to.
/// - `{"input":"move_to","x":640,"y":400}` — sets the cursor to an *absolute* position in the
///   same pixel space `game/screenshot` captures, for UI hover/click testing when you already
///   know where something is from a screenshot or a `game/ui` dump. Doesn't touch
///   `AccumulatedMouseMotion` — this is a convenience teleport, not a simulated drag.
/// - `{"input":"wheel","x":0,"y":1,"unit":"Line"}` — `unit` is `Line` (default) or `Pixel`,
///   matching `MouseScrollUnit`.
/// - `{"input":"reset"}` — releases every mouse button (both mechanisms) and zeros the motion/
///   wheel accumulators. Doesn't move the cursor back to center.
pub(crate) fn mouse_method(params: In<Option<serde_json::Value>>, world: &mut World) -> BrpResult {
    use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
    use bevy::input::touch::TouchPhase;
    use bevy::picking::pointer::{PointerAction, PointerButton, PointerInput};

    let Some(params) = params.0 else {
        return Err(BrpError::internal("missing params"));
    };
    let input = params
        .get("input")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| BrpError::internal("missing params.input"))?;
    let target = pointer_target(world);

    match input {
        "button" => {
            let button_name = params
                .get("button")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| BrpError::internal("missing params.button"))?;
            let button = parse_mouse_button(button_name)
                .ok_or_else(|| BrpError::internal(&format!("unknown button {button_name:?}")))?;
            let pressed = params
                .get("pressed")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true);
            {
                let mut buttons = world.resource_mut::<ButtonInput<MouseButton>>();
                if pressed {
                    buttons.press(button);
                } else {
                    buttons.release(button);
                }
            }
            if let (Some(pointer_button), Some(target)) =
                (mouse_button_to_pointer_button(button), target)
            {
                let location = current_pointer_location(world, &target);
                let action = if pressed {
                    PointerAction::Press(pointer_button)
                } else {
                    PointerAction::Release(pointer_button)
                };
                world.write_message(PointerInput::new(AGENT_POINTER, location, action));
            }
            Ok(json!({"button": button_name, "pressed": pressed}).into())
        }
        "motion" => {
            let dx = params.get("dx").and_then(serde_json::Value::as_f64).unwrap_or(0.0) as f32;
            let dy = params.get("dy").and_then(serde_json::Value::as_f64).unwrap_or(0.0) as f32;
            let delta = Vec2::new(dx, dy);
            // A direct `insert_resource(AccumulatedMouseMotion{..})` doesn't stick: Bevy's own
            // `accumulate_mouse_motion_system` (bevy_input-0.19.0/src/mouse.rs:259-268)
            // unconditionally overwrites this resource from `MouseMotion` events every frame
            // ("reset to zero every frame", per its own doc comment) — it ran before our BRP
            // write reached the world in one live test, wiping the value before an input
            // crate's reader ever saw it (confirmed: `look_yaw` stayed exactly 0.0 after a
            // `dx: 200` call that should have turned the camera). Writing a real `MouseMotion`
            // event instead lets that system pick it up in its own scheduled slot, whichever
            // frame that lands on — the same mechanism a real winit mouse-delta uses.
            world.write_message(bevy::input::mouse::MouseMotion { delta });
            if let Some(target) = target {
                let mut location = current_pointer_location(world, &target);
                location.position += delta;
                world.write_message(PointerInput::new(
                    AGENT_POINTER,
                    location,
                    PointerAction::Move { delta },
                ));
            }
            Ok(json!({"dx": dx, "dy": dy}).into())
        }
        "move_to" => {
            let Some(target) = target else {
                return Err(BrpError::internal(
                    "no pointer target (no offscreen target configured and no primary window)",
                ));
            };
            let x = params.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0) as f32;
            let y = params.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0) as f32;
            let previous = current_pointer_location(world, &target);
            let position = Vec2::new(x, y);
            let location = bevy::picking::pointer::Location {
                target: previous.target,
                position,
            };
            world.write_message(PointerInput::new(
                AGENT_POINTER,
                location,
                PointerAction::Move {
                    delta: position - previous.position,
                },
            ));
            Ok(json!({"x": x, "y": y}).into())
        }
        "wheel" => {
            let x = params.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0) as f32;
            let y = params.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0) as f32;
            let unit_name = params
                .get("unit")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Line");
            let unit = match unit_name {
                "Line" => MouseScrollUnit::Line,
                "Pixel" => MouseScrollUnit::Pixel,
                other => {
                    return Err(BrpError::internal(&format!(
                        "unknown scroll unit {other:?} (expected Line|Pixel)"
                    )));
                }
            };
            // Same reasoning as `"motion"` above: a real `MouseWheel` event, not a direct
            // resource write, so `accumulate_mouse_scroll_system` computes
            // `AccumulatedMouseScroll` on its own terms. `window: Entity::PLACEHOLDER` is safe
            // here — that system never reads the field, it only exists for consumers that care
            // which window received the scroll.
            world.write_message(bevy::input::mouse::MouseWheel {
                unit,
                x,
                y,
                window: Entity::PLACEHOLDER,
                phase: TouchPhase::Moved,
            });
            if let Some(target) = target {
                let location = current_pointer_location(world, &target);
                world.write_message(PointerInput::new(
                    AGENT_POINTER,
                    location,
                    PointerAction::Scroll {
                        unit,
                        x,
                        y,
                        phase: TouchPhase::Moved,
                    },
                ));
            }
            Ok(json!({"x": x, "y": y, "unit": unit_name}).into())
        }
        "reset" => {
            world.resource_mut::<ButtonInput<MouseButton>>().release_all();
            if let Some(target) = target {
                let location = current_pointer_location(world, &target);
                for button in [
                    PointerButton::Primary,
                    PointerButton::Secondary,
                    PointerButton::Middle,
                ] {
                    world.write_message(PointerInput::new(
                        AGENT_POINTER,
                        location.clone(),
                        PointerAction::Release(button),
                    ));
                }
            }
            world.insert_resource(AccumulatedMouseMotion { delta: Vec2::ZERO });
            world.insert_resource(AccumulatedMouseScroll {
                unit: MouseScrollUnit::Line,
                delta: Vec2::ZERO,
            });
            Ok(json!({"reset": true}).into())
        }
        other => Err(BrpError::internal(&format!(
            "unknown input {other:?} (expected button|motion|move_to|wheel|reset)"
        ))),
    }
}

fn parse_mouse_button(name: &str) -> Option<MouseButton> {
    Some(match name {
        "Left" => MouseButton::Left,
        "Right" => MouseButton::Right,
        "Middle" => MouseButton::Middle,
        "Back" => MouseButton::Back,
        "Forward" => MouseButton::Forward,
        _ => return None,
    })
}

fn mouse_button_to_pointer_button(
    button: MouseButton,
) -> Option<bevy::picking::pointer::PointerButton> {
    match button {
        MouseButton::Left => Some(bevy::picking::pointer::PointerButton::Primary),
        MouseButton::Right => Some(bevy::picking::pointer::PointerButton::Secondary),
        MouseButton::Middle => Some(bevy::picking::pointer::PointerButton::Middle),
        MouseButton::Back | MouseButton::Forward | MouseButton::Other(_) => None,
    }
}
