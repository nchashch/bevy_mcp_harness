//! The custom BRP methods — the game tools. Handlers run in the main world with `&mut World`
//! access (registered as plain systems into `bevy::remote::RemoteMethods`); the MCP layer
//! proxies to them over loopback HTTP. Ported from PROTOTYPE_19's `dev/tool_api.rs`, minus its
//! game-specific methods (`game/input` action mocks, `game/select`, `game/trigger`,
//! `game/levels`, `game/select_level` — all bound to that prototype's own crates).

use std::collections::{HashMap, HashSet};
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

use crate::headless::CaptureTarget;
use crate::{McpHarnessConfig, NoRenderMode, PreconditionFn};

/// The nodes payload of the last `game/ui` dump served in full. If the next read's filtered
/// node list hashes identically, the response omits `nodes` (`unchanged: true`) — agents
/// re-read the UI after every input mostly to check "did anything change", and the full dump
/// is the most expensive repeated read. Hashing the *filtered* node list means a changed
/// filter is a changed hash (full dump).
#[derive(Resource, Default)]
pub(crate) struct LastUiDump {
    hash: Option<u64>,
}

/// `game/ui` read filters.
#[derive(Default)]
struct UiFilter {
    clickable_only: bool,
    text_contains: Option<String>,
}

impl UiFilter {
    fn from_params(params: Option<&serde_json::Value>) -> Self {
        let get = |key: &str| {
            params
                .and_then(|p| p.get(key))
                .and_then(serde_json::Value::as_str)
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        };
        Self {
            clickable_only: params
                .and_then(|p| p.get("clickable_only"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            text_contains: get("text_contains").map(|s| s.to_lowercase()),
        }
    }

    fn keeps(&self, row_text: Option<&str>, clickable: bool) -> bool {
        if self.clickable_only && !clickable {
            return false;
        }
        match &self.text_contains {
            Some(needle) => row_text.is_some_and(|text| text.to_lowercase().contains(needle)),
            None => true,
        }
    }
}

fn hash_value(value: &serde_json::Value) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&serde_json::to_string(value).unwrap_or_default(), &mut hasher);
    std::hash::Hasher::finish(&hasher)
}

/// Declared preconditions by full BRP method name (`{prefix}/name`), populated by
/// [`crate::register_game_method_with_precondition`] and the harness's own built-ins, read by
/// the `{prefix}/plan_check` pre-flight method.
#[derive(Resource, Default)]
pub(crate) struct GamePreconditions(pub HashMap<String, PreconditionFn>);

/// The built-in `screenshot` precondition: rendering must be enabled (no `NoRenderMode`) and
/// a capture target must exist (offscreen texture, or a primary window in windowed sessions).
pub(crate) fn screenshot_precondition(
    world: &World,
    _params: Option<&serde_json::Value>,
) -> Result<(), String> {
    if world.get_resource::<NoRenderMode>().is_some() {
        return Err(
            "no rendering enabled (no_render): screenshots are unavailable; use game/ui for \
             on-screen content and game/state for ground truth"
                .to_owned(),
        );
    }
    let has_window = world
        .iter_entities()
        .any(|entity| entity.get::<bevy::window::PrimaryWindow>().is_some());
    if world.get_resource::<CaptureTarget>().is_none() && !has_window {
        return Err(
            "no capture target (no offscreen target configured and no primary window)".to_owned(),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// game/state
// ---------------------------------------------------------------------------

/// `game/state` — the host-registered snapshot (see [`crate::StateSnapshotFn`]). An empty
/// object when the host registered no hook. Params are ignored.
pub(crate) fn game_state_method(_params: In<Option<serde_json::Value>>, world: &mut World) -> BrpResult {
    Ok(game_state_snapshot(world))
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
    let mut payload = json!({
        "no_render": config.no_render,
        "brp_port": config.brp_port,
        "mcp_port": config.mcp_port,
        "screenshots_dir": config.screenshots_dir.display().to_string(),
        "screenshots_available": !config.no_render,
        "rendering": !config.no_render,
        "target_size": world
            .get_resource::<CaptureTarget>()
            .and_then(|target| {
                world
                    .get_resource::<Assets<Image>>()
                    .and_then(|images| images.get(&target.0))
                    .map(|image| vec![image.size().x, image.size().y])
            }),
    });
    // The host's game-specific mode flags (vr, headless_render, …), merged under one key so
    // they can't collide with the generic fields.
    if let Some(host) = config.client_info_host.clone() {
        payload["host"] = host(world);
    }
    Ok(payload)
}

/// `{prefix}/plan_check` — pre-flight check for a sequence of intended calls. Params:
/// `{"calls": ["{prefix}/trigger", {"method": "{prefix}/input", "params": {...}}, …]}` — each
/// entry is a bare method name or an object with `method` + optional `params`. For every call
/// the response reports `ok: true`, or `ok: false` with the `reason` the call would fail with
/// (unknown method; declared precondition unmet). Methods without a declared precondition
/// report `ok: true` with `"precondition": "none"` — `plan_check` can only vouch for what the
/// host declared; the method's own checks remain the source of truth.
pub(crate) fn plan_check_method(
    params: In<Option<serde_json::Value>>,
    world: &mut World,
) -> BrpResult {
    use bevy::remote::RemoteMethods;

    let calls = params
        .0
        .as_ref()
        .and_then(|p| p.get("calls"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| BrpError::internal("missing params.calls (array of method names or {method, params} objects)"))?;

    let methods = world.resource::<RemoteMethods>();
    let preconditions = world.resource::<GamePreconditions>();

    let mut results = Vec::new();
    let mut all_ok = true;
    for call in calls {
        let (name, intended_params) = match call {
            serde_json::Value::String(name) => (name.clone(), None),
            obj @ serde_json::Value::Object(_) => (
                obj.get("method")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                obj.get("params").cloned(),
            ),
            other => {
                all_ok = false;
                results.push(json!({
                    "call": other, "ok": false,
                    "reason": "call entries must be strings or {method, params} objects",
                }));
                continue;
            }
        };

        let mut entry = json!({ "call": name });
        if methods.get(&name).is_none() {
            all_ok = false;
            entry["ok"] = json!(false);
            entry["reason"] = json!(format!(
                "unknown method (registered: {:?})",
                methods.methods()
            ));
        } else if let Some(precondition) =
            preconditions.0.get(&name).cloned()
        {
            match precondition(world, intended_params.as_ref()) {
                Ok(()) => {
                    entry["ok"] = json!(true);
                }
                Err(reason) => {
                    all_ok = false;
                    entry["ok"] = json!(false);
                    entry["reason"] = json!(reason);
                }
            }
        } else {
            entry["ok"] = json!(true);
            entry["precondition"] = json!("none");
        }
        results.push(entry);
    }

    Ok(json!({ "all_ok": all_ok, "calls": results }))
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
    }))
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
/// screenshots directory (encoded by `save_encoded_to_disk`, async); poll
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
    // Optional render-debug view (depth / normals / motion vectors / deferred buffers — the
    // bevy_dev_tools F1 overlay) for this capture only.
    #[cfg(feature = "render_debug")]
    let debug_mode = params
        .0
        .as_ref()
        .and_then(|p| p.get("debug_view"))
        .and_then(serde_json::Value::as_str)
        // `"wireframe"` is handled separately (global `WireframeConfig` toggle below) —
        // `parse_mode` would reject it as an unknown overlay mode.
        .filter(|name| *name != "wireframe" && *name != "physics")
        .map(render_debug::parse_mode)
        .transpose()?;
    #[cfg(not(feature = "render_debug"))]
    if params
        .0
        .as_ref()
        .and_then(|p| p.get("debug_view"))
        .is_some()
    {
        return Err(BrpError::internal(
            "debug_view requires the render_debug cargo feature (bevy_dev_tools render-debug \
             views)",
        ));
    }
    // Optional overview downscale: the encoded PNG fits within this many pixels on its long
    // edge (aspect preserved). The capture itself stays full-resolution; the crop (if any) is
    // applied first, then the resize — a cropped region keeps full effective resolution.
    let max_dimension = match params.0.as_ref().and_then(|p| p.get("max_dimension")) {
        Some(value) => {
            let requested = value.as_u64().ok_or_else(|| {
                BrpError::internal("max_dimension must be a positive pixel count")
            })?;
            Some(requested.clamp(64, 4096) as u32)
        }
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
                return Err(BrpError::internal(format!(
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
                return Err(BrpError::internal(format!(
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
        .get_resource::<CaptureTarget>()
        .map(|target| Screenshot(bevy::camera::RenderTarget::Image(target.0.clone().into())))
        .unwrap_or_else(Screenshot::primary_window);

    // `debug_view: "physics"` is a persistent toggle — Avian3D collider gizmos via
    // bevy_gizmos. The gizmos stay on for subsequent captures (a logical view, not a
    // one-shot visual overlay). Requires the `physics_debug` cargo feature.
    #[cfg(feature = "physics_debug")]
    if params
        .0
        .as_ref()
        .and_then(|p| p.get("debug_view"))
        .and_then(serde_json::Value::as_str)
        == Some("physics")
    {
        physics_debug::apply_physics_debug(world)?;
        world
            .spawn(capture_target)
            .observe(save_encoded_to_disk(path.clone(), crop, max_dimension))
            .observe(restore_camera_order);
        return Ok(json!({
            "status": "capturing",
            "poll": "game/screenshot/get",
            "path": path.display().to_string(),
            "crop": crop,
            "max_dimension": max_dimension,
            "debug_view": "physics",
            "note": "physics collider gizmos enabled — they stay on for subsequent captures",
        }));
    }

    // `debug_view: "wireframe"` is a separate mechanism (global `WireframeConfig` toggle in
    // bevy_pbr, not the bevy_dev_tools F1 overlay) — handled before the overlay modes.
    #[cfg(feature = "render_debug")]
    if params
        .0
        .as_ref()
        .and_then(|p| p.get("debug_view"))
        .and_then(serde_json::Value::as_str)
        == Some("wireframe")
    {
        let previous_config =
            world.get_resource::<bevy::pbr::wireframe::WireframeConfig>().cloned();
        world.insert_resource(bevy::pbr::wireframe::WireframeConfig {
            global: true,
            default_color: Color::srgb(0.0, 1.0, 0.5),
            ..Default::default()
        });
        world.insert_resource(render_debug::PendingWireframeCapture {
            path: path.clone(),
            crop,
            max_dimension,
            previous_config,
            frames_since_apply: 0,
        });
        return Ok(json!({
            "status": "capturing",
            "poll": "game/screenshot/get",
            "path": path.display().to_string(),
            "crop": crop,
            "max_dimension": max_dimension,
            "debug_view": "wireframe",
            "note": "wireframe applied deferred — the first game/screenshot/get poll may return ready:false for a few hundred ms while the wireframe pipeline compiles",
        }));
    }

    // A `debug_view` capture is deferred: the overlay and its prepass pipelines compile on
    // first use, so the runner applies the overlay and spawns the capture only after a few
    // warm-up frames (see the render_debug module docs). Without `debug_view`, spawn now.
    #[cfg(feature = "render_debug")]
    if let Some(mode) = debug_mode {
        let camera = render_debug::resolve_camera(world, capture_camera)?;
        let missing = render_debug::required_prepasses(&mode)
            .iter()
            .copied()
            .filter(|name| {
                !match *name {
                    "DepthPrepass" => world
                        .get::<bevy::core_pipeline::prepass::DepthPrepass>(camera)
                        .is_some(),
                    "NormalPrepass" => world
                        .get::<bevy::core_pipeline::prepass::NormalPrepass>(camera)
                        .is_some(),
                    "MotionVectorPrepass" => world
                        .get::<bevy::core_pipeline::prepass::MotionVectorPrepass>(camera)
                        .is_some(),
                    "DeferredPrepass" => world
                        .get::<bevy::core_pipeline::prepass::DeferredPrepass>(camera)
                        .is_some(),
                    _ => false,
                }
            })
            .collect();
        world.insert_resource(render_debug::PendingDebugView {
            camera,
            mode,
            previous_overlay: world
                .get::<bevy_dev_tools::render_debug::RenderDebugOverlay>(camera)
                .cloned(),
            missing_prepasses: missing,
            capture: Some(render_debug::EncodedCapture {
                path: path.clone(),
                crop,
                max_dimension,
            }),
            frames_since_apply: 0,
            overlay_applied: false,
        });
        return Ok(json!({
            "status": "capturing",
            "poll": "game/screenshot/get",
            "path": path.display().to_string(),
            "crop": crop,
            "max_dimension": max_dimension,
            "debug_view": params
                .0
                .as_ref()
                .and_then(|p| p.get("debug_view"))
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            "note": "debug view applied deferred — the first game/screenshot/get poll may return ready:false for a few hundred ms while the debug pipelines warm up",
        }));
    }

    world
        .spawn(capture_target)
        .observe(save_encoded_to_disk(path.clone(), crop, max_dimension))
        .observe(restore_camera_order);
    Ok(json!({
        "status": "capturing",
        "poll": "game/screenshot/get",
        "path": path.display().to_string(),
        "crop": crop,
        "max_dimension": max_dimension,
    }))
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
/// order/clear config back. Runs after `save_encoded_to_disk` (which only reads the
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
/// `max_dimension` (when set) downscales the *encoded* result to fit within that many pixels
/// on the long edge (aspect preserved, Lanczos3) — overview reads cost fewer vision tokens;
/// the capture itself stays full-resolution.
fn save_encoded_to_disk(
    path: impl AsRef<Path>,
    crop: Option<[u32; 4]>,
    max_dimension: Option<u32>,
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
        let encoded = match max_dimension {
            Some(max) => {
                let long = cropped.width().max(cropped.height());
                (long > max).then(|| {
                    let scale = max as f64 / long as f64;
                    (
                        (cropped.width() as f64 * scale).round().max(1.0) as u32,
                        (cropped.height() as f64 * scale).round().max(1.0) as u32,
                    )
                })
            }
            None => None,
        }
        .map(|(w, h)| cropped.resize_exact(w, h, image::imageops::FilterType::Lanczos3))
        .unwrap_or(cropped);
        match encoded.save_with_format(&path, image::ImageFormat::Png) {
            Ok(_) => info!(
                "Screenshot saved to {} (crop: {crop:?}, max_dimension: {max_dimension:?}, {}×{})",
                path.display(),
                encoded.width(),
                encoded.height()
            ),
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
/// Computes screenspace projections for all visible `Aabb` entities, relative to the given
/// camera. Returns a list of `{entity, name, center, bounding_box, depth}` entries sorted by
/// depth (nearest first), suitable for embedding in a screenshot response or a standalone
/// `entities_on_screen` method.
///
/// The 2D bounding box is computed by projecting all 8 corners of the world-space AABB
/// through the camera and taking the min/max. Entities behind the camera or with
/// `InheritedVisibility = false` are excluded.
pub(crate) fn entities_on_screen_data(
    world: &mut World,
    camera_entity: Entity,
) -> BrpResult<Vec<serde_json::Value>> {
    let camera = world
        .get::<Camera>(camera_entity)
        .ok_or_else(|| BrpError::internal("camera entity has no Camera component"))?
        .clone();
    let camera_transform = *world
        .get::<GlobalTransform>(camera_entity)
        .ok_or_else(|| BrpError::internal("camera entity has no GlobalTransform"))?;

    let mut query = world.query_filtered::<(
        Entity,
        &bevy::camera::primitives::Aabb,
        &GlobalTransform,
        Option<&Name>,
        Option<&InheritedVisibility>,
        Option<&ViewVisibility>,
    ), ()>();

    let mut entries: Vec<(f32, serde_json::Value)> = Vec::new();
    for (entity, aabb, global_transform, name, inherited_vis, view_vis) in query.iter(world) {
        if inherited_vis.is_some_and(|vis| !vis.get()) {
            continue;
        }
        if view_vis.is_some_and(|vis| !vis.get()) {
            continue;
        }

        // Project the AABB center to screenspace. The `Aabb` is in the entity's LOCAL
        // space — transform it to world space via the entity's GlobalTransform first.
        let world_center = global_transform.transform_point(Vec3::from(aabb.center));
        let Ok(center_2d) =
            camera.world_to_viewport(&camera_transform, world_center)
        else {
            continue;
        };

        // Project all 8 AABB corners to compute the 2D bounding box. Each corner is
        // transformed from local space to world space via the entity's GlobalTransform
        // (so rotated entities get correctly-projected corners).
        let mut min_x = f32::MAX;
        let mut min_y = f32::MAX;
        let mut max_x = f32::MIN;
        let mut max_y = f32::MIN;
        let mut any_in_front = false;
        for sx in [0.0, 1.0] {
            for sy in [0.0, 1.0] {
                for sz in [0.0, 1.0] {
                    let local_corner = Vec3::new(
                        aabb.center.x + aabb.half_extents.x * (sx * 2.0 - 1.0),
                        aabb.center.y + aabb.half_extents.y * (sy * 2.0 - 1.0),
                        aabb.center.z + aabb.half_extents.z * (sz * 2.0 - 1.0),
                    );
                    let world_corner = global_transform.transform_point(local_corner);
                    if let Ok(pos) = camera.world_to_viewport(&camera_transform, world_corner) {
                        any_in_front = true;
                        min_x = min_x.min(pos.x);
                        min_y = min_y.min(pos.y);
                        max_x = max_x.max(pos.x);
                        max_y = max_y.max(pos.y);
                    }
                }
            }
        }
        if !any_in_front {
            continue;
        }

        // Depth: distance from camera to the AABB center.
        let distance = camera_transform
            .translation()
            .distance(Vec3::from(aabb.center));

        let mut entry = json!({
            "entity": entity,
            "center": [
                center_2d.x.round() as i32,
                center_2d.y.round() as i32,
            ],
            "bounding_box": [
                min_x.round() as i32,
                min_y.round() as i32,
                (max_x - min_x).round() as i32,
                (max_y - min_y).round() as i32,
            ],
            "depth": (distance * 100.0).round() / 100.0,
        });
        if let Some(name) = name {
            entry["name"] = json!(name.to_string());
        }
        entries.push((distance, entry));
    }

    entries.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(entries.into_iter().map(|(_, entry)| entry).collect())
}

/// `{prefix}/entities_on_screen` — projects all visible `Aabb` entities through the capture
/// camera into screenspace rects, sorted by depth (nearest first). Lets the agent correlate
/// pixels on a screenshot to entity ids without OCR.
pub(crate) fn entities_on_screen_method(params: In<Option<serde_json::Value>>, world: &mut World) -> BrpResult {
    let camera_entity = params
        .0
        .as_ref()
        .and_then(|p| p.get("camera"))
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .map(bevy::ecs::entity::Entity::from_bits);

    let camera_entity = match camera_entity {
        Some(entity) => entity,
        None => {
            // Default: the highest-order active camera (the one the agent "sees through").
            let mut best: Option<(Entity, isize)> = None;
            let mut query = world.query_filtered::<(Entity, &Camera), ()>();
            for (entity, camera) in query.iter(world) {
                if camera.is_active && best.is_none_or(|(_, order)| camera.order > order) {
                    best = Some((entity, camera.order));
                }
            }
            best.map(|(entity, _)| entity).ok_or_else(|| {
                BrpError::internal("no active camera found")
            })?
        }
    };

    let entities = entities_on_screen_data(world, camera_entity)?;
    Ok(json!({
        "camera_entity": camera_entity,
        "entities": entities,
    }))
}

/// Resolves the highest-order active Camera3d entity (the one rendering 3D content — UI
/// cameras have orthographic projections that can't project 3D world positions) and projects
/// all visible `Aabb` entities through it. Returns a JSON array (empty on error).
fn screenshot_get_entities(world: &mut World) -> serde_json::Value {
    let mut best: Option<(Entity, isize)> = None;
    let mut query = world.query_filtered::<(Entity, &Camera), With<Camera3d>>();
    for (entity, camera) in query.iter(world) {
        if camera.is_active && best.is_none_or(|(_, order)| camera.order > order) {
            best = Some((entity, camera.order));
        }
    }
    let Some(camera_entity) = best.map(|(entity, _)| entity) else {
        return json!([]);
    };
    match entities_on_screen_data(world, camera_entity) {
        Ok(entries) => json!(entries),
        Err(_) => json!([]),
    }
}

pub(crate) fn screenshot_get_method(_params: In<Option<serde_json::Value>>, world: &mut World) -> BrpResult {
    let Some(dir) = screenshots_dir(world) else {
        return Err(BrpError::internal(
            "McpHarnessConfig resource missing (was BevyMcpHarnessPlugin built?)",
        ));
    };
    let Some(path) = newest_screenshot(&dir) else {
        return Ok(json!({"ready": false}));
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(json!({"ready": false})),
    };
    let state = game_state_snapshot(world);
    let entities = screenshot_get_entities(world);

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
            "entities": entities,
        }));
    }
    if let Some(mut last) = world.get_resource_mut::<LastServedCapture>() {
        last.hash = Some(hash);
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
            "entities": entities,
        }))
    }
}

/// Render-debug screenshot support — the `debug_view` parameter on `game/screenshot`: the
/// bevy_dev_tools F1 overlay (depth / normals / motion vectors / deferred buffers) applied
/// to the capture camera for one capture, then restored. Requires the `render_debug` cargo
/// feature — `bevy_dev_tools` pulls the render-pipeline crates, so it is not part of the
/// minimal-bevy default graph.
///
/// Why deferred: the overlay and its prepass pipelines compile on first use — a capture
/// spawned in the same frame as the overlay insert races the compile and comes back as a
/// normal render (observed). So the request lands in [`PendingDebugView`], and a runner
/// system waits [`WARMUP_FRAMES`] before inserting the overlay and spawning the capture; by
/// then the pipelines are warm and the capture shows the debug view. The restore observer
/// runs on capture-complete (pipelines stay warm by then, so restoring immediately is safe).
#[cfg(feature = "render_debug")]
pub(crate) mod render_debug {
    use super::*;
    use bevy_dev_tools::render_debug::{RenderDebugMode, RenderDebugOverlay};

    /// Frames between applying the overlay and spawning the capture — enough for the overlay
    /// and prepass pipelines to compile and produce their first pass.
    pub(crate) const WARMUP_FRAMES: u32 = 5;

    /// Valid `debug_view` names → modes. `depth_pyramid` additionally needs
    /// `OcclusionCulling` on the camera to produce meaningful data.
    pub(crate) fn parse_mode(name: &str) -> Result<RenderDebugMode, BrpError> {
        Ok(match name {
            "depth" => RenderDebugMode::Depth,
            "normals" => RenderDebugMode::Normal,
            "motion_vectors" => RenderDebugMode::MotionVectors,
            "deferred" => RenderDebugMode::Deferred,
            "deferred_base_color" => RenderDebugMode::DeferredBaseColor,
            "deferred_emissive" => RenderDebugMode::DeferredEmissive,
            "deferred_metallic_roughness" => RenderDebugMode::DeferredMetallicRoughness,
            "depth_pyramid" => RenderDebugMode::DepthPyramid { mip_level: 0 },
            other => {
                return Err(BrpError::internal(&format!(
                    "unknown debug_view {other:?} (expected depth | normals | motion_vectors | \
                     deferred | deferred_base_color | deferred_emissive | \
                     deferred_metallic_roughness | depth_pyramid)"
                )));
            }
        })
    }

    /// The prepass markers the overlay's data source needs for `mode` (matching
    /// bevy_dev_tools' own F1 support rules: `Depth` reads the depth prepass or the deferred
    /// gbuffer, `Normal` the normal prepass or the gbuffer, `MotionVectors` the motion-vector
    /// prepass, the deferred modes the deferred prepass).
    pub(crate) fn required_prepasses(mode: &RenderDebugMode) -> &'static [&'static str] {
        match mode {
            RenderDebugMode::Depth => &["DepthPrepass"],
            RenderDebugMode::Normal => &["NormalPrepass"],
            RenderDebugMode::MotionVectors => &["MotionVectorPrepass"],
            RenderDebugMode::Deferred
            | RenderDebugMode::DeferredBaseColor
            | RenderDebugMode::DeferredEmissive
            | RenderDebugMode::DeferredMetallicRoughness => &["DeferredPrepass"],
            RenderDebugMode::DepthPyramid { .. } => &["DepthPrepass"],
        }
    }

    /// The capture camera for a debug view: the explicitly requested camera, else the
    /// highest-order active camera on the capture target (headless: the most recent "agent's
    /// view" camera), else the highest-order active camera on the primary window.
    pub(crate) fn resolve_camera(
        world: &mut World,
        explicit: Option<Entity>,
    ) -> Result<Entity, BrpError> {
        if let Some(camera) = explicit {
            return Ok(camera);
        }
        if let Some(target) = world.get_resource::<CaptureTarget>() {
            let target = target.0.clone();
            let mut best: Option<(Entity, isize)> = None;
            let mut query =
                world.query_filtered::<(Entity, &bevy::camera::RenderTarget, &Camera), ()>();
            for (entity, render_target, camera) in query.iter(world) {
                if render_target
                    .as_image()
                    .is_some_and(|image| image.id() == target.id())
                    && best.is_none_or(|(_, order)| camera.order > order)
                {
                    best = Some((entity, camera.order));
                }
            }
            return best.map(|(entity, _)| entity).ok_or_else(|| {
                BrpError::internal(
                    "no active camera renders to the capture target (use game/cameras to list \
                     cameras and pass `camera`)"
                        .to_owned(),
                )
            });
        }
        let mut best: Option<(Entity, isize)> = None;
        let mut query =
            world.query_filtered::<(Entity, &bevy::camera::RenderTarget, &Camera), ()>();
        for (entity, render_target, camera) in query.iter(world) {
            if matches!(
                render_target,
                bevy::camera::RenderTarget::Window(bevy::window::WindowRef::Primary)
            ) && best.is_none_or(|(_, order)| camera.order > order)
            {
                best = Some((entity, camera.order));
            }
        }
        best.map(|(entity, _)| entity).ok_or_else(|| {
            BrpError::internal(
                "no active camera renders to the primary window (use game/cameras to list \
                 cameras and pass `camera`)"
                    .to_owned(),
            )
        })
    }

    /// A deferred debug-view capture: the overlay is applied and the screenshot entity
    /// spawned [`WARMUP_FRAMES`] frames after the request (see the module docs).
    #[derive(Resource)]
    pub(crate) struct PendingDebugView {
        pub camera: Entity,
        pub mode: RenderDebugMode,
        pub previous_overlay: Option<RenderDebugOverlay>,
        pub missing_prepasses: Vec<&'static str>,
        pub capture: Option<EncodedCapture>,
        pub frames_since_apply: u32,
        /// Set to `true` once the runner has applied the overlay + prepasses; the counter
        /// then counts frames until the capture spawns (giving the render app time to
        /// extract and run the overlay pass).
        pub overlay_applied: bool,
    }

    /// The saved capture request, taken out of [`PendingDebugView`] when the warm-up is done.
    #[derive(Clone)]
    pub(crate) struct EncodedCapture {
        pub path: PathBuf,
        pub crop: Option<[u32; 4]>,
        pub max_dimension: Option<u32>,
    }

    /// The camera state to restore after a debug-view capture: prepass markers the harness
    /// inserted, and the camera's previous overlay (if any).
    #[derive(Resource)]
    pub(crate) struct RenderDebugRestore {
        pub entity: Entity,
        pub previous_overlay: Option<RenderDebugOverlay>,
        pub depth: bool,
        pub normal: bool,
        pub motion: bool,
        pub deferred: bool,
    }

    /// Runs every `Update`: applies the pending overlay on the first call, then spawns the
    /// capture a few frames later (once the overlay has been extracted and rendered); with
    /// no pending capture, nothing happens.
    pub(crate) fn runner(world: &mut World) {
        let Some(mut pending) = world.get_resource_mut::<PendingDebugView>() else {
            return;
        };
        if !pending.overlay_applied {
            // Phase 1: apply the overlay + prepasses. The render app extracts them on the
            // NEXT frame's render pass — so the capture can't spawn yet.
            pending.overlay_applied = true;
            pending.frames_since_apply = 0;
            let camera = pending.camera;
            let mode = pending.mode;
            let missing = pending.missing_prepasses.clone();
            drop(pending);
            {
                let mut entity = world.entity_mut(camera);
                entity.insert(RenderDebugOverlay {
                    enabled: true,
                    mode,
                    opacity: 1.0,
                });
                use bevy::core_pipeline::prepass::{
                    DepthPrepass, DeferredPrepass, MotionVectorPrepass, NormalPrepass,
                };
                for name in missing.iter().copied() {
                    match name {
                        "DepthPrepass" => {
                            entity.insert(DepthPrepass);
                        }
                        "NormalPrepass" => {
                            entity.insert(NormalPrepass);
                        }
                        "MotionVectorPrepass" => {
                            entity.insert(MotionVectorPrepass);
                        }
                        "DeferredPrepass" => {
                            entity.insert(DeferredPrepass);
                        }
                        _ => {}
                    }
                }
            }
            return;
        }
        // Phase 2: the overlay has been active for a few frames — the render app has
        // extracted it and the overlay/prepass pipelines are warm. Spawn the capture.
        pending.frames_since_apply += 1;
        if pending.frames_since_apply < WARMUP_FRAMES {
            return;
        }
        let Some(capture) = pending.capture.take() else {
            return;
        };
        let camera = pending.camera;
        let previous_overlay = pending.previous_overlay.clone();
        let missing = pending.missing_prepasses.clone();
        drop(pending);
        world.insert_resource(RenderDebugRestore {
            entity: camera,
            previous_overlay,
            depth: missing.contains(&"DepthPrepass"),
            normal: missing.contains(&"NormalPrepass"),
            motion: missing.contains(&"MotionVectorPrepass"),
            deferred: missing.contains(&"DeferredPrepass"),
        });
        world
            .spawn(Screenshot(bevy::camera::RenderTarget::Image(
                world
                    .get_resource::<CaptureTarget>()
                    .expect("debug_view only runs when a CaptureTarget exists")
                    .0
                    .clone()
                    .into(),
            )))
            .observe(save_encoded_to_disk(capture.path, capture.crop, capture.max_dimension))
            .observe(restore_render_debug);
        world.remove_resource::<PendingDebugView>();
    }

    /// Restores the capture camera after a debug-view capture. Runs on `ScreenshotCaptured`
    /// — the image is already transferred to the event, so removing the overlay here cannot
    /// affect the saved file. The overlay/prepass pipelines are warm by capture time (the
    /// capture was deferred past their warm-up), so restoring immediately is safe.
    pub(crate) fn restore_render_debug(
        _captured: On<ScreenshotCaptured>,
        restore: Option<Res<RenderDebugRestore>>,
        mut commands: Commands,
    ) {
        let Some(restore) = restore else {
            return;
        };
        let mut entity = commands.entity(restore.entity);
        if restore.depth {
            entity.remove::<bevy::core_pipeline::prepass::DepthPrepass>();
        }
        if restore.normal {
            entity.remove::<bevy::core_pipeline::prepass::NormalPrepass>();
        }
        if restore.motion {
            entity.remove::<bevy::core_pipeline::prepass::MotionVectorPrepass>();
        }
        if restore.deferred {
            entity.remove::<bevy::core_pipeline::prepass::DeferredPrepass>();
        }
        match &restore.previous_overlay {
            Some(previous) => entity.insert(previous.clone()),
            None => entity.remove::<RenderDebugOverlay>(),
        };
        commands.remove_resource::<RenderDebugRestore>();
    }

    /// A deferred wireframe capture: `WireframeConfig { global: true }` is inserted at
    /// request time (extracted to the render world on the next frame), and the screenshot
    /// entity spawns [`WARMUP_FRAMES`] frames later (the wireframe pipeline compiles on
    /// first use — same warm-up race as the overlay).
    #[derive(Resource)]
    pub(crate) struct PendingWireframeCapture {
        pub path: PathBuf,
        pub crop: Option<[u32; 4]>,
        pub max_dimension: Option<u32>,
        pub previous_config: Option<bevy::pbr::wireframe::WireframeConfig>,
        pub frames_since_apply: u32,
    }

    /// The wireframe config state to restore after a wireframe capture.
    #[derive(Resource)]
    pub(crate) struct WireframeRestore {
        pub previous_config: Option<bevy::pbr::wireframe::WireframeConfig>,
    }

    /// Runs every `Update`: spawns the capture after the wireframe pipeline has warmed up,
    /// then restores the previous config.
    pub(crate) fn wireframe_runner(world: &mut World) {
        let Some(mut pending) = world.get_resource_mut::<PendingWireframeCapture>() else {
            return;
        };
        pending.frames_since_apply += 1;
        if pending.frames_since_apply < WARMUP_FRAMES {
            return;
        }
        let path = pending.path.clone();
        let crop = pending.crop;
        let max_dimension = pending.max_dimension;
        let previous_config = pending.previous_config.clone();
        drop(pending);

        world.insert_resource(WireframeRestore { previous_config });
        world
            .spawn(Screenshot(bevy::camera::RenderTarget::Image(
                world
                    .get_resource::<CaptureTarget>()
                    .expect("wireframe only runs when a CaptureTarget exists")
                    .0
                    .clone()
                    .into(),
            )))
            .observe(save_encoded_to_disk(path, crop, max_dimension))
            .observe(wireframe_restore);
        world.remove_resource::<PendingWireframeCapture>();
    }

    /// Restores the previous `WireframeConfig` after a wireframe capture. The pipelines are
    /// warm by capture time (the capture was deferred past their warm-up).
    pub(crate) fn wireframe_restore(
        _captured: On<ScreenshotCaptured>,
        restore: Option<Res<WireframeRestore>>,
        mut commands: Commands,
    ) {
        let Some(restore) = restore else {
            return;
        };
        match &restore.previous_config {
            Some(previous) => {
                commands.insert_resource(previous.clone());
            }
            None => {
                commands.remove_resource::<bevy::pbr::wireframe::WireframeConfig>();
            }
        }
        commands.remove_resource::<WireframeRestore>();
    }
}

// ---------------------------------------------------------------------------
// Physics debug views — Avian3D collider gizmos via bevy_gizmos. The gizmos are drawn by
// systems that run every frame in PostUpdate (gated by `PhysicsGizmos.enabled`), so once
// enabled they appear in every subsequent capture without a warm-up race.
#[cfg(feature = "physics_debug")]
mod physics_debug {
    use super::*;
    

    /// Ensures the `PhysicsDebugPlugin` is added and the `PhysicsGizmos` config has
    /// collider rendering enabled. The gizmos are persistent — they stay on for subsequent
    /// captures until explicitly disabled or the app exits. Returns the collider color used.
    pub(crate) fn apply_physics_debug(world: &mut World) -> Result<Color, BrpError> {
        let collider_color = Color::srgb(0.0, 1.0, 0.5);
        // Toggle the PhysicsGizmos config group through GizmoConfigStore — the plugin's
        // PostUpdate systems check `enabled` and `collider_color` to decide what to draw.
        {
            let mut store = world.resource_mut::<bevy::gizmos::config::GizmoConfigStore>();
            let (gizmo_config, physics_config) =
                store.config_mut::<avian3d::debug_render::PhysicsGizmos>();
            gizmo_config.enabled = true;
            physics_config.collider_color = Some(collider_color);
            physics_config.aabb_color = Some(Color::srgba(1.0, 1.0, 0.0, 0.3));
        }
        Ok(collider_color)
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
///
/// Read filters (optional params):
/// - `{"clickable_only": true}` — only interactive rows (the "find the button" read).
/// - `{"text_contains": "Play"}` — rows whose text matches (case-insensitive substring).
/// - `{"refresh": true}` — force a full dump even when the filtered node list is identical to
///   the last read.
///
/// Unchanged suppression: the filtered node list is hashed; identical to the previous read →
/// `{unchanged: true, node_count, pointer, hovered_entities}` **without** `nodes` (the agent
/// already holds that dump). The filter is part of the hash — a different filter is a
/// different read.
pub(crate) fn ui_dump_method(params: In<Option<serde_json::Value>>, world: &mut World) -> BrpResult {
    let filter = UiFilter::from_params(params.0.as_ref());
    let refresh = params
        .0
        .as_ref()
        .and_then(|p| p.get("refresh"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let mut dump = ui_dump_snapshot(world, &filter);

    let node_count = dump["nodes"].as_array().map(|nodes| nodes.len()).unwrap_or(0);
    let hash = hash_value(&dump["nodes"]);
    let unchanged = !refresh
        && world
            .get_resource::<LastUiDump>()
            .is_some_and(|last| last.hash == Some(hash));

    if let Some(mut last) = world.get_resource_mut::<LastUiDump>() {
        last.hash = Some(hash);
    }
    if unchanged {
        // The agent already holds this exact dump; keep only the cheap, changing fields.
        let mut payload = serde_json::Map::new();
        for key in ["pointer", "hovered_entities", "target_size"] {
            if let Some(value) = dump.get(key) {
                payload.insert(key.to_owned(), value.clone());
            }
        }
        payload.insert("unchanged".into(), json!(true));
        payload.insert("node_count".into(), json!(node_count));
        payload.insert(
            "note".into(),
            json!("nodes omitted — identical to the previous game/ui read; pass refresh:true to force a full dump"),
        );
        return Ok(serde_json::Value::Object(payload));
    }
    dump["node_count"] = json!(node_count);
    Ok(dump)
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
    offscreen: Option<Res<CaptureTarget>>,
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
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn update_agent_cursor(
    offscreen: Option<Res<CaptureTarget>>,
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

/// The [`ui_dump_method`] payload. See that function's doc comment for the semantics and the
/// read filters.
fn ui_dump_snapshot(world: &mut World, filter: &UiFilter) -> serde_json::Value {
    let offscreen = world
        .get_resource::<CaptureTarget>()
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

    // The host's clickable convention (e.g. an HTML-markup UI whose buttons declare
    // `data-on-click`-style hooks instead of `bevy_ui::Interaction`), evaluated up front over
    // the stack — the hook takes `&World`, which the per-row query pass below would otherwise
    // conflict with.
    let extra_clickable: std::collections::HashSet<Entity> = match world
        .get_resource::<McpHarnessConfig>()
        .and_then(|config| config.clickable.clone())
    {
        Some(clickable) => stack
            .iter()
            .copied()
            .filter(|&entity| clickable(world, entity))
            .collect(),
        None => Default::default(),
    };

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
    // system only updates nodes that have it) — or the host's `clickable` hook claims it.
    // Pressed/hovered come from `Interaction` itself, hovered also from picking's own
    // `Hovered` component.
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
        let clickable = interaction.is_some() || extra_clickable.contains(&entity);
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

    // Read filters (`clickable_only` / `text_contains`) — applied before the ancestor fold so
    // a kept interactive row still folds its kept-label children.
    rows.retain(|row| filter.keeps(row.text.as_deref(), row.clickable));

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
                .ok_or_else(|| BrpError::internal(format!("unknown button {button_name:?}")))?;
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
            Ok(json!({"gamepad_entity": gamepad_entity, "button": button_name, "pressed": pressed}))
        }
        "axis" => {
            let axis_name = params
                .get("axis")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| BrpError::internal("missing params.axis"))?;
            let axis = parse_gamepad_axis(axis_name)
                .ok_or_else(|| BrpError::internal(format!("unknown axis {axis_name:?}")))?;
            let value = params
                .get("value")
                .and_then(serde_json::Value::as_f64)
                .ok_or_else(|| BrpError::internal("missing params.value"))? as f32;
            gamepad.analog_mut().set(axis, value);
            Ok(json!({"gamepad_entity": gamepad_entity, "axis": axis_name, "value": value}))
        }
        "reset" => {
            *gamepad = Gamepad::default();
            Ok(json!({"gamepad_entity": gamepad_entity, "reset": true}))
        }
        other => Err(BrpError::internal(format!(
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
        return Ok(json!({"reset": true}));
    }
    let key_name = params
        .get("key")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| BrpError::internal("missing params.key"))?;
    let key: KeyCode = serde_json::from_value(json!(key_name))
        .map_err(|err| BrpError::internal(format!("unknown key {key_name:?}: {err}")))?;
    let pressed = params
        .get("pressed")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    if pressed {
        keys.press(key);
    } else {
        keys.release(key);
    }
    Ok(json!({"key": key_name, "pressed": pressed}))
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
    if let Some(target) = world.get_resource::<CaptureTarget>() {
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
                .ok_or_else(|| BrpError::internal(format!("unknown button {button_name:?}")))?;
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
            Ok(json!({"button": button_name, "pressed": pressed}))
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
            Ok(json!({"dx": dx, "dy": dy}))
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
            Ok(json!({"x": x, "y": y}))
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
                    return Err(BrpError::internal(format!(
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
            Ok(json!({"x": x, "y": y, "unit": unit_name}))
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
            Ok(json!({"reset": true}))
        }
        other => Err(BrpError::internal(format!(
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
