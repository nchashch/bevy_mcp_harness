# Changelog

All notable changes to bevy_mcp_harness are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Before 1.0, a minor version bump (0.2 → 0.3) may contain breaking changes.

The design history behind these changes lives in
[`docs/agents/adr/`](docs/agents/adr/README.md).

## [Unreleased]

### Changed

- **Bevy 0.20** (the 0.4 line; the 0.3 line stays on bevy 0.19.1). The minimal-Bevy
  feature list is unchanged (all nine features exist in 0.20); `bevy_dev_tools` bumps with
  it. Host-facing behavior changes:
  - **`physics_debug` is absent on this line.** The newest avian3d (0.7) requires
    bevy `^0.19`, so no avian3d release compiles against bevy 0.20 — keeping the feature
    would break every `--all-features` build. `debug_view: "physics"` now returns a clean
    error instead of silently producing a plain capture (the previous no-feature fallback,
    a latent bug this exposed). The feature returns when avian3d updates.
  - **`game/ui` reads bevy 0.20's widget state**: the dump now also reads
    `bevy_ui::Pressed` and marks hover from the **hover map** (bevy 0.20's `Hovered`
    component is opt-in — only maintained on entities that already carry it, so it never
    appears on a widget button). A hovered/pressed state on a label child propagates up to
    the kept interactive ancestor (the fold), matching the legacy `Interaction` semantics:
    a hovered widget button reads `Hovered`, not `Idle`. Verified live end-to-end: Idle →
    Hovered → Pressed → Hovered on the example's widget button.
  - The example migrates to the `ui_widgets::Button` widget (`ButtonPlugin` observers +
    `InputFocusPlugin` in the render-less composition — `DefaultPlugins` provides both in
    the rendered branch) and registers a `clickable` hook for the new widget — the
    documented extension point, since the harness's default `clickable` convention (legacy
    `Interaction`) is unchanged and `bevy_ui::Interaction` remains deprecated-but-maintained
    in 0.20 (bevy's own `Button` still requires it). Legacy-UI hosts see no behavior change.

## [0.3.1] - 2026-10-09

### Changed

- **Captures always save the full-resolution frame** ([ADR
  0011](docs/agents/adr/0011-always-full-res-captures-and-annotation-workflow.md)).
  The file on disk is
  never cropped or downscaled: `crop` and `max_dimension` now shape only the
  *served view* (`png_base64` — the token-efficient image the agent looks at),
  applied at poll time to the full-res file. The agent-facing tool surface is
  unchanged — this completes 0.3.0's documented "persistent, human-browsable
  record" intent rather than changing it; scripts that read the screenshots
  directory directly (report flows, file parsers) see full-resolution frames
  where they previously saw the downscaled/cropped view. The report/human
  artifact is the high-res file, and the coordinate tables (`entities`,
  `game/ui` rects) map onto it 1:1 — annotators draw directly on it, no
  scaling math.

### Added

- **Self-contained annotation sidecars** — every `<capture>.json` sidecar now
  carries the per-frame `entities` projection table and an `alignment` block
  (the file's `png_size`, the `capture_size`, and the served
  `view` `{size, crop, max_dimension, coordinate_scale}`) beside the PNG path
  and the `state` snapshot. The per-frame entity table lived only in the poll
  response before — unrecoverable post-hoc, since `entities_on_screen`
  projects the *current* frame. A later session, a human, or the report flow
  can annotate an existing capture with no harness calls; the annotation
  convention (boxes/labels from the tables, `<name>-annotated.png` beside the
  untouched original) is documented in the playtest guide §6a and
  cross-referenced from the bugreport guide's Evidence section.
- [ADR 0011](docs/agents/adr/0011-always-full-res-captures-and-annotation-workflow.md)
  records the two-readers split and the annotation workflow.

## [0.3.0] - 2026-10-08

### Added

- **Render-debug views on screenshots** (`render_debug` feature;
  [ADR 0009](docs/agents/adr/0009-render-debug-views-on-screenshots.md)).
  `game/screenshot` accepts a `debug_view` parameter: `depth`, `normals`,
  `motion_vectors`, `deferred`, `deferred_base_color`, `deferred_emissive`,
  `deferred_metallic_roughness`, `depth_pyramid` (the `bevy_dev_tools` F1
  overlay), `wireframe` (`bevy_pbr`'s global `WireframeConfig` toggle), and —
  under the separate `physics_debug` feature — `physics` (Avian3D collider
  gizmos via `bevy_gizmos`, a persistent toggle rather than a one-shot
  overlay). Overlay and wireframe captures run through a two-phase deferred
  runner (apply camera state → warm-up frames for pipeline compile → spawn the
  capture → restore the camera's previous state on `ScreenshotCaptured`),
  which eliminates the warm-up race that otherwise made the first debug
  capture show the plain render. All views verified live against a real
  rendering host, pixel-level (depth 16.9 KB grayscale vs ~590 KB full-color;
  wireframe/physics confirmed by their green line pixels).
- **Batched debug views** (`debug_views` on the MCP `screenshot` tool;
  [ADR 0010](docs/agents/adr/0010-batched-debug-views.md)). One tool call
  captures N views of the same scene moment and returns N image blocks —
  instead of N round trips whose captures drift apart as the game advances.
  Each view's poll matches on the exact capture path its own `capturing`
  response returned, so captures cannot be attributed to the wrong view (a
  naive "newest PNG" poll did exactly that). `["physics", "wireframe",
  "depth"]` verified in a single call.
- **Entity-to-pixel correlation** ([ADR
  0008](docs/agents/adr/0008-entity-to-pixel-correlation.md)). The
  `game/entities_on_screen` BRP method — and the `entities` array embedded in
  every `game/screenshot/get` response — projects each visible `Aabb`-bearing
  entity through the active 3D camera into screenshot pixel space:
  `{entity, name, center, bounding_box, depth}`, sorted nearest-first. The
  same coordinates `game/mouse move_to` consumes, so "click entity X" is read
  off the screenshot response rather than estimated from the image. Entities
  are skipped behind the camera; UI cameras are never used for projection.
- **CI** (`.github/workflows/ci.yml`): a minimal-Bevy lib guard (the lib
  feature list must build without Bevy's default features — the wayland-sys
  regression that motivated 0.2.1 fails here if reintroduced), build/tests/
  clippy/docs across the feature matrix, examples, and a publish dry-run.
  Lib tests run in the all-targets job: dev-dependencies pull full Bevy, so
  the minimal-guard job must not see them.

### Fixed

- `entities_on_screen` projected `Aabb` centers in the entity's **local**
  space, placing every entity behind the camera (0 results) — AABBs are now
  transformed through the entity's `GlobalTransform` before projection, and
  the projected bounding box is computed from all 8 transformed corners
  (correct for rotated entities).
- The physics view's `PhysicsDebugPlugin` add panicked on hosts that already
  add it themselves (prototype_19's own UI code does) — the harness's add is
  now guarded with `is_plugin_added`.

## [0.2.1] - 2026-10-08

### Fixed

- **Minimal Bevy dependency.** The harness now declares `bevy` with
  `default-features = false` and only the features its code touches
  (`bevy_camera`, `bevy_log`, `bevy_mesh`, `bevy_picking`, `bevy_remote`,
  `bevy_render`, `bevy_text`, `bevy_ui`, `serialize`). Hosts whose Bevy is
  minimal — a headless dedicated server embedding the harness — no longer
  inherit Bevy's default set, which pulled `bevy_winit`/Wayland and broke
  GPU-less CI builds on `wayland-sys`'s `pkg-config` probe. Examples build
  against the full feature set via a dev-dependency; host builds of the lib
  never see it.

## [0.2.0] - 2026-10-08

### Added

- **Pre-flight preconditions.** `register_game_method_with_precondition`
  declares a `PreconditionFn` (`fn(&World, params) -> Result<(), String>`)
  per method, and the `{prefix}/plan_check` BRP method + `plan_check` MCP tool
  pre-flight an intended call sequence against those declarations — per-call
  `ok`/`reason` (unknown methods report the registered list). Advisory by
  design: the method's own checks remain the source of truth at call time. The
  built-in `screenshot` declares one (rendering enabled + a capture target).
  [ADR 0006](docs/agents/adr/0006-pre-flight-preconditions-and-no-pddl-planner.md)
  records why a PDDL planner was declined.
- **Token-efficiency tool set** ([ADR
  0007](docs/agents/adr/0007-token-efficiency-tool-set.md)):
  - **`game/ui` unchanged-suppression and filters.** The filtered node list is
    hashed; a re-read identical to the previous one answers
    `{unchanged: true, node_count, pointer, hovered_entities}` without `nodes`.
    Filters: `clickable_only` (the find-the-button read) and `text_contains`
    (case-insensitive). `refresh: true` forces a full dump.
  - **`wait_until` (MCP only).** Server-side polling with a timeout over
    `game_state` or `game_ui` — one call instead of a sleep/re-poll loop.
    Conditions: a dot-separated `path` + `equals` (numbers compare
    numerically), or `text_contains` for `game/ui`; returns `{met, attempts,
    elapsed_ms, last}`.
  - **`click_node` (MCP only).** Compound UI click: dump → match a clickable
    node (`text_contains`/`entity`/`rect`) → `move_to` its center → press →
    release → return the post-click full UI dump. One call instead of the
    dump/move/press/release/re-dump sequence.
  - **`screenshot` `max_dimension`** (64..=4096): downscales the encoded PNG to
    fit the long edge (aspect preserved, Lanczos3) — ~640 for overview checks
    costs ~4× fewer vision pixels; the capture stays full-resolution and the
    crop is applied before the resize.
  - **`input_sequence` (MCP only).** Scripted device-mock choreography:
    `{"keyboard": {…}}`/`{"gamepad": {…}}`/`{"mouse": {…}}` steps with
    `{"ticks": n}` waits, executed with internal timing. One call instead of a
    chain of mock calls with sleeps between.
  - **`game_assert` (MCP only).** Declarative verification:
    `{source, path, op, value}` expectations (`eq`/`ne`/`gt`/`gte`/`lt`/`lte`/
    `exists`/`text_contains`, numbers compared numerically) return compact
    pass/fail with only the mismatches and their actuals.
- **Orchestration-tools boundary.** Tools that sleep or iterate over time
  (`wait_until`, `click_node`, `input_sequence`, `game_assert`) are MCP-only —
  a BRP handler runs inside the app's frame loop and cannot block. Instant,
  single-shot tools stay BRP methods. [ADR
  0007](docs/agents/adr/0007-token-efficiency-tool-set.md).
- **Architecture Decision Records** in `docs/agents/adr/`: the extraction
  from prototype_19 (0001), the generalization boundary (0002), headless
  rendering ownership (0003), the bundled guides (0004), the extension
  ergonomics (0005) and the pre-flight design (0006).

### Fixed

- Built-in MCP tools double-prefixed their BRP targets under a non-default
  `method_prefix` (`game/game/ui`) — all built-in calls now go through one
  prefix-aware helper.

## [0.1.0] - 2026-10-08

First release. Extracted from prototype_19's `dev::tool_api`
([ADR 0001](docs/agents/adr/0001-extract-the-agent-tool-api-into-a-reusable-crate.md)),
with the generalization boundary of
[ADR 0002](docs/agents/adr/0002-generalization-boundary-what-stays-generic.md)
and the first real host adoption's ergonomics
([ADR 0005](docs/agents/adr/0005-extension-ergonomics-from-the-first-host-adoption.md))
already folded in.

### Added

- **BRP layer.** The Bevy Remote Protocol server (guarded — added only when the
  host hasn't, so hosts like Skein that own `RemotePlugin` coexist), with
  custom `game/*` methods:
  - `game/state` — the host-registered snapshot hook
    (`McpHarnessConfig::state_snapshot`), fused into every screenshot response
    and written to a `.json` sidecar beside each capture.
  - `game/client_info` — effective launch flags, ports, capture-target size,
    screenshots availability.
  - `game/cameras` — camera entities and poses (ids usable as `screenshot`'s
    `camera` param).
  - `game/screenshot` + `game/screenshot/get` — async capture to a persistent
    PNG (with optional `label`, `crop` in `game/ui` pixel space, `camera`
    targeting with automatic order restore), base64 on poll with
    **unchanged-suppression** for pixel-identical re-reads, the `game/state`
    payload fused into every response, and a `.json` sidecar per capture.
  - `game/ui` — accessibility-tree dump in screenshot pixel space:
    back-to-front `UiStack` order, rects, text (`Text`/`TextSpan` aggregation
    for buttons), `clickable`, interaction/hover state, the mocked pointer's
    position, and the hovered-entity set.
  - **Device-level input mocks** through the app input crate's real binding
    resolution: `game/gamepad` (the `Gamepad` `analog` map on a synthetic
    entity — the trap that a `digital` write compiles and silently does
    nothing), `game/keyboard` (`ButtonInput<KeyCode>` by exact `KeyCode` serde
    name), `game/mouse` (buttons + real `MouseMotion`/`MouseWheel` **events** —
    the accumulated resources are overwritten every frame — plus
    `bevy_picking`'s `PointerInput` pipeline for position and UI clicks).
- **MCP server.** rmcp Streamable HTTP (stateless) on its own thread; tools are
  thin proxies to BRP over loopback HTTP — all `World` access stays in BRP
  systems. Built-in tools: `client_info`, `game_state`, `ui_tree`, `screenshot`,
  `keyboard_input`, `gamepad_input`, `mouse_input`.
- **Agent guides bundled into the binary** and served via the `read_guide`
  tool (whole document, one `##` section by number/title prefix, or an index):
  the playtesting playbook, the bug-reporting skill, `AGENTS.md` and the
  README — zero-setup onboarding for an agent that only sees the MCP server.
- **Headless rendering support.** `offscreen_size` configures a shared offscreen
  texture that all cameras are retargeted into (with the camera-ordering
  invariants: exactly one clearer, the `IsDefaultUiCamera` holder drawn last,
  and a `Projection::set_changed()` nudge so late-retargeted cameras resolve
  their render target at all); a bootstrap UI camera exists before any content
  camera; and the agent cursor overlay renders the mocked pointer's
  position/hover/press state into captures. `no_render` runs with no
  wgpu/Vulkan at all — UI layout, `game/ui`, hover and clicks still work via a
  `target_info` shim; screenshots return a clean error. Renders are produced
  with **guarded `init_asset` calls** (`init_asset` replaces an existing store
  and divorces it from issued handles — index-out-of-bounds panics) and the
  visibility-propagation plugin added when missing (render-less hosts never get
  it otherwise, leaving every UI node invisible to `game/ui` and picking).
- **Fleet testing.** `--brp-port`/`--mcp-port`/`--no-render` read from the
  process args; a non-default BRP port isolates captures into a per-client
  `screenshots/client-<port>/` directory so concurrent clients don't
  cross-contaminate.
- **Host extension surface.** Custom BRP methods (any plugin, any time —
  `RemoteMethods` is looked up per request) and `extra_tools` MCP tools
  (`HarnessTool`: schemars-generated schemas, argument validation, a
  `BrpClient` loopback handle, errors served as `isError` content).
- **Host-owned headless rendering.** `offscreen_target: Option<Handle<Image>>`
  for hosts whose headless camera machinery also runs without this crate
  compiled in — the harness wraps the host's handle internally and adds only
  the cursor overlay. (Superseded in 0.2.0 by `OffscreenMode`, which carries
  the handle without a public duplicate resource.)
- **Ergonomics.** `from_env` (client-flavored defaults:
  `--brp-port`/`--mcp-port`/`--no-render`), `from_env_with_defaults` (servers
  with their own port conventions), `isolated_screenshots_dir` (the fleet
  isolation rule, public), `register_game_method` (one-line custom method
  registration with a visible warning when `RemoteMethods` is missing),
  `client_info_host` (host mode flags merged under a `"host"` key),
  `clickable` (pluggable clickable convention for `game/ui` — prototype_19's
  bevy_markup `data-on-click` buttons dump `clickable: true`),
  `method_prefix`/`disabled_tools` (a dedicated server serves `server/state`
  and hides the tools that are meaningless without a world render/window),
  and `pub use schemars; pub use serde_json;` re-exports.

[Unreleased]: https://github.com/nchashch/bevy_mcp_harness/compare/v0.3.1...HEAD
[0.3.1]: https://github.com/nchashch/bevy_mcp_harness/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/nchashch/bevy_mcp_harness/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/nchashch/bevy_mcp_harness/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/nchashch/bevy_mcp_harness/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/nchashch/bevy_mcp_harness/releases/tag/v0.1.0
