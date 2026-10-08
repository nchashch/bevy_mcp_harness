//! The agent/QA tool API — a localhost tool surface on a Bevy app so LLM agents can inspect
//! game state, capture screenshots, and drive input for automated QA/playtesting. Ported from
//! PROTOTYPE_19's `dev/tool_api` (ADR 0009) and generalized into a reusable plugin.
//!
//! Three layers:
//!
//! 1. **BRP** (the Bevy Remote Protocol, `bevy_remote`) as the data layer: JSON-RPC 2.0 over
//!    HTTP on `127.0.0.1:15702`. The built-in methods (`bevy/query`, `bevy/get_components`,
//!    `bevy/list+watch`, `bevy/spawn`, …) expose the whole reflected ECS for free.
//! 2. **Custom BRP methods** as the game tools: `game/state` (an optional host-registered
//!    snapshot hook — agents work better with a small structured view than raw ECS dumps),
//!    `game/screenshot` + `game/screenshot/get` (capture via bevy's `Screenshot` → PNG on disk
//!    → base64 on poll), `game/gamepad` (mocks `bevy_input::gamepad::Gamepad`'s own button/axis
//!    state on a synthetic gamepad entity, so injected state flows through input crates' *real*
//!    binding resolution — dead zones, shared-device ownership, per-context device selection —
//!    exactly like a human's controller), `game/keyboard` (mocks `ButtonInput<KeyCode>`
//!    directly — see [`brp::keyboard_method`]), and `game/mouse` (mocks
//!    `ButtonInput<MouseButton>` plus real `MouseMotion`/`MouseWheel` events, and drives
//!    `bevy_picking`'s own `PointerInput` pipeline for cursor position and UI clicks — see
//!    [`brp::mouse_method`]'s doc comment, including a real gotcha found by testing:
//!    `AccumulatedMouseMotion`/`AccumulatedMouseScroll` can't be set directly, only injected as
//!    events). `game/ui` is the vision aid that pairs with all of this: an accessibility-tree
//!    dump of labeled rects + text in screenshot pixel space, so the model reads rows instead
//!    of OCR-ing pixels.
//! 3. **An in-process MCP server** (`rmcp`, Streamable HTTP on `127.0.0.1:15710`, stateless
//!    mode) whose tools proxy to the BRP methods over loopback HTTP — the MCP layer owns only
//!    the protocol surface (tool listing + schemas), never the `World` (the handlers are async
//!    and run outside Bevy's world; all `World` access stays in BRP's systems). `read_guide`
//!    serves the agent guides bundled into the binary at compile time — an agent connected to
//!    the MCP server can read the playtesting playbook with zero setup.
//!
//! **Never enable this in player-facing builds**: it is a debug/QA tool surface and BRP is
//! unauthenticated by design — localhost bind only.
//!
//! # Extending with game-specific tools
//!
//! Two independent extension points, both available to the host app at any time (before or
//! after the plugin is added, in any plugin's `build`):
//!
//! 1. **Custom BRP methods** — attach a system to the `bevy::remote::RemoteMethods` resource.
//!    Handlers run in the main world with `&mut World` access, so they can read game state
//!    directly:
//!
//!    ```no_run
//!    # use bevy::prelude::*;
//!    # use bevy_mcp_harness::BevyMcpHarnessPlugin;
//!    # #[derive(Component)] struct Health { current: f32 }
//!    # fn my_game_state(world: &mut World) -> bevy::remote::BrpResult {
//!    #     Ok(serde_json::json!({}).into())
//!    # }
//!    fn register_my_methods(app: &mut App) {
//!        let id = app.register_system(my_game_state);
//!        app.world_mut()
//!            .resource_mut::<bevy::remote::RemoteMethods>()
//!            .insert("game/my_state", bevy::remote::RemoteMethodSystemId::Instant(id));
//!    }
//!    ```
//!
//! 2. **Custom MCP tools** — pass [`HarnessTool`]s via [`McpHarnessConfig::extra_tools`]. The
//!    callback runs on the MCP server thread with parsed arguments and a [`BrpClient`]; the
//!    conventional shape proxies to a custom BRP method like the one above:
//!
//!    ```no_run
//!    # use bevy::prelude::*;
//!    # use bevy_mcp_harness::{BevyMcpHarnessPlugin, BrpClient, HarnessTool, McpHarnessConfig};
//!    # #[derive(serde::de::DeserializeOwned, schemars::JsonSchema)]
//!    # struct MyToolArgs { detailed: bool }
//!    # fn make_tool() -> HarnessTool {
//!    HarnessTool::new(
//!        "my_state",
//!        "Game-specific state snapshot.",
//!        |client: BrpClient, args: MyToolArgs| async move {
//!            let mut params = serde_json::json!({});
//!            if args.detailed {
//!                params["detailed"] = serde_json::json!(true);
//!            }
//!            client.call("game/my_state", params).await
//!        },
//!    )
//!    # }
//!    # let tool = make_tool();
//!    # let _ = BevyMcpHarnessPlugin { config: McpHarnessConfig {
//!    #     extra_tools: vec![tool], ..McpHarnessConfig::default() } };
//!    ```
//!
//!    (The host crate needs `rmcp`, `serde`, `schemars`, and `serde_json` in its
//!    `[dependencies]` only for the argument struct derives — the harness re-exports
//!    [`BrpClient`] and [`HarnessTool`] itself.)
//!
//! # Usage
//!
//! ```no_run
//! use bevy::prelude::*;
//! use bevy_mcp_harness::{BevyMcpHarnessPlugin, OffscreenMode};
//!
//! App::new()
//!     .add_plugins(BevyMcpHarnessPlugin::default()) // windowed app: tool surfaces only
//!     // Headless agent host (no window): render into a 1280×800 offscreen texture instead.
//!     // .add_plugins(BevyMcpHarnessPlugin { config: McpHarnessConfig {
//!     //     offscreen: OffscreenMode::Owned(UVec2::new(1280, 800)),
//!     //     ..McpHarnessConfig::from_env() } })
//!     .run();
//! ```

pub mod brp;
pub mod headless;
pub mod mcp;

pub use headless::{HeadlessUiCameraBootstrap, NoRenderMode, OffscreenRenderTarget};
pub use mcp::{BrpClient, HarnessTool};

// Re-exported for hosts: useful for `HarnessTool`-adjacent code without adding a version-matched
// dep. NOTE: `#[derive(JsonSchema)]` still needs the host's own `schemars` dependency — derive
// macros expand to `schemars::` paths (see docs/agents/api-friction.md #6).
pub use schemars;
pub use serde_json;

use std::path::PathBuf;
use std::sync::Arc;

use bevy::asset::AssetApp;
use bevy::ecs::system::{IntoSystem, SystemId};
use bevy::prelude::*;
use bevy::remote::http::RemoteHttpPlugin;
use bevy::remote::{BrpResult, RemotePlugin};

/// The offscreen capture target's default size (Steam Deck 800p) for [`McpHarnessConfig`] users
/// that want the proven headless geometry.
pub const DEFAULT_OFFSCREEN_SIZE: UVec2 = UVec2::new(1280, 800);

/// The MCP surface's default port — NOT 15703, which is `bevy_remote`'s render-subapp BRP port
/// (`DEFAULT_RENDER_PORT`, active whenever `bevy_render` runs): binding the MCP listener there
/// makes the render app's BRP bind fail and the main BRP pipeline hang in release builds.
pub const DEFAULT_MCP_PORT: u16 = 15710;

/// The host-registered `game/state` snapshot: everything an agent needs to reason about *this*
/// game (app state, the player's entity/position/health, whatever matters here). Called with
/// `&mut World` from the BRP handler thread-side systems; return a JSON object. Fused into
/// every `game/screenshot/get` response (and its `.json` sidecar) so a screenshot arrives with
/// its ground-truth state attached.
pub type StateSnapshotFn = Arc<dyn Fn(&mut World) -> serde_json::Value + Send + Sync>;

/// The host's clickable-UI convention for `game/ui`: return `true` when `entity` is a
/// button/interactive node under an interaction convention the harness can't know (an
/// HTML-markup UI's click hooks, say). `bevy_ui::Interaction` holders are always reported
/// clickable; this hook adds to them.
pub type ClickableFn = Arc<dyn Fn(&World, Entity) -> bool + Send + Sync>;

/// Harness configuration, inserted as a resource at plugin build. `Default` is the plain
/// windowed setup (tool surfaces on the default ports, screenshots into
/// `<cwd>/mcp_harness/screenshots/`); [`McpHarnessConfig::from_env`] applies the same CLI flags
/// the prototype used (`--brp-port N`, `--mcp-port N`, `--no-render`).
#[derive(Resource, Clone)]
pub struct McpHarnessConfig {
    /// The BRP surface's port (default 15702 = `bevy::remote::http::DEFAULT_PORT`;
    /// `--brp-port N` for fleet testing — several clients on one machine, each at its own port).
    pub brp_port: u16,
    /// The MCP surface's port (default [`DEFAULT_MCP_PORT`]; `--mcp-port N` for fleet testing).
    pub mcp_port: u16,
    /// Where captures land — persistent, NOT consumed on read, so a human can browse
    /// everything the agent saw. A non-default `brp_port` should get a per-client subdirectory
    /// (`Default` does this): a shared directory would cross-contaminate clients — client A's
    /// `game/screenshot/get` poll would return client B's capture.
    pub screenshots_dir: PathBuf,
    /// Headless rendering mode (see [`OffscreenMode`]). `Windowed` = captures come from the
    /// primary window; no camera machinery, no cursor overlay.
    pub offscreen: OffscreenMode,
    /// Marker mode for render-less headless hosts: no wgpu/Vulkan at all. Screenshots return a
    /// clean error instead of waiting on a capture that can never complete, and
    /// [`headless::shim_camera_computed`] feeds each camera's `Camera.computed.target_info` by
    /// hand so UI layout, `game/ui`, hover, and clicks still work. In [`OffscreenMode::
    /// HostManaged`] the harness adds the marker but not the shim — the host owns
    /// `target_info` feeding (it owns the cameras).
    pub no_render: bool,
    /// The host's `game/state` snapshot hook — see [`StateSnapshotFn`]. `game/state` returns
    /// an empty object without one.
    pub state_snapshot: Option<StateSnapshotFn>,
    /// Optional host extension to the `game/client_info` payload, merged under a `"host"` key
    /// — game-specific mode flags (vr, headless_render, …) that the harness's generic payload
    /// can't know.
    pub client_info_host: Option<StateSnapshotFn>,
    /// Optional host hook deciding whether a UI node is clickable, for interaction conventions
    /// the harness can't know (e.g. an HTML-markup UI whose buttons carry `data-on-click`
    /// signals instead of `bevy_ui::Interaction`). Called per UI-stack entity when serving
    /// `game/ui`; a `true` return adds the `clickable` flag and subtree-text aggregation.
    /// `None` = the bevy_ui `Interaction` convention only.
    pub clickable: Option<ClickableFn>,
    /// Game-specific MCP tools served alongside the built-ins — see [`HarnessTool`]. The
    /// usual shape: a custom BRP method registered by the host (any plugin, any time) plus an
    /// `extra_tools` entry whose callback proxies to it through the provided [`BrpClient`].
    pub extra_tools: Vec<HarnessTool>,
    /// The BRP method name prefix (default `"game"` → `game/state`, `game/ui`, …). A
    /// non-game app — a dedicated server, say — typically uses `"server"` → `server/state`.
    pub method_prefix: String,
    /// MCP tool names to hide from `tools/list` and reject on call — for hosts where some
    /// built-ins are meaningless (a headless dedicated server hides `screenshot`, `ui_tree`,
    /// and the input mocks).
    pub disabled_tools: Vec<String>,
}

/// The headless rendering mode — see [`McpHarnessConfig::offscreen`].
#[derive(Clone, Debug, Default)]
pub enum OffscreenMode {
    /// Captures come from the primary window; no offscreen machinery, no cursor overlay.
    #[default]
    Windowed,
    /// The host owns the whole headless stack: it creates its own offscreen texture (and keeps
    /// its own resource/type for it — this mode exists for hosts whose headless machinery also
    /// runs without this crate compiled in), hands the handle here, and owns camera
    /// retargeting, the bootstrap UI camera, clear/order management, and (with `no_render`)
    /// the `target_info` shim. The harness stores the handle internally (screenshots, the
    /// `game/ui` camera filter, the mocked pointer, the cursor overlay) and inserts no public
    /// resource of its own.
    HostManaged(Handle<Image>),
    /// The harness owns the full headless stack: it creates the offscreen target at this size,
    /// spawns the bootstrap UI camera, and adds the retarget/clear-order chain. The
    /// standalone-headless case.
    Owned(UVec2),
}

impl Default for McpHarnessConfig {
    fn default() -> Self {
        let brp_port = bevy::remote::http::DEFAULT_PORT;
        Self {
            brp_port,
            mcp_port: DEFAULT_MCP_PORT,
            screenshots_dir: default_screenshots_dir(brp_port),
            offscreen: OffscreenMode::Windowed,
            no_render: false,
            state_snapshot: None,
            client_info_host: None,
            clickable: None,
            extra_tools: Vec::new(),
            method_prefix: "game".to_owned(),
            disabled_tools: Vec::new(),
        }
    }
}

impl McpHarnessConfig {
    /// Reads the CLI flags out of the process args: `--brp-port N`, `--mcp-port N`,
    /// `--no-render`, with **client-flavored defaults** (BRP 15702, MCP 15710). Server
    /// binaries with their own port conventions use
    /// [`from_env_with_defaults`](Self::from_env_with_defaults) instead. Fleet isolation is
    /// preserved: a non-default `brp_port` captures into a per-client
    /// `screenshots/client-<port>/` directory.
    pub fn from_env() -> Self {
        Self::from_env_with_defaults(bevy::remote::http::DEFAULT_PORT, DEFAULT_MCP_PORT)
    }

    /// [`from_env`](Self::from_env) with caller-chosen default ports — for apps whose port
    /// conventions differ from a windowed client's (a dedicated server, say).
    pub fn from_env_with_defaults(brp_default: u16, mcp_default: u16) -> Self {
        let brp_port = port_flag("--brp-port").unwrap_or(brp_default);
        let no_render = std::env::args().any(|arg| arg == "--no-render");
        Self {
            brp_port,
            mcp_port: port_flag("--mcp-port").unwrap_or(mcp_default),
            screenshots_dir: default_screenshots_dir(brp_port),
            no_render,
            ..Default::default()
        }
    }
}

/// Appends the fleet-testing isolation rule to a screenshots base directory: a non-default
/// `brp_port` captures into a per-client `screenshots/client-<port>/` subdirectory instead of
/// the base (a shared directory would cross-contaminate `game/screenshot/get` polls between
/// concurrently running clients). Hosts that keep their own screenshots convention use this
/// with their base directory so they get the isolation rule for free.
pub fn isolated_screenshots_dir(base: PathBuf, brp_port: u16) -> PathBuf {
    if brp_port == bevy::remote::http::DEFAULT_PORT {
        base
    } else {
        base.join(format!("client-{brp_port}"))
    }
}

/// Reads `<flag> N` out of the process args. `None` when absent or malformed.
fn port_flag(flag: &str) -> Option<u16> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|i| args.get(i + 1))
        .and_then(|value| value.parse().ok())
}

fn default_screenshots_dir(brp_port: u16) -> PathBuf {
    let base = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("mcp_harness/screenshots");
    isolated_screenshots_dir(base, brp_port)
}

/// Registers a custom BRP method (the harness's own `game/*` methods are registered the same
/// way): `system` runs in the main world with `&mut World` access whenever an agent calls
/// `<name>` over BRP. Must be called **after** [`BevyMcpHarnessPlugin`] (or any other
/// `RemotePlugin` adder) so the `RemoteMethods` resource exists — from `main`, or from a later
/// plugin. Returns the registered system's id.
///
/// ```no_run
/// # use bevy::prelude::*;
/// # use bevy_mcp_harness::register_game_method;
/// # fn my_method(_: In<Option<serde_json::Value>>, world: &mut World) -> bevy::remote::BrpResult {
/// #     Ok(serde_json::json!({}).into())
/// # }
/// # let mut app = App::new();
/// register_game_method(&mut app, "game/my_state", my_method);
/// ```
pub fn register_game_method<S, M>(
    app: &mut App,
    name: impl Into<String>,
    system: S,
) -> SystemId<In<Option<serde_json::Value>>, BrpResult>
where
    S: IntoSystem<In<Option<serde_json::Value>>, BrpResult, M> + Send + Sync + 'static,
{
    let name = name.into();
    let id = app.register_system(system);
    match app.world_mut().get_resource_mut::<bevy::remote::RemoteMethods>() {
        Some(mut methods) => {
            methods.insert(name, bevy::remote::RemoteMethodSystemId::Instant(id));
        }
        None => error!(
            "register_game_method({name}): no RemoteMethods resource — add BevyMcpHarnessPlugin (or RemotePlugin) first"
        ),
    }
    id
}

/// The MCP + Agent playtesting harness. Adds the BRP server (unless the host already did),
/// registers the `game/*` custom methods, runs the in-process MCP server on a background
/// thread, and — configured for headless use — owns the offscreen render target machinery.
pub struct BevyMcpHarnessPlugin {
    pub config: McpHarnessConfig,
}

impl Default for BevyMcpHarnessPlugin {
    fn default() -> Self {
        Self {
            config: McpHarnessConfig::default(),
        }
    }
}

impl Plugin for BevyMcpHarnessPlugin {
    fn build(&self, app: &mut App) {
        let config = self.config.clone();

        // BRP: this plugin owns the BRP server by default, but tolerates a host that added
        // `RemotePlugin` itself (custom methods, a different HTTP port, …) — each half is
        // added only when missing. Custom methods attach post-build via the `RemoteMethods`
        // resource (the plugins' `with_method_main` only works at construction).
        if !app.is_plugin_added::<RemotePlugin>() {
            app.add_plugins(RemotePlugin::default());
        }
        if !app.is_plugin_added::<RemoteHttpPlugin>() {
            app.add_plugins(RemoteHttpPlugin::default().with_port(config.brp_port));
        }

        // Headless rendering, per `OffscreenMode`:
        // - `HostManaged`: the host created its own offscreen texture and inserted it as an
        //   `OffscreenRenderTarget` resource, and owns camera retargeting/bootstrap/clear-order
        //   (+ the `target_info` shim under `no_render`). The harness seeds its internal
        //   capture-target view and adds only the cursor overlay.
        // - `Owned(size)`: the harness owns the full headless stack (see `headless.rs`).
        // `init_asset::<Image>` in the owned path is defensive for hosts that disabled the
        // render plugins entirely (`ImagePlugin` is what normally registers it) — guarded,
        // because `init_asset` REPLACES an existing `Assets` store with a fresh one (divorcing
        // it from handles the server already issued: index-out-of-bounds panics in
        // `handle_internal_asset_events`), it is only for genuinely missing stores.
        match config.offscreen.clone() {
            OffscreenMode::HostManaged(handle) => {
                app.insert_resource(headless::CaptureTarget(handle))
                    // The agent cursor overlay (spawns only while an offscreen target
                    // exists).
                    .add_systems(
                        Update,
                        (brp::spawn_agent_cursor_if_headless, brp::update_agent_cursor),
                    );
            }
            OffscreenMode::Owned(size) => {
                if !app.world().contains_resource::<Assets<Image>>() {
                    app.init_asset::<Image>();
                }
                let target = {
                    let mut images = app.world_mut().resource_mut::<Assets<Image>>();
                    OffscreenRenderTarget::new(size.x, size.y, &mut images)
                };
                app.insert_resource(headless::CaptureTarget::from_offscreen(&target))
                    .insert_resource(target)
                    // A camera for UI that exists before any content camera does — otherwise
                    // bevy_ui has nothing to render onto until the app's own cameras arrive.
                    // `HeadlessUiCameraBootstrap` marks it so `maintain_default_ui_camera` can hand
                    // the `IsDefaultUiCamera` marker back and forth between it and later cameras
                    // instead of letting both hold it at once.
                    .add_systems(
                        Startup,
                        |mut commands: Commands| {
                            commands.spawn((Camera2d, IsDefaultUiCamera, HeadlessUiCameraBootstrap));
                        },
                    )
                    .add_systems(
                        Update,
                        (
                            headless::retarget_cameras_to_offscreen,
                            headless::maintain_default_ui_camera,
                            headless::keep_ui_camera_drawn_last,
                        )
                            .chain(),
                    )
                    // The agent cursor overlay (spawns only while an offscreen target exists).
                    .add_systems(
                        Update,
                        (brp::spawn_agent_cursor_if_headless, brp::update_agent_cursor),
                    );
            }
            OffscreenMode::Windowed => {}
        }

        if config.no_render {
            app.insert_resource(NoRenderMode);
            // The `target_info` shim only when the harness owns the cameras — a HostManaged
            // host feeds its own (p19 does, because its headless machinery also runs without
            // this crate compiled in).
            if matches!(config.offscreen, OffscreenMode::Owned(_)) {
                app.add_systems(Update, headless::shim_camera_computed);
            }
        }

        // Visibility propagation normally arrives via the render app (`RenderPlugin` →
        // `CameraPlugin` → `VisibilityPlugin`); render-less hosts have to add it themselves or
        // every UI node stays `InheritedVisibility(false)` and `game/ui` (and picking) sees
        // nothing. Skipped when a full plugin group already added it. Its mesh-bounds systems
        // validate `Assets<Mesh>`/`Assets<SkinnedMeshInverseBindposes>` unconditionally — those
        // stores are normally registered render-side, so init only the missing ones (guarded:
        // `init_asset` replaces an existing store — see the offscreen branch above).
        if !app.world().contains_resource::<Assets<Mesh>>() {
            app.init_asset::<Mesh>();
        }
        if !app
            .world()
            .contains_resource::<Assets<bevy::mesh::skinning::SkinnedMeshInverseBindposes>>()
        {
            app.init_asset::<bevy::mesh::skinning::SkinnedMeshInverseBindposes>();
        }
        if !app.is_plugin_added::<bevy::camera::visibility::VisibilityPlugin>() {
            app.add_plugins(bevy::camera::visibility::VisibilityPlugin);
        }

        app.insert_resource(config.clone());
        app.init_resource::<brp::LastServedCapture>();

        let state_method = app.register_system(brp::game_state_method);
        let screenshot_start = app.register_system(brp::screenshot_start_method);
        let screenshot_get = app.register_system(brp::screenshot_get_method);
        let gamepad_method_id = app.register_system(brp::gamepad_method);
        let keyboard_method_id = app.register_system(brp::keyboard_method);
        let mouse_method_id = app.register_system(brp::mouse_method);
        let ui_method = app.register_system(brp::ui_dump_method);
        let client_info_method = app.register_system(brp::client_info_method);
        let cameras_method = app.register_system(brp::cameras_method);
        let mut methods = app
            .world_mut()
            .resource_mut::<bevy::remote::RemoteMethods>();
        let instant = bevy::remote::RemoteMethodSystemId::Instant;
        let prefix = config.method_prefix.clone();
        methods.insert(format!("{prefix}/state"), instant(state_method));
        methods.insert(format!("{prefix}/screenshot"), instant(screenshot_start));
        methods.insert(
            format!("{prefix}/screenshot/get"),
            instant(screenshot_get),
        );
        methods.insert(format!("{prefix}/gamepad"), instant(gamepad_method_id));
        methods.insert(format!("{prefix}/keyboard"), instant(keyboard_method_id));
        methods.insert(format!("{prefix}/mouse"), instant(mouse_method_id));
        methods.insert(format!("{prefix}/ui"), instant(ui_method));
        methods.insert(format!("{prefix}/client_info"), instant(client_info_method));
        methods.insert(format!("{prefix}/cameras"), instant(cameras_method));

        mcp::start_mcp_server(
            config.brp_port,
            config.mcp_port,
            config.method_prefix.clone(),
            config.disabled_tools.clone(),
            config.extra_tools.clone(),
        );
    }
}
