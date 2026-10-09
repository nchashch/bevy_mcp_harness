//! Minimal headless usage of `BevyMcpHarnessPlugin`: an agent host (no window) with an
//! offscreen 1280×800 capture target, one clickable UI button, and the full tool surface up on
//! localhost. Default composition is render-less (no wgpu at all); run with `--render` to
//! include the render plugins and exercise the `game/screenshot` capture path on real Vulkan.
//!
//! Run it, then from another terminal:
//!
//! ```text
//! curl -s http://127.0.0.1:15702 -X POST -H 'Content-Type: application/json' \
//!     -d '{"jsonrpc":"2.0","id":1,"method":"game/ui","params":{}}'
//! ```
//!
//! (or `game/keyboard` with `{"key":"KeyW","pressed":true}`, or the MCP surface on
//! `http://127.0.0.1:15710/mcp`). The app logs what its own systems observe every 120 frames,
//! then exits after ~10 minutes (Ctrl-C earlier if you like).
//!
//! **Dev/QA tooling only** — never ship this in a player-facing build.

use std::time::Duration;

use bevy::app::{ScheduleRunnerPlugin, TaskPoolPlugin};
use bevy::diagnostic::{FrameCount, FrameCountPlugin};
use bevy::picking::input::PointerInputPlugin;
use bevy::prelude::*;
use bevy::time::TimePlugin;
use bevy::window::ExitCondition;
use bevy_mcp_harness::{
    BevyMcpHarnessPlugin, BrpClient, DEFAULT_OFFSCREEN_SIZE, HarnessTool, McpHarnessConfig,
};

fn main() {
    // `--render`: include the render plugins (offscreen Vulkan rendering — needs a GPU or
    // lavapipe) so the `game/screenshot` capture path runs for real; default is render-less.
    let render = std::env::args().any(|arg| arg == "--render");
    let mut app = App::new();
    if render {
        // Rendered headless composition (the proven no-window pattern): full `DefaultPlugins`
        // minus winit — `ScheduleRunnerPlugin` drives the frame loop, and every camera renders
        // into the harness's offscreen texture instead of a window.
        app.add_plugins(
            DefaultPlugins
                .build()
                .disable::<bevy::winit::WinitPlugin>()
                // The render app runs one frame behind the main app; with no winit driving
                // frames, that pipeline deadlocks/segfaults (the prototype hit this too).
                .disable::<bevy::render::pipelined_rendering::PipelinedRenderingPlugin>()
                .set(bevy::window::WindowPlugin {
                    primary_window: None,
                    exit_condition: ExitCondition::DontExit,
                    ..default()
                }),
        );
    } else {
        // Render-less composition: no winit, no render app — UI layout, picking, and input
        // mocking are all render-app-free logic.
        app.add_plugins((
            TaskPoolPlugin::default(),
            FrameCountPlugin,
            TimePlugin,
            bevy::log::LogPlugin::default(),
            bevy::window::WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                ..default()
            },
            bevy::asset::AssetPlugin::default(),
            bevy::text::TextPlugin,
            bevy::ui::UiPlugin,
            // The widget-button observers (`ui_widgets::Button` activates on press/click);
            // in the --render branch `DefaultPlugins` provides this group instead.
            bevy::ui_widgets::UiWidgetsPlugins,
            // The widgets' text-input systems read `InputFocus` — the resource comes from
            // this plugin (part of `DefaultPlugins` in the --render branch).
            bevy::input_focus::InputFocusPlugin,
            bevy::input::InputPlugin,
            // Picking core: PointerInputPlugin spawns the mouse pointer entity and consumes the
            // harness's mocked `PointerInput` events; InteractionPlugin maintains the hover map.
            bevy::picking::PickingPlugin,
            PointerInputPlugin,
            bevy::picking::InteractionPlugin,
        ));
    }
    // Frame loop driver for both compositions (`DefaultPlugins` doesn't include it, and the
    // render-less branch deliberately has no winit to drive frames).
    app.add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(
        1.0 / 60.0,
    )))
    .add_plugins(BevyMcpHarnessPlugin {
        config: McpHarnessConfig {
            offscreen: bevy_mcp_harness::OffscreenMode::Owned(DEFAULT_OFFSCREEN_SIZE),
            no_render: !render,
            // Game-specific MCP tool served alongside the harness's built-ins (see
            // `register_game_methods` below for the BRP method it proxies to).
            extra_tools: vec![describe_button_tool()],
            // `game/ui`'s `clickable` convention is pluggable (the default reads the legacy
            // `bevy_ui::Interaction`); this app uses bevy 0.20's widget button, so teach the
            // dump about it here. Hosts on their own widget systems do the same.
            clickable: Some(std::sync::Arc::new(|world, entity| {
                world.get::<bevy::ui_widgets::Button>(entity).is_some()
            })),
            ..McpHarnessConfig::from_env()
        },
    })
    // `bevy::ui_widgets::UiWidgetsPlugins` (the widget-button observers) is part of
    // `DefaultPlugins` already — nothing to add here.
    .add_systems(Startup, spawn_ui)
    .add_systems(Update, (observe_mocked_input, exit_after_warmup));
    // Custom BRP methods attach any time after the harness plugin: the system is registered
    // here, in `main`, and served from the `RemoteMethods` resource from then on.
    register_game_methods(&mut app);
    if !render {
        // `UiPlugin`'s Image-widget sizing systems read `Assets<TextureAtlasLayout>`, which a
        // render-side plugin normally registers; a render-less host must init it by hand.
        app.init_asset::<bevy::image::TextureAtlasLayout>();
    }
    app.run();
}

#[derive(Component)]
struct DemoButton;

// --- Game-specific extension demo -------------------------------------------------------------
// The host app owns its game logic; the harness just serves it. Two halves:
//
// 1. A custom BRP method (`game/demo_button`): a normal system with `&mut World` access,
//    registered into `RemoteMethods` AFTER the harness plugin — any plugin or Startup system
//    can do this, whenever the game's own types are available.
// 2. An MCP tool (`demo_button`) via `McpHarnessConfig::extra_tools`, whose callback proxies
//    to that BRP method through the provided `BrpClient`.

fn register_game_methods(app: &mut App) {
    let demo_button = app.register_system(demo_button_method);
    app.world_mut()
        .resource_mut::<bevy::remote::RemoteMethods>()
        .insert(
            "game/demo_button",
            bevy::remote::RemoteMethodSystemId::Instant(demo_button),
        );
}
/// `game/demo_button` — reads this game's own components straight out of the `World` (impossible
/// from the MCP thread; this is why the tool layer proxies over BRP).
fn demo_button_method(_params: In<Option<serde_json::Value>>, world: &mut World) -> bevy::remote::BrpResult {
    use bevy::remote::BrpError;
    let Ok((entity, node, transform, hovered, pressed)) = world
        .query_filtered::<(
            Entity,
            &ComputedNode,
            &UiGlobalTransform,
            Option<&bevy::picking::hover::Hovered>,
            Option<&bevy::ui::Pressed>,
        ), With<DemoButton>>()
        .single(world)
    else {
        return Err(BrpError::internal("demo button not found (not laid out yet?)"));
    };
    let (_, _, translation) = transform.to_scale_angle_translation();
    let interaction = if pressed.is_some() {
        "Pressed"
    } else if hovered.is_some_and(|hovered| hovered.get()) {
        "Hovered"
    } else {
        "Idle"
    };
    Ok(serde_json::json!({
        "entity": entity,
        "rect": [
            (translation.x - node.size().x / 2.0).round() as i32,
            (translation.y - node.size().y / 2.0).round() as i32,
            node.size().x.round() as i32,
            node.size().y.round() as i32,
        ],
        "interaction": interaction,
    }))
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct DescribeButtonArgs {
    /// Also include the `game/ui` dump for this frame.
    include_ui_dump: bool,
}

/// The MCP-facing half: typed args (schema generated via schemars) + a `BrpClient` callback.
fn describe_button_tool() -> HarnessTool {
    HarnessTool::new(
        "demo_button",
        "This game's demo button: entity id, rect in screenshot pixel space, and interaction state.",
        |client: BrpClient, args: DescribeButtonArgs| async move {
            let mut result = client.call("game/demo_button", serde_json::json!({})).await?;
            if args.include_ui_dump {
                result["ui"] = client.call("game/ui", serde_json::json!({})).await?;
            }
            Ok(result)
        },
    )
}

// ----------------------------------------------------------------------------------------------

fn spawn_ui(mut commands: Commands) {
    // The harness's bootstrap UI camera exists already (headless mode); all we add is one
    // interactive button for `game/ui` to dump and the mocked pointer to hover/click. Bevy
    // 0.20's widget button (`ui_widgets::Button` + `ButtonPlugin` observers, added above) —
    // the deprecated `bevy_ui::Button`/`Interaction` pair can't be constructed anymore.
    commands.spawn((
        DemoButton,
        bevy::ui_widgets::Button,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(560.0),
            top: Val::Px(360.0),
            width: Val::Px(160.0),
            height: Val::Px(80.0),
            ..default()
        },
        BackgroundColor(Color::srgb(0.2, 0.4, 0.8)),
        children![(
            Text::new("Play"),
            TextColor(Color::WHITE),
            TextFont {
                font_size: FontSize::Px(32.0),
                ..default()
            },
        )],
    ));
}

/// What the app's own systems observe about the mocked input — the ground truth the curl
/// round-trips above are checked against.
fn observe_mocked_input(
    frame: Res<FrameCount>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    button: Query<
        (
            Option<&bevy::picking::hover::Hovered>,
            Option<&bevy::ui::Pressed>,
        ),
        With<DemoButton>,
    >,
    hover: Option<Res<bevy::picking::hover::HoverMap>>,
) {
    if !frame.0.is_multiple_of(120) {
        return;
    }
    let interaction = button.iter().next().map(|(hovered, pressed)| {
        if pressed.is_some() {
            "Pressed"
        } else if hovered.is_some_and(|hovered| hovered.get()) {
            "Hovered"
        } else {
            "Idle"
        }
    });
    let hover_entries = hover.as_ref().map(|map| map.0.len()).unwrap_or(0);
    info!(
        "frame {}: KeyW pressed={}, Left pressed={}, button interaction={interaction:?}, hover entries={hover_entries}",
        frame.0,
        keys.pressed(KeyCode::KeyW),
        mouse.pressed(MouseButton::Left),
    );
}

fn exit_after_warmup(frame: Res<FrameCount>, mut exit: MessageWriter<AppExit>) {
    if frame.0 == 36000 {
        info!("smoke run complete, exiting");
        exit.write(AppExit::Success);
    }
}
