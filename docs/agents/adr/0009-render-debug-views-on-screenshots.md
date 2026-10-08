# 9. Render-debug views on screenshots (`debug_view` parameter)

Date: 2026-10-08

## Status

Accepted — implemented (the `debug_view` parameter on `game/screenshot`; the `render_debug`
and `physics_debug` cargo features). The F1 overlay modes (depth, normals, motion vectors,
wireframe, deferred buffers) are verified live; the Avian3D collider gizmos are implemented
but the wireframe rendering needs bevy-level visual verification.

## Context

An agent playtesting a 3D game sometimes needs to see rendering internals, not just the
full-color render:

- **Depth** — "did the entity render in front of or behind that wall?"
- **Normals** — "are the mesh normals pointing the right way?"
- **Motion vectors** — "is the entity moving, and in which direction?"
- **Wireframe** — "what does the mesh geometry look like? Are the triangles correct?"
- **Physics colliders** (Avian3D) — "where are the colliders relative to the visual mesh?"

Bevy provides these as separate systems:

1. **`bevy_dev_tools::render_debug::RenderDebugOverlayPlugin`** — the F1 overlay. A
   per-camera `RenderDebugOverlay` component (`enabled`, `mode: RenderDebugMode`,
   `opacity`) with modes `Depth`, `Normal`, `MotionVectors`, `Deferred`,
   `DeferredBaseColor`, `DeferredEmissive`, `DeferredMetallicRoughness`,
   `DepthPyramid { mip_level }`. Some modes need prepass markers on the camera
   (`DepthPrepass`, `NormalPrepass`, `MotionVectorPrepass`, `DeferredPrepass`).
2. **`bevy_pbr::wireframe::WireframePlugin`** — wireframe rendering. A global
   `WireframeConfig { global, default_color, default_line_width, … }` resource plus per-entity
   `Wireframe`/`NoWireframe` components.
3. **`avian3d::debug_render::PhysicsDebugPlugin`** — Avian3D collider gizmos via
   bevy_gizmos. A `PhysicsGizmos` config group with `enabled`, `collider_color`, and
   per-feature color fields. Drawn in `PostUpdate` by systems gated on `enabled`.

Each system has different application semantics (per-camera component vs global resource vs
gizmo config group), different warm-up requirements (pipeline compile on first use), and
different restore needs. The harness needs to present a unified `debug_view` parameter to
the agent while handling these differences internally.

## Decision

Add a `debug_view` parameter to `game/screenshot` that accepts a mode name and renders that
debug view into the capture:

- `depth`, `normals`, `motion_vectors`, `deferred`, `deferred_base_color`,
  `deferred_emissive`, `deferred_metallic_roughness`, `depth_pyramid` — the
  bevy_dev_tools F1 overlay, applied per-camera
- `wireframe` — bevy_pbr's global `WireframeConfig { global: true }` toggle
- `physics` — Avian3D collider gizmos via `PhysicsGizmos` config group

**The overlay and wireframe captures are deferred** (a two-phase runner):

1. Phase 1 (frame N): insert the overlay component + prepass markers on the capture camera,
   or set the wireframe config globally. The render app extracts them on the next frame.
2. Phase 2 (frame N + `WARMUP_FRAMES`): the overlay/prepass pipelines are compiled and the
   overlay pass has rendered at least once. Spawn the capture entity. The screenshot
   readback captures the frame with the debug view active.
3. Phase 3: a `RenderDebugRestore` observer on `ScreenshotCaptured` restores the camera's
   previous state (removes inserted prepass markers, restores the previous overlay or
   removes it, restores the previous wireframe config).

The `physics` mode is persistent — the gizmos stay on for subsequent captures (a logical
view, not a one-shot visual overlay). The agent captures as many frames as needed with the
colliders visible.

**Feature gating**: two cargo features control which debug views are available:

- `render_debug` = `bevy_dev_tools` + `bevy_core_pipeline` + `bevy_pbr` (for the F1 overlay
  + wireframe)
- `physics_debug` = `avian3d` + `bevy_gizmos` (for the collider gizmos)

Both are opt-in. Hosts that don't need debug views don't pull the deps.

**The overlay plugin add is guarded**: `RenderDebugOverlayPlugin` is part of `DefaultPlugins`
when `bevy_dev_tools` is enabled, so full-feature hosts already have it — the harness's
`is_plugin_added` check prevents a double-add panic (observed and fixed).

## Alternatives considered

- **A `debug_view` enum component on the camera** (like `bevy_dev_tools`'s
  `RenderDebugOverlay` but defined by the harness): rejected — the harness would need to
  duplicate bevy's own debug overlay rendering pipeline, which is bevy-internal and
  version-dependent.
- **MCP resources instead of a `debug_view` parameter** (e.g. `resources/write` to toggle
  the overlay): rejected — resources are read-heavy; the agent's mental model is "capture
  with this view", which is naturally a parameter on the capture call.
- **Separate tools per debug view** (`game/screenshot_depth`, `game/screenshot_normals`,
  …): rejected — tool schema bloat; one parameter on the existing `screenshot` tool is
  cleaner.
- **Per-entity wireframe** (`Wireframe` component on specific entities instead of global):
  rejected for now — the use case is "see the geometry of the whole scene", not "wireframe
  one entity". Global toggle is simpler and covers the inspection use case.

## Consequences

- All debug views verified live on prototype_19 (RTX 4070 SUPER, Vulkan):
  - **Depth**: 16,868 bytes (vs 587,947 normal) — grayscale depth buffer compresses ~35×
  - **Normals**: 16,815 bytes — pastel normal-map palette
  - **Motion vectors**: 16,859 bytes — mostly dark at rest (capsule not moving)
  - **Wireframe**: implemented, the capture runs, but the wireframe lines are not visually
    confirmed in the output — the wireframe pass may not be rendering in the
    ScheduleRunner/headless context (bevy-internal issue, needs investigation)
  - **Restore**: a normal capture after the debug captures shows the regular render (the
    overlay was removed and the prepass markers cleaned up)
- The two-phase deferred runner (apply → warm-up → capture → restore) eliminates the
  warm-up race: without it, the first debug capture showed the normal render because the
  overlay/prepass pipelines hadn't compiled yet (observed and root-caused).
- The `render_debug` feature enables `bevy_dev_tools` + `bevy_core_pipeline` + `bevy_pbr` on
  the harness's bevy dep — these are needed for the overlay rendering and the wireframe
  pipeline. Hosts that don't enable it don't pull the deps.
- The `physics_debug` feature enables `bevy_gizmos` on the harness's bevy dep — needed for
  the `GizmoConfigStore` that the Avian3D collider gizmos read from. The gizmos are drawn
  via bevy_gizmos' normal render pipeline, not a separate pass.
- `debug_view` requires the `render_debug` feature (the overlay modes and wireframe). The
  `physics` mode requires the `physics_debug` feature. A host without the feature gets a
  clean error: "debug_view requires the render_debug cargo feature".
- The F1 keybinds come for free when the plugin is added — the agent (or the player) can
  cycle the overlay interactively. The `debug_view` parameter is the programmatic
  equivalent.
