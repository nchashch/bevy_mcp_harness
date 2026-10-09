# bevy_mcp_harness

A localhost MCP + BRP tool surface for agent-driven QA playtesting of Bevy apps.

**Dev/QA tooling only — never enable in player-facing builds.** BRP is unauthenticated by
design; both surfaces bind to `127.0.0.1` only.

For an example of this being used in a demo see [this repo](https://github.com/nchashch/p19).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), your choice — the same
terms as Bevy itself.

## Bevy compatibility

| bevy_mcp_harness | bevy |
|---|---|
| 0.4 | 0.20 |
| 0.3 | 0.19.1 |
| 0.2 | 0.19.1 |
| 0.1 | 0.19.1 |

One harness line per bevy minor release. 0.1.x requires bevy **0.19.1 or later 0.19.x** — the
crate reads bevy internals whose shape changed within 0.19 (e.g. `RenderTarget::as_image()`
returning `&Handle<Image>`), so it will not compile against 0.19.0. Harness patch releases
never require a bevy bump.

## Architecture

Three layers:

1. **BRP** (`bevy_remote`): JSON-RPC 2.0 over HTTP on `127.0.0.1:15702`. The built-in methods
   (`world/query`, `world/get_components`, `world/spawn_entity`, …) expose the whole reflected
   ECS.
2. **Custom BRP methods** — the game tools:
   - `game/state` — the host-registered snapshot (see `McpHarnessConfig::state_snapshot`);
     empty object when the host registers no hook.
   - `game/client_info` — mode flags + surface ports; call first on a fresh session.
   - `game/cameras` — every camera: entity id (usable as `game/screenshot`'s `camera` param),
     position, look angles, active flag.
   - `game/screenshot` / `game/screenshot/get` — async capture → full-resolution PNG on disk
     (persistent, human-browsable, with a `.json` state sidecar) → base64 on poll. Optional
     `label`, `crop` `[x,y,w,h]`, and `camera` params; `crop` and `max_dimension` shape only
     the **served view** (the token-efficient image the agent looks at) — the file on disk is
     always the full-resolution, uncropped frame. Pixel-identical polls answer `unchanged:
     true` without re-sending the image. With the `render_debug` feature, a `debug_view` param
     renders rendering internals into the capture (`depth`, `normals`, `motion_vectors`,
     `deferred*`, `depth_pyramid`, `wireframe`; the `physics` collider-gizmo view needs
     `avian3d` and returns a clean error on this line until avian3d supports bevy 0.20 —
     see [ADR 0009](docs/agents/adr/0009-render-debug-views-on-screenshots.md)).
     Every screenshot response embeds an `entities` table: visible `Aabb` entities projected
     into screenshot pixel space (`{entity, name, center, bounding_box, depth}`,
     nearest-first) — see
     [ADR 0008](docs/agents/adr/0008-entity-to-pixel-correlation.md).
   - `game/entities_on_screen` — the same entity-projection table standalone, without a
     capture.
   - `game/ui` — accessibility-tree dump: every laid-out node in back-to-front render order
     with its rect in screenshot pixel space, text, clickability (`bevy_ui` `Interaction`),
     pressed/hovered state, the hovered-entity set, and the mocked pointer's position. Read
     this instead of OCR-ing screenshots.
   - `game/gamepad` — mocks a real gamepad's button/axis state on a synthetic gamepad entity,
     so injected state flows through the app's real binding resolution (dead zones, device
     selection) exactly like a human's controller. Level-triggered; `{"input":"reset"}` to
     release everything.
   - `game/keyboard` — mocks `ButtonInput<KeyCode>` by exact `KeyCode` variant name
     (`{"key":"KeyW","pressed":true}`, or `{"reset":true}`).
   - `game/mouse` — mocks `ButtonInput<MouseButton>` + real `MouseMotion`/`MouseWheel` events,
     and drives `bevy_picking`'s `PointerInput` pipeline for cursor position and UI clicks
     (`button` / `motion` / `move_to` / `wheel` / `reset`). Note: the accumulators can't be
     set directly — real events are injected instead (Bevy's own accumulation systems overwrite
     the resources every frame).
3. **MCP server** (`rmcp`, Streamable HTTP, stateless) on `127.0.0.1:15710/mcp`: tools
   `client_info`, `game_state`, `ui_tree`, `screenshot`, `keyboard_input`, `gamepad_input`,
   `mouse_input`, `input_sequence`, `click_node`, `wait_until`, `game_assert`, `plan_check`,
   `read_guide` — thin proxies to the BRP methods over loopback HTTP. The `screenshot` tool
   accepts `debug_views: ["depth", …]` to capture several render-debug views in one call —
   one tool call returns N image blocks of the same scene moment, with path-matched polling
   so captures can't be attributed to the wrong view
   ([ADR 0010](docs/agents/adr/0010-batched-debug-views.md)). `plan_check` pre-flights
   an intended call sequence against **declared preconditions**
   (`register_game_method_with_precondition`; the built-in `screenshot` declares one too), so
   an agent catches state-machine mistakes — "play before connect", "screenshot with no
   rendering" — before sending anything.
   `read_guide` serves the agent guides bundled into the binary
   (`docs/agents/skills/playtest.md`, `docs/agents/skills/bugreport.md`, `AGENTS.md`,
   `README.md`) — an agent connected to the MCP server can
   read the playtesting playbook with zero setup: `read_guide` with no arguments returns the
   index; `{"guide":"playtest","section":"6"}` returns one section.

Headless support (`McpHarnessConfig::offscreen`): `OffscreenMode::Owned(size)` — every camera
is retargeted into a shared offscreen texture (with the UI-camera ordering invariant
maintained), captures read that texture, and the agent cursor overlay renders the mocked
pointer's position/hover/press state into captures. `OffscreenMode::HostManaged(handle)` is
for hosts with existing headless camera machinery: they keep their own target/resource and the
harness adds only the cursor overlay. `no_render: true` additionally runs with no wgpu/Vulkan
at all — UI layout, `game/ui`, hover, and clicks still work; screenshots return a clean error.

Customization beyond the config fields (`state_snapshot`, `client_info_host` for game-specific
mode flags, `clickable` for non-`Interaction` UI conventions, `extra_tools`, `method_prefix`
for non-game apps, `disabled_tools`, `register_game_method`): see `AGENTS.md` and the crate
docs.

## Cargo features

All off by default — hosts opt into what they need:

| Feature | Pulls | Enables |
|---|---|---|
| `render_debug` | `bevy_dev_tools`, `bevy_core_pipeline`, `bevy_pbr` | the `debug_view` / `debug_views` overlay modes and `wireframe` ([ADR 0009](docs/agents/adr/0009-render-debug-views-on-screenshots.md)) |

The `physics_debug` feature (the `physics` Avian3D collider-gizmo view) is **absent on this
bevy line**: the newest avian3d release requires bevy 0.19, so no avian3d version can compile
against bevy 0.20 — `debug_view: "physics"` returns a clean error instead. The feature
returns when avian3d updates.

The base dependency is `bevy` with `default-features = false` and only the features this
crate's code touches — hosts whose Bevy is minimal (headless dedicated servers) don't inherit
`bevy_winit`/Wayland system deps. Hosts enable the rest of Bevy's features themselves.

## Usage

```toml
[dependencies]
bevy_mcp_harness = { path = "..." }
```

```rust
use bevy::prelude::*;
use bevy_mcp_harness::{BevyMcpHarnessPlugin, McpHarnessConfig};

// Windowed app — tool surfaces only:
App::new().add_plugins(BevyMcpHarnessPlugin::default());

// Headless agent host (compose without winit/render plugins yourself):
App::new()
    .add_plugins(BevyMcpHarnessPlugin {
        config: McpHarnessConfig {
            offscreen_size: Some(bevy_mcp_harness::DEFAULT_OFFSCREEN_SIZE),
            no_render: false,
            ..McpHarnessConfig::from_env() // reads --brp-port N / --mcp-port N / --no-render
        },
    });
```

`examples/headless.rs` is a complete render-less host with one UI button; run it and drive it
with curl:

```text
curl -s http://127.0.0.1:15702 -X POST -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","id":1,"method":"game/ui","params":{}}'
```

## Extending with game-specific tools

Both extension points are available to the host app at any time, before or after the plugin is
added (the crate is a normal cargo dependency — no forking needed):

1. **Custom BRP methods** — attach a system to `bevy::remote::RemoteMethods`; handlers run in
   the main world with `&mut World` access:

   ```rust
   let id = app.register_system(my_game_state_method);
   app.world_mut()
       .resource_mut::<bevy::remote::RemoteMethods>()
       .insert("game/my_state", bevy::remote::RemoteMethodSystemId::Instant(id));
   ```

2. **Custom MCP tools** — pass `HarnessTool`s via `McpHarnessConfig::extra_tools`. Arguments
   deserialize into your own struct (schema generated via schemars); the callback gets a
   `BrpClient` (loopback JSON-RPC to this app's BRP surface — including your custom methods
   from step 1) and returns JSON:

   ```rust
   BevyMcpHarnessPlugin {
       config: McpHarnessConfig {
           extra_tools: vec![HarnessTool::new(
               "my_state",
               "Game-specific state snapshot.",
               |client: BrpClient, args: MyToolArgs| async move {
                   client.call("game/my_state", serde_json::json!({ "detailed": args.detailed }))
                       .await
               },
           )],
           ..Default::default()
       },
   }
   ```

   The host needs `rmcp`, `serde`, `schemars`, and `serde_json` as direct dependencies only
   for the argument-struct derives. `examples/headless.rs` demonstrates both halves end to end
   (`game/demo_button` + the `demo_button` MCP tool).

## Ports

- BRP: 15702 (`bevy::remote::http::DEFAULT_PORT`); `--brp-port N` for fleet testing.
- MCP: 15710 — deliberately NOT 15703, which is `bevy_remote`'s render-subapp BRP port
  (binding the MCP listener there breaks the render app's BRP bind).
- Fleet isolation: a non-default `brp_port` captures into a per-client
  `screenshots/client-<port>/` directory so concurrent clients don't cross-contaminate.
