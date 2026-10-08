# Changelog

All notable changes to bevy_mcp_harness are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Before 1.0, a minor version bump (0.2 → 0.3) may contain breaking changes.

The design history behind these changes lives in
[`docs/agents/adr/`](docs/agents/adr/README.md).

## [Unreleased]

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
