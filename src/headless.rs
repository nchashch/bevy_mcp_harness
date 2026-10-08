//! Headless-mode rendering support: the shared offscreen texture every camera renders into,
//! the camera retargeting that makes window-less apps produce frames, and the render-less
//! (`no_render`) shim that keeps UI layout working without any render app. Ported from
//! PROTOTYPE_19's `controls/camera.rs`.

use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::render_resource::{TextureFormat, TextureUsages};
use bevy::ui::IsDefaultUiCamera;

/// The offscreen texture every camera renders to in headless mode — the rendered view the
/// agent's `game/screenshot` tool reads. Created once at plugin build (see
/// [`crate::McpHarnessConfig::offscreen_size`]).
#[derive(Resource, Clone)]
pub struct OffscreenRenderTarget(pub Handle<Image>);

impl OffscreenRenderTarget {
    pub fn new(width: u32, height: u32, images: &mut Assets<Image>) -> Self {
        let mut image = Image::new_target_texture(width, height, TextureFormat::Rgba8UnormSrgb, None);
        // The screenshot readback copies from this texture; the default render-attachment
        // usage doesn't include COPY_SRC.
        image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
        Self(images.add(image))
    }

    /// The target's current size in pixels, when its image asset exists.
    pub fn size(&self, images: &Assets<Image>) -> Option<UVec2> {
        images.get(&self.0).map(|image| image.size())
    }
}

/// Marker for render-less mode (headless agent host with the render plugins disabled — no
/// wgpu/Vulkan at all). Consumers: [`crate::brp`]'s screenshot methods return a clean error
/// instead of waiting on a capture that can never complete.
#[derive(Resource)]
pub struct NoRenderMode;

/// `no_render` shim: feeds each camera's `Camera.computed.target_info` by hand, because the
/// system that normally computes it (`camera_system` in `bevy_render`) doesn't run without the
/// render app. `bevy_ui`'s camera propagation, layout, and `bevy_picking`'s UI backend all read
/// exactly these accessors (`target_scaling_factor()` / `physical_target_size()` →
/// `computed.target_info`), so with the shim in place UI *layout*, the `game/ui` dump, hover,
/// and clicks all work with zero rendering. The size comes from the offscreen target when one
/// exists (scale factor 1: image targets have no DPI scaling). `clip_from_view` stays identity,
/// which nothing in a render-less app consumes.
pub fn shim_camera_computed(
    offscreen: Option<Res<OffscreenRenderTarget>>,
    images: Option<Res<Assets<Image>>>,
    mut cameras: Query<&mut Camera>,
) {
    const FALLBACK: UVec2 = UVec2::new(1280, 800);
    let size = offscreen
        .as_ref()
        .zip(images.as_ref())
        .and_then(|(target, images)| target.size(images))
        .unwrap_or(FALLBACK);
    for mut camera in &mut cameras {
        let needs_shim = !matches!(
            camera.computed.target_info,
            Some(bevy::camera::RenderTargetInfo {
                physical_size,
                scale_factor: 1.0,
            }) if physical_size == size
        );
        if needs_shim {
            camera.computed.target_info = Some(bevy::camera::RenderTargetInfo {
                physical_size: size,
                scale_factor: 1.0,
            });
        }
    }
}

/// Rewrites every camera that would render nowhere to render into [`OffscreenRenderTarget`]
/// instead — loaded worlds/scenes carry their own cameras that would otherwise render nowhere,
/// and the app's content cameras are covered by the same sweep. Polling rather than an
/// `On<Add, Camera>` observer: cameras can appear at any time, and `RenderTarget` may also
/// *change back* after spawn.
///
/// Two value shapes render nowhere: `Window(_)` (headless — there is no window) and
/// `RenderTarget::None { .. }` (authored/glTF cameras that opt out of a target). Deliberately
/// untouched: `Image(_)` (cameras already rendering into a texture — stealing it would blank
/// whatever displays that texture) and `TextureView(_)` (XR eye targets).
///
/// All the claimed cameras share ONE target, so each must also stop clearing it
/// (`ClearColorConfig::None`) — a camera that clears erases every camera that rendered before
/// it. The ordering invariant "UI camera drawn last, exactly one camera clears" is maintained
/// by [`maintain_default_ui_camera`] + [`keep_ui_camera_drawn_last`] below.
///
/// **The target_info recompute gotcha** (found via `bevy_render::camera::camera_system`): every
/// camera spawns pointed at the default `RenderTarget::Window(Primary)`, but headless mode
/// never has a primary window — so `RenderTarget::normalize` returns `None` for it, and
/// `camera_system` silently skips its *entire* per-camera body (including the
/// `Camera.computed.target_info` recompute) for any camera still in that state. Critically,
/// merely running that `Query` item still consumes the camera's one-tick `is_added()` window
/// even though the skipped body never reads it. By the time this system gets around to
/// retargeting a camera that spawned mid-session, `is_added()` has already gone false, and the
/// shared image's own one-time `AssetEvent::Added` was consumed by whichever camera claimed it
/// first. Nothing in `camera_system`'s recompute gate (window/image asset events, `is_added()`,
/// projection change, viewport-size change) ever fires again for that camera, so `target_info`
/// — and thus its render output — stays permanently unresolved. The fix forces the one
/// recompute condition this system *can* trigger deliberately: touching `Projection`'s own
/// change-detection flag right when we fix the target, so `camera_projection.is_changed()` is
/// true on the very next pass — regardless of the race above. `set_changed()` alone (no value
/// mutation) is enough.
pub fn retarget_cameras_to_offscreen(
    offscreen: Option<Res<OffscreenRenderTarget>>,
    mut cameras: Query<(Entity, &mut RenderTarget, &mut Camera, &mut Projection)>,
    mut next_order: Local<u32>,
) {
    let Some(offscreen) = offscreen else {
        return;
    };
    for (entity, mut target, mut camera, mut projection) in &mut cameras {
        if matches!(
            *target,
            RenderTarget::Window(_) | RenderTarget::None { .. }
        ) {
            *target = RenderTarget::Image(offscreen.0.clone().into());
            *next_order += 1;
            camera.order = *next_order as isize;
            if *next_order == 1 {
                // The first-claimed camera renders first (lowest order): it is the shared
                // target's base layer and keeps its clear. Every later claim draws on top of
                // it instead of erasing it — a camera left on `Default` clear would erase every
                // camera that rendered before it, leaving only the last camera's frame.
            } else {
                camera.clear_color = ClearColorConfig::None;
            }
            // See the doc comment above: this is what actually makes `camera_system` compute
            // `target_info` for this camera at all, since the `RenderTarget`/`Camera` writes
            // above don't by themselves satisfy its recompute gate.
            projection.set_changed();
            info!(
                "retargeted camera {entity} to the offscreen target (order {})",
                *next_order
            );
        }
    }
}

/// Marks headless mode's `Startup`-spawned UI camera specifically, distinguishing it from the
/// app's own content cameras (which may also carry `IsDefaultUiCamera`) so
/// [`maintain_default_ui_camera`] can hand the marker between holders.
#[derive(Component)]
pub struct HeadlessUiCameraBootstrap;

/// Keeps "exactly one live entity carries `IsDefaultUiCamera`" true at all times, headless
/// offscreen mode only. `bevy_ui`'s fallback for "no unique holder" only considers cameras
/// whose `RenderTarget` is `Window(Primary)` — structurally dead in headless mode, where every
/// camera targets `Image` — so this invariant has to be maintained here rather than relying on
/// upstream to recover from a momentary zero- or two-holder state. Hands the marker to the
/// bootstrap camera whenever nothing else holds it (covers boot, and every teardown of the
/// app's content cameras), and strips it the instant a real content camera claims it on its
/// own — a one-tick window where both exist is possible but harmless, since the next run of
/// this system resolves it before `camera_system`/`bevy_ui` do anything observably wrong.
pub fn maintain_default_ui_camera(
    bootstrap: Query<(Entity, Has<IsDefaultUiCamera>), With<HeadlessUiCameraBootstrap>>,
    other_holders: Query<
        Entity,
        (
            With<IsDefaultUiCamera>,
            Without<HeadlessUiCameraBootstrap>,
        ),
    >,
    mut commands: Commands,
) {
    let Ok((bootstrap_entity, bootstrap_has_marker)) = bootstrap.single() else {
        return;
    };
    let other_holder_exists = !other_holders.is_empty();
    if other_holder_exists && bootstrap_has_marker {
        commands.entity(bootstrap_entity).remove::<IsDefaultUiCamera>();
    } else if !other_holder_exists && !bootstrap_has_marker {
        commands.entity(bootstrap_entity).insert(IsDefaultUiCamera);
    }
}

/// Given [`maintain_default_ui_camera`]'s invariant (exactly one live `IsDefaultUiCamera`
/// holder), keeps whichever entity currently holds it drawn *last* — highest order, no clear —
/// among the cameras sharing the offscreen target, so it actually composites on top of a
/// later-arriving opaque 3D world camera instead of being painted over by one. Re-derived fresh
/// every tick (not decided once at claim time) because *which* entity holds the marker changes
/// over a session. Write-if-different throughout: `Mut<Camera>` flags `Changed<Camera>` on any
/// dereference for write even when the assigned value doesn't change, and this runs every tick.
pub fn keep_ui_camera_drawn_last(
    offscreen: Option<Res<OffscreenRenderTarget>>,
    mut cameras: Query<(
        Entity,
        &RenderTarget,
        &mut Camera,
        Has<IsDefaultUiCamera>,
    )>,
) {
    let Some(offscreen) = offscreen else {
        return;
    };
    let on_our_target = |target: &RenderTarget| {
        matches!(target, RenderTarget::Image(image) if image.handle == offscreen.0)
    };

    // Phase 1: bump the UI camera above whatever else currently exists, if anything does.
    // Highest order *excluding* the UI camera itself — the bump target has to be a fixed point
    // that doesn't move just because the UI camera's own order changed, or bumping it to
    // "highest + 1" every tick would increment it forever. `None` when no other camera exists
    // yet — nothing to bump above, so the UI camera keeps its initial claim order.
    let highest_non_ui_order = cameras
        .iter()
        .filter(|(_, target, _, is_ui_camera)| on_our_target(target) && !is_ui_camera)
        .map(|(_, _, camera, _)| camera.order)
        .max();
    if let Some(highest_non_ui_order) = highest_non_ui_order {
        let wanted = highest_non_ui_order + 1;
        for (_, target, mut camera, is_ui_camera) in &mut cameras {
            if is_ui_camera && on_our_target(target) && camera.order != wanted {
                camera.order = wanted;
            }
        }
    }

    // Phase 2: re-derive lowest order fresh, *after* any Phase 1 bump — computing it before
    // would use the UI camera's stale pre-bump order, meaning on the exact tick it first moves
    // away from being lowest, nothing would end up matching and the target would go
    // unrendered-to (not cleared) for that one tick.
    let Some(lowest_order) = cameras
        .iter()
        .filter(|(_, target, ..)| on_our_target(target))
        .map(|(_, _, camera, _)| camera.order)
        .min()
    else {
        return;
    };

    // Phase 3: whichever camera now has that lowest order clears; everyone else on this target
    // doesn't. Deliberately not excluding the UI camera here — when it's the only camera that
    // exists, it's trivially both lowest and highest, and correctly keeps `Default` (the
    // bootstrap-alone case). Once a non-UI camera also exists and Phase 1 has bumped the UI
    // camera above it, the UI camera naturally stops being lowest on its own.
    for (entity, target, mut camera, _) in &mut cameras {
        if !on_our_target(target) {
            continue;
        }
        let wants_default = camera.order == lowest_order;
        let is_default = matches!(camera.clear_color, ClearColorConfig::Default);
        if wants_default != is_default {
            camera.clear_color = if wants_default {
                ClearColorConfig::Default
            } else {
                ClearColorConfig::None
            };
            info!(
                "camera {entity} clear -> {}",
                if wants_default { "Default" } else { "None" }
            );
        }
    }
}
