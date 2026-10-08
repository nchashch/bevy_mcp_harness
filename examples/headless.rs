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
use bevy_mcp_harness::{BevyMcpHarnessPlugin, DEFAULT_OFFSCREEN_SIZE, McpHarnessConfig};

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
            offscreen_size: Some(DEFAULT_OFFSCREEN_SIZE),
            no_render: !render,
            ..McpHarnessConfig::from_env()
        },
    })
    .add_systems(Startup, spawn_ui)
    .add_systems(Update, (observe_mocked_input, exit_after_warmup));
    if !render {
        // `UiPlugin`'s Image-widget sizing systems read `Assets<TextureAtlasLayout>`, which a
        // render-side plugin normally registers; a render-less host must init it by hand.
        app.init_asset::<bevy::image::TextureAtlasLayout>();
    }
    app.run();
}

#[derive(Component)]
struct DemoButton;

fn spawn_ui(mut commands: Commands) {
    // The harness's bootstrap UI camera exists already (headless mode); all we add is one
    // interactive button for `game/ui` to dump and the mocked pointer to hover/click.
    commands.spawn((
        DemoButton,
        Button,
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
    button: Query<&Interaction, With<DemoButton>>,
    hover: Option<Res<bevy::picking::hover::HoverMap>>,
) {
    if frame.0 % 120 != 0 {
        return;
    }
    let interaction = button.iter().next().copied();
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
