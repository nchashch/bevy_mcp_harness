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
//!    and run outside Bevy's world; all `World` access stays in BRP's systems).
//!
//! **Never enable this in player-facing builds**: it is a debug/QA tool surface and BRP is
//! unauthenticated by design — localhost bind only.
//!
//! # Usage
//!
//! ```no_run
//! use bevy::prelude::*;
//! use bevy_mcp_harness::{BevyMcpHarnessPlugin, McpHarnessConfig};
//!
//! App::new()
//!     .add_plugins(BevyMcpHarnessPlugin::default()) // windowed: tool surfaces only
//!     // Headless agent host (no window): render into a 1280×800 offscreen texture instead.
//!     // .add_plugins(BevyMcpHarnessPlugin { config: McpHarnessConfig {
//!     //     offscreen_size: Some(UVec2::new(1280, 800)), ..McpHarnessConfig::from_env() } })
//!     .run();
//! ```

pub mod brp;
pub mod headless;
pub mod mcp;

pub use headless::{HeadlessUiCameraBootstrap, NoRenderMode, OffscreenRenderTarget};

use std::path::PathBuf;
use std::sync::Arc;

use bevy::asset::AssetApp;
use bevy::prelude::*;
use bevy::remote::http::RemoteHttpPlugin;
use bevy::remote::RemotePlugin;

/// The offscreen capture target's default size (Steam Deck 800p) for [`McpHarnessConfig`] users
/// that want the proven headless geometry.
pub const DEFAULT_OFFSCREEN_SIZE: UVec2 = UVec2::new(1280, 800);

/// The MCP surface's default port — NOT 15703, which is `bevy_remote`'s render-subapp BRP port
/// (`DEFAULT_RENDER_PORT`, active whenever `bevy_render` runs): binding the MCP listener there
/// makes the render app's BRP bind fail and the main BRP pipeline hang in release builds.
pub const DEFAULT_MCP_PORT: u16 = 15710;

/// A host-registered `game/state` snapshot: everything an agent needs to reason about *this*
/// game (app state, the player's entity/position/health, whatever matters here). Called with
/// `&mut World` from the BRP handler thread-side systems; return a JSON object. Fused into
/// every `game/screenshot/get` response (and its `.json` sidecar) so a screenshot arrives with
/// its ground-truth state attached.
pub type StateSnapshotFn = Arc<dyn Fn(&mut World) -> serde_json::Value + Send + Sync>;

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
    /// Headless rendering: all cameras are retargeted into a shared offscreen texture of this
    /// size (`None` = windowed — captures come from the primary window). Also spawns the
    /// bootstrap UI camera and the camera-retarget ordering systems, and enables the agent
    /// cursor overlay (on a real desktop the OS cursor is already visible).
    pub offscreen_size: Option<UVec2>,
    /// Marker mode for render-less headless hosts: no wgpu/Vulkan at all. Screenshots return a
    /// clean error instead of waiting on a capture that can never complete, and
    /// [`headless::shim_camera_computed`] feeds each camera's `Camera.computed.target_info` by
    /// hand so UI layout, `game/ui`, hover, and clicks still work.
    pub no_render: bool,
    /// The host's `game/state` snapshot hook — see [`StateSnapshotFn`]. `game/state` returns
    /// an empty object without one.
    pub state_snapshot: Option<StateSnapshotFn>,
}

impl Default for McpHarnessConfig {
    fn default() -> Self {
        let brp_port = bevy::remote::http::DEFAULT_PORT;
        Self {
            brp_port,
            mcp_port: DEFAULT_MCP_PORT,
            screenshots_dir: default_screenshots_dir(brp_port),
            offscreen_size: None,
            no_render: false,
            state_snapshot: None,
        }
    }
}

impl McpHarnessConfig {
    /// Reads the prototype's CLI flags out of the process args: `--brp-port N` (default 15702),
    /// `--mcp-port N` (default 15710), `--no-render`. Everything else is [`Default`]. Fleet
    /// isolation is preserved: a non-default `brp_port` captures into a per-client
    /// `screenshots/client-<port>/` directory.
    pub fn from_env() -> Self {
        let brp_port = port_flag("--brp-port").unwrap_or(bevy::remote::http::DEFAULT_PORT);
        let no_render = std::env::args().any(|arg| arg == "--no-render");
        Self {
            brp_port,
            mcp_port: port_flag("--mcp-port").unwrap_or(DEFAULT_MCP_PORT),
            screenshots_dir: default_screenshots_dir(brp_port),
            no_render,
            ..Default::default()
        }
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
    if brp_port == bevy::remote::http::DEFAULT_PORT {
        base
    } else {
        base.join(format!("client-{brp_port}"))
    }
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

        // Headless rendering: the offscreen texture, the bootstrap UI camera, and the
        // camera-retarget/ordering systems that make renderless-window apps actually produce
        // frames (see `headless.rs`). `init_asset::<Image>` is defensive for hosts that
        // disabled the render plugins entirely (`ImagePlugin` is what normally registers it) —
        // guarded, because `init_asset` REPLACES an existing `Assets` store with a fresh one
        // (divorcing it from handles the server already issued: index-out-of-bounds panics in
        // `handle_internal_asset_events`), it is only for genuinely missing stores.
        if config.offscreen_size.is_some() {
            if !app.world().contains_resource::<Assets<Image>>() {
                app.init_asset::<Image>();
            }
            let size = config.offscreen_size.unwrap();
            let target = {
                let mut images = app.world_mut().resource_mut::<Assets<Image>>();
                OffscreenRenderTarget::new(size.x, size.y, &mut images)
            };
            app.insert_resource(target)
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

        if config.no_render {
            app.insert_resource(NoRenderMode)
                .add_systems(Update, headless::shim_camera_computed);
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
        methods.insert(
            "game/state",
            bevy::remote::RemoteMethodSystemId::Instant(state_method),
        );
        methods.insert(
            "game/screenshot",
            bevy::remote::RemoteMethodSystemId::Instant(screenshot_start),
        );
        methods.insert(
            "game/screenshot/get",
            bevy::remote::RemoteMethodSystemId::Instant(screenshot_get),
        );
        methods.insert(
            "game/gamepad",
            bevy::remote::RemoteMethodSystemId::Instant(gamepad_method_id),
        );
        methods.insert(
            "game/keyboard",
            bevy::remote::RemoteMethodSystemId::Instant(keyboard_method_id),
        );
        methods.insert(
            "game/mouse",
            bevy::remote::RemoteMethodSystemId::Instant(mouse_method_id),
        );
        methods.insert(
            "game/ui",
            bevy::remote::RemoteMethodSystemId::Instant(ui_method),
        );
        methods.insert(
            "game/client_info",
            bevy::remote::RemoteMethodSystemId::Instant(client_info_method),
        );
        methods.insert(
            "game/cameras",
            bevy::remote::RemoteMethodSystemId::Instant(cameras_method),
        );

        mcp::start_mcp_server(config.brp_port, config.mcp_port);
    }
}
