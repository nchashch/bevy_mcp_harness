# AGENTS.md

Guidance for AI coding agents working in this repository. This file describes the **current state**
of the code only — history and rationale live in the git log and in the load-bearing doc comments
(`src/headless.rs`, `src/brp.rs`). When a change alters behavior described here, update this file
in the same change; do not append changelog entries.

## Rules

- **This crate is a debug/QA tool surface. Never enable it in player-facing builds** and never
  relax the localhost-only bind. BRP is unauthenticated by design.
- **`src/*.rs` doc comments are load-bearing**: they record why (e.g. why `init_asset` calls are
  `contains_resource`-guarded, why real `MouseMotion` events are injected instead of resource
  writes). Keep them true when you change behavior; that is where the next agent looks first.
- **Rust edition 2024**: type positions use `Future`, not `std::future::Future` (prelude).
  Same for other prelude traits — no fully qualified paths where the prelude suffices.
- **Never stub, mock, or "scaffold" a fallback**: this crate's value is that every path actually
  works. If a feature can't be finished, say so instead of shipping a no-op.
- **Verify by running.** A compiling change is not a done change: run
  `examples/headless.rs` and exercise the changed surface over BRP/MCP (recipes under
  "Commands"). The crate has no unit-test suite by design — the example is the fixture.
- **MCP handlers never touch the Bevy `World`.** They run on a separate thread; all `World`
  access lives in BRP handler systems. A new tool that "just reads one component" still goes
  through BRP.

## What this is

`bevy_mcp_harness` — a reusable Bevy 0.19 (Rust, edition 2024) plugin that exposes a localhost
tool surface so LLM agents can inspect state, capture screenshots, and drive input for automated
QA playtesting. Ported from PROTOTYPE_19's `dev/tool_api` (ADR 0009) and generalized: the
prototype-specific gameplay methods (`game/input` action mocks, `game/select`, `game/trigger`,
`game/levels`, `game/select_level`) were dropped — they were bound to that project's crates.
Everything here compiles against plain bevy + rmcp.

Three layers, in one data flow:

1. **BRP** (`bevy_remote`): JSON-RPC 2.0 over HTTP, `127.0.0.1:15702` by default. Built-in
   methods are `world.*` (`world.query`, `world.get_components`, `world.spawn_entity`, …);
   custom methods are `game.*`. `RemoteMethods` is looked up per request, so methods can be
   attached any time.
2. **Custom BRP methods** (`src/brp.rs`): the game tools — `game/state`, `game/client_info`,
   `game/cameras`, `game/screenshot` + `game/screenshot/get`, `game/ui`, `game/gamepad`,
   `game/keyboard`, `game/mouse`, `game/plan_check` (pre-flight for an intended call
   sequence against declared preconditions). Handlers are plain systems registered into
   `RemoteMethods` in `BevyMcpHarnessPlugin::build`.
3. **MCP server** (`src/mcp.rs`): rmcp Streamable HTTP, stateless, on `127.0.0.1:15710/mcp`.
   Tools (`client_info`, `game_state`, `ui_tree`, `screenshot`, `keyboard_input`,
   `gamepad_input`, `mouse_input`, `read_guide`, plus host `extra_tools`) proxy to BRP over
   loopback HTTP via [`BrpClient`]. `read_guide` serves the agent guides bundled into the
   binary at compile time (`include_str!` of `docs/agents/skills/playtest.md`,
   `docs/agents/skills/bugreport.md`,
   `AGENTS.md`, `README.md`) — whole document, one `## ` section by number/title prefix, or an
   index. A pure-MCP agent can read the playbook from the server itself, zero setup.

`src/lib.rs` owns `BevyMcpHarnessPlugin` + `McpHarnessConfig`; `src/headless.rs` owns the
render-less/offscreen support machinery. `examples/headless.rs` is the canonical host
composition and the smoke-test fixture.

## Extension points (host apps)

Both work after the plugin was added as a normal cargo dependency — no forking:

- **Custom BRP methods**: `register_game_method(app, "game/x", system)` — one line per method
  (warns when `RemoteMethods` is missing). `register_game_method_with_precondition` adds a
  declared [`PreconditionFn`] (`fn(&World, params) -> Result<(), String>`) surfaced by the
  `{prefix}/plan_check` pre-flight method + MCP `plan_check` tool, so an agent can verify an
  intended call sequence would pass before sending it (advisory — the method's own checks
  remain the source of truth). Handlers run in the main world with `&mut World` access. Must
  be called where `&mut App` lives (`main` after the plugin, or a later plugin). Gotcha:
  filter the method's queries on the game's own marker components — the harness's cursor
  overlay spawns `ComputedNode`/`UiGlobalTransform` UI nodes that otherwise pollute
  `.single()`.
- **Custom MCP tools**: `McpHarnessConfig::extra_tools: Vec<HarnessTool>`. `HarnessTool::new`
  generates the input schema from the args struct (schemars), deserializes/validates arguments
  before invoking, and hands the callback a `BrpClient`. Conventional shape: the callback
  proxies to the host's own custom BRP method. Returning `Ok(json)` serves pretty-printed text
  content; `Err(msg)` becomes a tool error. The host needs `serde`, `schemars`, `serde_json`
  (and `rmcp` only to name rmcp types) as direct dependencies.

## Commands

```sh
cargo build --examples                 # the lib + examples/headless.rs
cargo run -q --example headless        # render-less host: no wgpu, no window
cargo run -q --example headless -- --render   # + render plugins (real Vulkan, offscreen)
```

The example runs ~10 minutes (60 fps, exits at frame 36000), logs observation lines every 120
frames, and brings up both surfaces. Drive it from another terminal:

```sh
# BRP (plain JSON-RPC, POST any method):
curl -s http://127.0.0.1:15702 -X POST -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"game/ui","params":{}}'

# MCP (Streamable HTTP; stateless mode still mints a session id per connection —
# capture it from the initialize response and send it as Mcp-Session-Id afterwards):
SID=$(curl -si http://127.0.0.1:15710/mcp -X POST -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26",
       "capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
  | grep -i mcp-session-id | tr -d '\r' | awk '{print $2}')
# then notifications/initialized + tools/list / tools/call with -H "Mcp-Session-Id: $SID"
```

Verification checklist when touching a surface: `game/client_info` (mode flags), `game/ui`
(dump lists the example button), `game/keyboard` (`{"key":"KeyW","pressed":true}` then watch the
example's own log lines flip `KeyW pressed=true`), `game/screenshot` (clean error in no_render;
PNG + crop + `unchanged:true` suppression when rendered). MCP: `tools/list` shows 9 tools (8
built-ins + the example's `demo_button`), `tools/call demo_button` round-trips,
`tools/call read_guide {"guide":"playtest","section":"6"}` returns the Screenshots section.

Operational notes for agent-driven sessions: run the example as a supervised background process
and gate on port 15702 listening; **a stale process from a previous run holds the ports** — its
BRP will answer and its MCP bind makes the new run log `AddrInUse` while the BRP ready-check
passes against the *old* process. Kill it before concluding anything.

## Configuration (`McpHarnessConfig`)

- Ports: BRP `bevy::remote::http::DEFAULT_PORT` (15702), MCP `DEFAULT_MCP_PORT` (15710).
  **MCP must never bind 15703** — that is `bevy_remote`'s render-subapp BRP port (binding the
  MCP listener there breaks the render app's BRP bind).
- `--brp-port` / `--mcp-port` / `--no-render` are read by `McpHarnessConfig::from_env`
  (client-flavored defaults) / `from_env_with_defaults(brp, mcp)` (servers with their own
  conventions). Fleet isolation: a non-default `brp_port` captures into `screenshots/
  client-<port>/` — hosts with their own screenshots convention call the public
  `isolated_screenshots_dir(base, brp_port)` to get the same rule.
- `offscreen: OffscreenMode`: `Owned(size)` = the harness owns the full headless stack
  (target + bootstrap UI camera + retarget chain + cursor overlay); `HostManaged(handle)` =
  the host keeps its own target/resource and machinery (for hosts whose headless code also
  runs without this crate compiled in) and the harness reads the handle + adds only the cursor
  overlay; `Windowed` = primary-window captures.
- `no_render: true`: `NoRenderMode` marker (screenshot methods return a clean error). The
  `target_info` shim is added only in `Owned` — a `HostManaged` host feeds its own.
- `state_snapshot` / `client_info_host`: the host's `game/state` payload, and extra mode flags
  merged under `"host"` in `game/client_info` (the harness payload is otherwise closed).
- `clickable: Option<ClickableFn>`: the host's clickable-UI convention for `game/ui` beyond
  `bevy_ui::Interaction` (e.g. an HTML-markup UI's click hooks). `Interaction` holders are
  always clickable.
- `method_prefix` (default `"game"`) renames the BRP methods (`server/state` on a dedicated
  server); `disabled_tools` hides meaningless built-ins from `tools/list` and rejects calls.
- `extra_tools` + `register_game_method(app, "game/x", system)` for game-specific MCP tools
  and BRP methods.

## Headless support: what the plugin adds, and why

All guarded so a full `DefaultPlugins` host is unaffected:

- `Assets<Image>` **only if missing** → then `OffscreenRenderTarget` (Rgba8UnormSrgb +
  `COPY_SRC`, needed by the screenshot readback), the `HeadlessUiCameraBootstrap` camera, and
  the `retarget_cameras_to_offscreen → maintain_default_ui_camera → keep_ui_camera_drawn_last`
  chain in `Update` (see `src/headless.rs` for the ordering invariants: exactly one clearer,
  `IsDefaultUiCamera` holder drawn last, `Projection::set_changed()` to force `target_info`
  recompute for late-retargeted cameras).
- `no_render`: `NoRenderMode` marker + `shim_camera_computed` (feeds `Camera.computed.
  target_info` by hand — `bevy_ui` layout and `bevy_picking`'s UI backend read exactly that).
- **Visibility propagation** (`bevy::camera::visibility::VisibilityPlugin`): normally added by
  the render app (`RenderPlugin → CameraPlugin`); render-less hosts never get it, and without
  it every UI node stays `InheritedVisibility(false)` — `game/ui` sees nothing and picking is
  blind. Added when missing, together with `Assets<Mesh>` /
  `Assets<bevy::mesh::skinning::SkinnedMeshInverseBindposes>` (its bounds systems validate those
  stores unconditionally and panic without them).
- The agent cursor overlay (`brp.rs`): spawns only while an offscreen target exists; recolors
  by hover (`CURSOR_HOVER` = `srgb(1.0, 0.85, 0.1)` = `(255, 217, 26)` in captures — verified
  pixel-level) and press state; `Pickable::IGNORE` so it never occludes the UI it mirrors.

## Hard-won invariants (do not regress)

- **`init_asset::<A>` is not idempotent.** It constructs a *fresh* `Assets<A>` and
  `insert_resource` replaces an existing store — divorcing it from handles the server already
  issued → `index out of bounds` in `bevy_asset`'s `handle_internal_asset_events` (observed as
  len 1, index 5, on rendered hosts). Every `init_asset` here is `contains_resource`-guarded.
  The same applies to the `Mesh`/`SkinnedMeshInverseBindposes` inits.
- **`game/keyboard` needs bevy's `serialize` feature** (this crate enables it): `KeyCode`
  variants deserialize by name through KeyCode's own serde impl.
- **Gamepad mocking writes the `analog` map** (`gamepad.analog_mut().set(...)`), never
  `digital` — input crates (bevy_enhanced_input 0.26 confirmed) read `Gamepad::get`, i.e.
  analog; a `digital_mut().press()` compiles and silently does nothing.
- **Mouse motion/scroll must be injected as real `MouseMotion`/`MouseWheel` events.** Bevy's
  `accumulate_mouse_motion_system` overwrites the `Accumulated*` resources every frame; a
  direct `insert_resource` is wiped before any reader sees it (confirmed live: `dx: 200`
  produced zero look rotation).
- **`bevy_ui::Interaction` does not update for image-target cameras** — upstream
  `ui_focus_system` only considers cameras rendering to a window. On the offscreen target
  `interaction` stays `Idle`; the headless source of truth is picking (`pointer_hovered`,
  `hovered_entities` from `HoverMap`) plus the mocked left button state. Don't "fix" the dump
  to synthesize hover from `HoverMap` into `interaction` without relitigating that decision.
- **Mouse pointer target** = offscreen image if present, else the primary window
  (`WindowRef::Primary.normalize(Some(entity))` — `NormalizedWindowRef` is a private-field
  newtype over `Entity`, not an enum). `move_to` errors when neither exists.
- **`game/screenshot/get` unchanged-suppression** hashes PNG bytes (`DefaultHasher`): identical
  pixels → identical bytes, so a byte hash is a frame hash. `unchanged: true` omits
  `png_base64` on purpose; the agent already holds that image.
- **Camera-targeted captures** (`{"camera": id}`) raise `Camera.order` to 900_000 and restore
  via `CameraCaptureRestore` on `ScreenshotCaptured`; the capture reuses the shared offscreen
  texture — a brand-new image is unknown to the render app and `Screenshot` warns forever.
- **rmcp 3.5 API notes**: `ServerInfo` is a deprecated alias — use `ServerConfig`;
  `#[tool_handler(router = self.router)]` takes an arbitrary expr (that is how `extra_tools`
  join the router); dynamic routes are `ToolRoute::new_dyn` over `ToolCallContext` (public
  `arguments` field); `CallToolResult` → `CallToolResponse` via `Into`.
- **bevy 0.19.1 (vs 0.19.0) API drift** the port hit: `RenderTarget::as_image()` returns
  `&Handle<Image>` (0.19.0 returned `&ImageRenderTarget`); built-in BRP methods are `world.*`
  (not `bevy/*`); `world.query` selects via `data.components` (plural) + `filter.with`;
  `FrameCount`/`FrameCountPlugin` live in `bevy::diagnostic`; `TimePlugin` in `bevy::time`;
  `PipelinedRenderingPlugin` in `bevy::render::pipelined_rendering`;
  `SkinnedMeshInverseBindposes` in `bevy::mesh::skinning`; `NormalizedWindowRef` in
  `bevy::window`; `TextFont::font_size` is a `FontSize` (`Px`/`Vw`/…), not an f32.
- **Rendered headless must disable `PipelinedRenderingPlugin`** (the render app runs a frame
  behind; with `ScheduleRunnerPlugin` driving, no-winit pipelining breaks) — the example shows
  the proven composition (`DefaultPlugins` minus winit + pipelining, `primary_window: None`).

## Known limitations

- `game/ui`'s `interaction` field reads `Idle` for all nodes on an offscreen target (see the
  `Interaction` invariant above). `clickable`, `pointer_hovered`, and `hovered_entities` are
  the reliable headless signals.
- In render-less hosts, `game/screenshot` cannot work by definition — it returns a descriptive
  error; don't "fix" it to return an empty image.
- `game/state` returns `{}` unless the host registers a snapshot hook — the harness cannot know
  game-specific state.
- The example's UI is a single button; text is exercised through the default font only. CJK /
  missing-glyph behavior of the `game/ui` text dump is inherited from bevy_text and untested
  here.
- Screenshots are verified on one real GPU (NVIDIA, Vulkan) and the render-less path; no
  lavapipe/software-Vulkan run has been done in this repo.

## Dependency notes

- `rmcp` 3.5.1 (`server`, `macros`, `schemars`,
  `transport-streamable-http-server` features) runs its own tokio runtime on a dedicated
  `mcp-server` thread — spawned in `start_mcp_server`, two worker threads, never `async` into
  Bevy.
- `reqwest` (rustls, no default features) is the loopback HTTP client for MCP→BRP; keep it
  dependency-light.
- `image` 0.25 (`png` only) is the same line bevy_render pulls for screenshots — capture crops
  (`imageops::crop_imm`) and PNG encoding happen on the host side of the `ScreenshotCaptured`
  observer.
- Host-facing extension API re-exports: `BrpClient`, `HarnessTool` (crate root). The host also
  needs `serde` + `schemars` derives for its args structs — that is the only reason they are
  public-facing.
