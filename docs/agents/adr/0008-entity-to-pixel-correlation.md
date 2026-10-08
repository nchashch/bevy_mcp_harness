# 8. Entity-to-pixel correlation (`entities_on_screen`)

Date: 2026-10-08

## Status

Accepted — implemented (`game/entities_on_screen` BRP method; also embedded in every
`game/screenshot/get` response as an `"entities"` field).

## Context

An agent looking at a screenshot sees pixels but has no way to correlate those pixels to
entity ids. "There's a capsule at roughly (640, 400)" requires OCR or guessing. The agent
needs to know: "entity 4294967106 (the player capsule) is centered at (640, 400) with a
bounding box of [560, 360, 160, 80]." Without this mapping, the agent can't:
- click a specific entity (`game/mouse move_to` needs pixel coords),
- inspect a specific entity's state (`world.get_components` needs an entity id),
- or reason about spatial relationships between entities.

Bevy provides the pieces: `Aabb` (world-space bounding box computed from the mesh),
`GlobalTransform` (world position), and `Camera::world_to_viewport` (projects a world
position to viewport pixel coordinates). The harness just needs to combine them.

## Decision

A new `entities_on_screen_data` helper that, given a camera entity, projects all visible
`Aabb` entities into screenspace:

- **Camera resolution**: an explicit `camera` param, else the highest-order active `Camera`
  entity (the one the agent "sees through" — the most recently claimed camera on the
  offscreen target).
- **Per entity**: project the `Aabb` center to get the screenspace center; project all 8
  corners of the world-space AABB (`center ± half_extents` per axis) to compute the exact
  2D bounding box (an axis-aligned box viewed from any angle needs all 8 corners projected
  for an exact 2D bbox); compute depth as the distance from the camera to the AABB center.
- **Filters**: entities with `InheritedVisibility = false` or `ViewVisibility = false` are
  excluded; entities behind the camera (no corner projects in front) are excluded.
- **Output**: `{entity, name, center: [x, y], bounding_box: [x, y, w, h], depth}` per
  entity, sorted by depth (nearest first). The `name` comes from the `Name` component
  (optional).

The data is embedded in every `game/screenshot/get` response as an `"entities"` field, so
the agent gets the PNG + the entity table in one call. A standalone
`game/entities_on_screen` BRP method serves the same data without a capture.

## Alternatives considered

- **Render-world `VisibleEntities`** — per-camera visibility sets extracted to the render
  world. More accurate (render-side culling) but requires reading from the render app,
  which a BRP handler (main world, `&mut World`) cannot do. `ViewVisibility` + viewport
  projection is a sufficient proxy.
- **Depth-buffer-based picking** (read the depth texture, project a ray, find the entity):
  more precise for "what am I looking at" queries, but requires reading the depth texture
  from the GPU (async readback) and mapping the depth value back to an entity — much more
  complex for marginal gain.
- **A separate `entities_on_screen` MCP tool**: unnecessary — the data is embedded in the
  screenshot response, which the agent already reads.

## Consequences

- The agent gets entity-to-pixel correlation for free with every capture — "the capsule is
  at (640, 400), the ground plane is at (640, 600)" — enabling targeted `game/mouse` clicks,
  `world.get_components` lookups, and spatial reasoning.
- Only entities with `Mesh3d` (and therefore `Aabb`) are included. Entities without meshes
  (lights, audio, logic-only) are invisible to this surface — use BRP `world.query` for
  those.
- The 2D bounding box is an approximation: it's the projection of the world-space
  axis-aligned AABB, which may be larger than the entity's visual footprint (especially for
  rotated entities whose world-space AABB is larger than the rotated mesh). Good enough for
  entity correlation; not pixel-exact.
- `game/ui` handles UI nodes (buttons, panels) separately — this surface is for 3D entities.
  The two surfaces are complementary: `game/ui` for UI interaction, `entities_on_screen`
  for 3D entity inspection.
