//! The in-process MCP server (rmcp, Streamable HTTP, stateless) — the protocol surface whose
//! tools proxy to the BRP methods in [`crate::brp`] over loopback HTTP. The MCP layer owns only
//! the protocol surface (tool listing + schemas), never the `World` (the handlers are async and
//! run outside Bevy's world; all `World` access stays in BRP's systems).

use bevy::log::{error, info};
use rmcp::handler::server::router::tool::{ToolRoute, ToolRouter};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData, ServerConfig, Tool};
use serde_json::{json, Value};
use std::sync::Arc;

// The agent-facing guides, compiled into the binary (include_str = compile-time, so they ship
// with `cargo add bevy_mcp_harness` and are readable from the MCP server with zero setup — no
// repo checkout, no CWD assumptions). Paths are relative to this source file.
const PLAYTEST_GUIDE: &str = include_str!("../docs/agents/skills/playtest.md");
const BUGREPORT_GUIDE: &str = include_str!("../docs/agents/skills/bugreport.md");
const AGENTS_DOC: &str = include_str!("../AGENTS.md");
const README_DOC: &str = include_str!("../README.md");

/// (name, document) pairs served by the `read_guide` tool.
const GUIDES: &[(&str, &str)] = &[
    ("playtest", PLAYTEST_GUIDE),
    ("bugreport", BUGREPORT_GUIDE),
    ("agents", AGENTS_DOC),
    ("readme", README_DOC),
];

/// The `read_guide` tool's parameters.
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct ReadGuideParams {
    /// Which guide to read: `playtest` (the headless playtesting playbook), `bugreport` (the
    /// bug-reporting skill), `agents` (AGENTS.md — harness architecture, invariants, gotchas),
    /// or `readme` (README.md — usage). Omit (or pass `list`) to get an index of the guides
    /// with their section headings.
    pub guide: Option<String>,
    /// Optional: read only one `## ` section of the guide, matched by number or title prefix
    /// (e.g. `"6"` → "6. Screenshots", `"4b"`, `"reporting"`, `"prototype_19"`). First match
    /// wins; omit for the whole document.
    pub section: Option<String>,
}

/// Splits a Markdown document into `(title, body)` chunks at `## ` headings (one per `##`
/// section, `###` subsections included in their parent).
fn markdown_sections(doc: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current: Option<(String, usize)> = None;
    let line_count = doc.lines().count();
    for (idx, line) in doc.lines().enumerate() {
        if let Some(title) = line.strip_prefix("## ").map(str::trim) {
            if let Some((started, start)) = current.take() {
                out.push((started, doc_lines_range(doc, start, idx)));
            }
            current = Some((title.to_owned(), idx));
        }
    }
    if let Some((started, start)) = current {
        out.push((started, doc_lines_range(doc, start, line_count)));
    }
    out
}

/// `doc.lines()`-based `[start, end)` slice (end exclusive), joined back with newlines.
fn doc_lines_range(doc: &str, start: usize, end: usize) -> String {
    doc.lines()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `read_guide` tool's body — shared by the whole-document, section, and index paths.
fn read_guide_body(guide: Option<String>, section: Option<String>) -> Result<String, ErrorData> {
    match guide.as_deref().map(str::trim) {
        None | Some("") | Some("list") => {
            let mut index = String::from("# Bundled guides\n\nRead with read_guide(guide, [section]).\n");
            for (name, doc) in GUIDES {
                index.push_str(&format!("\n## `{name}` — {} bytes\n", doc.len()));
                for (title, _) in markdown_sections(doc) {
                    index.push_str(&format!("- {title}\n"));
                }
            }
            Ok(index)
        }
        Some(name) => {
            let Some((_, doc)) = GUIDES.iter().find(|(n, _)| *n == name) else {
                let names: Vec<&str> = GUIDES.iter().map(|(n, _)| *n).collect();
                return Err(ErrorData::invalid_params(
                    format!("unknown guide {name:?} — available: {}", names.join(", ")),
                    None,
                ));
            };
            let Some(query) = section.as_deref().map(str::trim).filter(|q| !q.is_empty()) else {
                return Ok((*doc).to_owned());
            };
            let q = query.to_lowercase();
            let sections = markdown_sections(doc);
            let found = sections.iter().find(|(title, _)| {
                let title = title.to_lowercase();
                let unnumbered = title
                    .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ' ')
                    .trim()
                    .to_lowercase();
                title.starts_with(&q) || unnumbered.starts_with(&q)
            });
            match found {
                Some((_, body)) => Ok((*body).to_owned()),
                None => {
                    let titles: Vec<&str> =
                        sections.iter().map(|(title, _)| title.as_str()).collect();
                    Err(ErrorData::invalid_params(
                        format!(
                            "no section matching {query:?} in {name:?} — sections: {}",
                            titles.join(" | ")
                        ),
                        None,
                    ))
                }
            }
        }
    }
}

/// The `plan_check` tool's parameters.
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct PlanCheckParams {
    /// The calls to pre-flight check: BRP method names (`"game/trigger"`) or objects
    /// `{"method": "game/input", "params": {...}}` for methods whose precondition reads
    /// params. Full method names, including the prefix.
    pub calls: Vec<serde_json::Value>,
}

/// The `ui_tree` tool's parameters — all optional.
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct UiTreeParams {
    /// Only dump interactive rows (the "find the button" read).
    pub clickable_only: Option<bool>,
    /// Only rows whose text contains this substring (case-insensitive).
    pub text_contains: Option<String>,
    /// Force a full dump even when the filtered node list is identical to the previous read
    /// (the default re-read answers `{unchanged: true}` with the nodes omitted).
    pub refresh: Option<bool>,
}

/// The `wait_until` tool's parameters. Conditions: `path` + `equals` (a dot-separated key path
/// into the polled payload, compared to an expected value), or — for `game/ui` —
/// `text_contains` (any node's text matching, case-insensitive). At least one condition is
/// required.
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct WaitUntilParams {
    /// What to poll: `game_state` (the app's state snapshot) or `game_ui` (the UI dump).
    pub source: String,
    /// Dot-separated key path into the polled payload (e.g. `game_state`, `position.x`,
    /// `host.mcp`). Required for `equals`.
    pub path: Option<String>,
    /// Wait until the value at `path` equals this (JSON equality; numbers compare
    /// numerically).
    pub equals: Option<serde_json::Value>,
    /// `game_ui` only: wait until any node's text contains this (case-insensitive). The UI
    /// dump is filtered server-side, so the returned `last` payload is already small.
    pub text_contains: Option<String>,
    /// Poll interval. Default 200, clamped 50..=2000 (ms).
    pub interval_ms: Option<u64>,
    /// Give up after this long, returning `met: false` plus the last payload. Default 10000,
    /// clamped 100..=60000 (ms).
    pub timeout_ms: Option<u64>,
}

enum WaitCondition {
    UiText(String),
    PathEquals(Option<String>, serde_json::Value),
}

/// Extracts a dot-separated key path (numeric segments index arrays) from a JSON payload.
fn json_path<'a>(payload: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = payload;
    for segment in path.split('.') {
        match current {
            serde_json::Value::Object(map) => current = map.get(segment)?,
            serde_json::Value::Array(items) => {
                current = items.get(segment.parse::<usize>().ok()?)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

/// JSON equality with numeric leniency: `100 == 100.0`.
fn values_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    if let (Some(x), Some(y)) = (a.as_f64(), b.as_f64()) {
        return (x - y).abs() < f64::EPSILON || x == y;
    }
    a == b
}

/// The `click_node` tool's parameters — exactly one selector.
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct ClickNodeParams {
    /// Case-insensitive substring of the node's text (buttons aggregate their label, so
    /// `"Connect"` matches the Connect button's row).
    pub text_contains: Option<String>,
    /// The node's entity id, as `game/ui` dumps it.
    pub entity: Option<u64>,
    /// The node's exact `[x, y, w, h]` rect, as a previous `game/ui` dump reported it.
    pub rect: Option<Vec<f64>>,
    /// Milliseconds between move→press→release so bevy's per-frame hover/click processing
    /// sees each step. Default 50, clamped 0..=1000.
    pub step_delay_ms: Option<u64>,
}

/// The `input_sequence` tool's parameters.
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct InputSequenceParams {
    /// Steps: a device-mock call (`{"keyboard": {…}}`, `{"gamepad": {…}}`, `{"mouse": {…}}` —
    /// the inner object is that mock's full params) or a wait (`{"ticks": 60}` — 60 frames ≈
    /// 1 s at 60 Hz). Max 100 steps; total wait budget 3600 ticks.
    pub steps: Vec<serde_json::Value>,
    /// Milliseconds per tick for `ticks` steps. Default 16.7 (60 Hz).
    pub tick_ms: Option<f64>,
}

/// The `game_assert` tool's parameters.
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct AssertParams {
    /// Expectations, each `{"source": "game_state"|"game_ui", "path": "dot.separated.key",
    /// "op": "eq"|"ne"|"gt"|"gte"|"lt"|"lte"|"exists"|"text_contains", "value": …}`.
    /// `text_contains` (game_ui) checks any node's text, case-insensitive; numbers compare
    /// numerically.
    pub expectations: Vec<serde_json::Value>,
}

/// The display text of every node in a UI dump payload.
fn node_texts(payload: &serde_json::Value) -> impl Iterator<Item = &str> {
    payload
        .get("nodes")
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|node| node.get("text").and_then(Value::as_str))
        })
        .into_iter()
        .flatten()
}

/// A loopback HTTP client to the app's BRP surface — the handle handed to every
/// [`HarnessTool`] callback. Custom tools drive the app the same way the built-in ones do:
/// JSON-RPC methods over `127.0.0.1:<brp_port>` (built-ins like `bevy/query` work, and so do
/// custom methods the host registered — see the crate docs' "Extending" section).
#[derive(Clone)]
pub struct BrpClient {
    url: String,
}

impl BrpClient {
    fn new(brp_port: u16) -> Self {
        Self {
            url: format!("http://127.0.0.1:{brp_port}"),
        }
    }

    /// One BRP JSON-RPC round-trip; unwraps the envelope into the `result` payload.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let response = reqwest::Client::new()
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|err| format!("brp unreachable: {err}"))?;
        let envelope: serde_json::Value = response
            .json()
            .await
            .map_err(|err| format!("brp bad response: {err}"))?;
        if let Some(error) = envelope.get("error") {
            return Err(format!("brp error from {method}: {error}"));
        }
        Ok(envelope
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }
}

type ToolFuture =
    std::pin::Pin<Box<dyn Future<Output = Result<serde_json::Value, String>> + Send>>;
type ToolCallback = Arc<dyn Fn(BrpClient, serde_json::Value) -> ToolFuture + Send + Sync>;

/// A host-supplied MCP tool, registered via [`crate::McpHarnessConfig::extra_tools`] and served
/// by the harness's MCP server alongside the built-ins. The callback receives parsed arguments
/// (schema generated from `P` via schemars) and a [`BrpClient`]; return the tool's result as
/// JSON (served as pretty-printed text content) or an `Err` message (surfaced as a tool error).
///
/// The host never touches rmcp types: everything (schema, dispatch, error mapping) is handled
/// here.
#[derive(Clone)]
pub struct HarnessTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
    call: ToolCallback,
}

impl HarnessTool {
    /// Defines a tool whose arguments deserialize into `P` (its JSON schema is generated
    /// automatically). `call` runs on the MCP server thread — never touch the Bevy `World`
    /// from it; drive the app through the [`BrpClient`] instead.
    pub fn new<P, F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        call: F,
    ) -> Self
    where
        P: serde::de::DeserializeOwned + schemars::JsonSchema,
        F: Fn(BrpClient, P) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, String>> + Send + 'static,
    {
        let input_schema = serde_json::to_value(schemars::schema_for!(P))
            .unwrap_or_else(|_| json!({"type": "object"}));
        let call: ToolCallback = Arc::new(move |client, args| {
            match serde_json::from_value::<P>(args) {
                Ok(parsed) => Box::pin(call(client, parsed)),
                Err(err) => Box::pin(async move { Err(format!("invalid arguments: {err}")) }),
            }
        });
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            call,
        }
    }

    /// Wraps the tool as an rmcp route on the harness's server struct. The callback gets a
    /// clone of the loopback client and the raw JSON arguments (already schema-validated by
    /// `HarnessTool::new`'s deserialization).
    fn into_route(self, client: BrpClient) -> ToolRoute<GameTools> {
        let input_schema = match self.input_schema {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        let call = self.call;
        ToolRoute::new_dyn(
            Tool::new(self.name, self.description, Arc::new(input_schema)),
            move |context| {
                let client = client.clone();
                let call = call.clone();
                let args = serde_json::Value::Object(context.arguments.unwrap_or_default());
                Box::pin(async move {
                    match call(client, args).await {
                        Ok(value) => {
                            let text = serde_json::to_string_pretty(&value)
                                .unwrap_or_else(|_| value.to_string());
                            Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
                        }
                        // `isError: true` content, not a protocol-level error: an expected
                        // failure ("no local player connected") is a normal tool outcome the
                        // agent reads and reacts to, not a server fault.
                        Err(message) => Ok(CallToolResult::error(vec![ContentBlock::text(message)]).into()),
                    }
                })
            },
        )
    }
}

/// Binds the MCP surface. The MCP port defaults to [`crate::DEFAULT_MCP_PORT`] — NOT 15703,
/// which is `bevy_remote`'s render-subapp BRP port (`DEFAULT_RENDER_PORT`, active whenever
/// `bevy_render` runs): binding our MCP listener there made the render app's BRP bind fail and
/// the main BRP pipeline hang in release builds.
pub fn start_mcp_server(
    brp_port: u16,
    mcp_port: u16,
    method_prefix: String,
    disabled_tools: Vec<String>,
    extra_tools: Vec<HarnessTool>,
) {
    std::thread::Builder::new()
        .name("mcp-server".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("mcp server: tokio runtime should build");
            if let Err(err) =
                runtime.block_on(serve_mcp(brp_port, method_prefix, disabled_tools, extra_tools, mcp_port))
            {
                error!("mcp server stopped: {err:?}");
            }
        })
        .expect("mcp server: thread should spawn");
}

async fn serve_mcp(
    brp_port: u16,
    method_prefix: String,
    disabled_tools: Vec<String>,
    extra_tools: Vec<HarnessTool>,
    mcp_port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use rmcp::transport::streamable_http_server::StreamableHttpService;
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;

    let session_manager: std::sync::Arc<LocalSessionManager> = Default::default();
    let service = StreamableHttpService::new(
        move || Ok(GameTools::new(brp_port, &method_prefix, disabled_tools.clone(), extra_tools.clone())),
        session_manager,
        Default::default(),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", mcp_port)).await?;
    info!("mcp tool server listening on http://127.0.0.1:{mcp_port}/mcp");
    axum::serve(listener, router).await?;
    Ok(())
}

/// The `gamepad_input` tool's parameters — see `game/gamepad`'s doc comment for
/// the full button/axis name lists and why this is a genuinely different mechanism from an
/// action-level mock.
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct GamepadInputParams {
    /// Which kind of input this call sets: `button`, `axis`, or `reset` (releases everything).
    pub input: String,
    /// `button`: one of the 19 standard `GamepadButton` names (`South`, `East`, `North`,
    /// `West`, `C`, `Z`, `LeftTrigger`, `LeftTrigger2`, `RightTrigger`, `RightTrigger2`,
    /// `Select`, `Start`, `Mode`, `LeftThumb`, `RightThumb`, `DPadUp`, `DPadDown`, `DPadLeft`,
    /// `DPadRight`).
    pub button: Option<String>,
    /// `button`: `true` to press (default), `false` to release. Held until you explicitly
    /// release it — this is level-triggered like a real controller, not duration-based.
    pub pressed: Option<bool>,
    /// `axis`: one of the 6 standard `GamepadAxis` names (`LeftStickX`, `LeftStickY`, `LeftZ`,
    /// `RightStickX`, `RightStickY`, `RightZ`).
    pub axis: Option<String>,
    /// `axis`: the value to set, roughly −1.0..1.0 for sticks.
    pub value: Option<f64>,
}

/// The `keyboard_input` tool's parameters — see `game/keyboard`'s doc comment.
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct KeyboardInputParams {
    /// `true` to release every currently-pressed key, ignoring `key`/`pressed`.
    pub reset: Option<bool>,
    /// The exact Rust `KeyCode` variant name, e.g. `KeyW`, `Digit1`, `Escape`, `Space`, `Enter`,
    /// `Tab`, `ArrowUp`, `ShiftLeft`, `ControlLeft`, `AltLeft`.
    pub key: Option<String>,
    /// `true` to press (default), `false` to release. Held until you explicitly release it —
    /// level-triggered like a real key, not duration-based.
    pub pressed: Option<bool>,
}

/// The `mouse_input` tool's parameters — see `game/mouse`'s doc comment for the
/// full design (why cursor motion/clicks go through `bevy_picking`'s real event pipeline).
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct MouseInputParams {
    /// Which kind of input this call sets: `button`, `motion`, `move_to`, `wheel`, or `reset`.
    pub input: String,
    /// `button`: `Left`, `Right`, `Middle`, `Back`, or `Forward` (matches `MouseButton`).
    pub button: Option<String>,
    /// `button`: `true` to press (default), `false` to release. Held until released.
    pub pressed: Option<bool>,
    /// `motion`: relative X delta this call. `move_to`: absolute X in the screenshot pixel
    /// space. `wheel`: horizontal scroll amount.
    pub x: Option<f64>,
    /// `motion`: relative Y delta this call. `move_to`: absolute Y. `wheel`: vertical scroll
    /// amount (most wheels only use this one).
    pub y: Option<f64>,
    /// `motion`: alias for `x` (relative dx) — either name works, `dx`/`dy` mirror the BRP
    /// method's own param names exactly.
    pub dx: Option<f64>,
    /// `motion`: alias for `y` (relative dy).
    pub dy: Option<f64>,
    /// `wheel`: `Line` (default, one detent per unit) or `Pixel` (raw pixel scroll).
    pub unit: Option<String>,
}

/// The MCP tool surface — every tool is a thin proxy to a BRP method over loopback HTTP; all
/// `World` access lives in the BRP handlers in [`crate::brp`]. The router is per-instance so
/// host-supplied [`HarnessTool`]s can be added at construction (routes are dynamic closures —
/// no compile-time coupling on the host side).
#[derive(Clone)]
struct GameTools {
    client: BrpClient,
    router: ToolRouter<GameTools>,
    /// The BRP method name prefix (from [`crate::McpHarnessConfig::method_prefix`]) — the
    /// built-in tools proxy to `{prefix}/…`.
    method_prefix: String,
}

impl GameTools {
    fn new(
        brp_port: u16,
        method_prefix: &str,
        disabled_tools: Vec<String>,
        extra_tools: Vec<HarnessTool>,
    ) -> Self {
        let client = BrpClient::new(brp_port);
        let mut router = Self::tool_router();
        for tool in extra_tools {
            router.add_route(tool.into_route(client.clone()));
        }
        // Hidden tools disappear from tools/list AND fail on call — for hosts where a
        // built-in is meaningless (a headless dedicated server hides screenshots/UI/input).
        for name in disabled_tools {
            router.disable_route(name);
        }
        Self {
            client,
            router,
            method_prefix: method_prefix.to_owned(),
        }
    }

    /// The BRP method name for a built-in tool, honoring the configured prefix.
    fn method(&self, name: &str) -> String {
        format!("{}/{}", self.method_prefix, name)
    }
}

/// The `screenshot` tool's parameters — both optional; omit them for a full-frame capture.
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct ScreenshotParams {
    /// Optional filename label; the PNG lands as `<millistamp>-<label>.png` under the
    /// configured screenshots directory.
    pub label: Option<String>,
    /// Optional `[x, y, w, h]` sub-rect to capture, in the same screenshot pixel space game/ui
    /// dumps (e.g. a button's rect). Shapes the served image only — the file on disk is always
    /// the full-resolution frame. A crop costs fewer vision tokens and keeps full effective
    /// resolution on the region of interest. Clamped to frame bounds.
    pub crop: Option<Vec<f64>>,
    /// Optional overview downscale: the served image (what you see) fits within this many
    /// pixels on its long edge (aspect preserved, clamped 64..=4096). Use ~640 for overview
    /// checks; omit for full-resolution detail reads. The file saved on disk is ALWAYS the
    /// full-resolution frame either way — `alignment.png_size` reports it, and reports
    /// should embed that file, not the downscaled view.
    pub max_dimension: Option<u32>,
    /// Optional render-debug views (requires the host to enable the harness's `render_debug`
    /// feature): a list of mode names to capture. Each mode renders into a separate PNG.
    /// Valid names: `depth`, `normals`, `motion_vectors`, `wireframe`, `deferred`,
    /// `deferred_base_color`, `deferred_emissive`, `deferred_metallic_roughness`,
    /// `depth_pyramid` — the same views F1 cycles in normal play. All captures show the
    /// same scene state (taken within a few frames of each other).
    pub debug_views: Option<Vec<String>>,
}

/// The tool definitions live in this block; the handlers proxy to BRP.
#[rmcp::tool_router]
impl GameTools {
    /// One BRP JSON-RPC round-trip; unwraps the BRP envelope into the `result` (or maps the
    /// error into an MCP tool error).
    async fn brp(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, rmcp::ErrorData> {
        self.client
            .call(method, params)
            .await
            .map_err(|err| rmcp::ErrorData::internal_error(err, None))
    }

    async fn text_result(result: serde_json::Value) -> Result<CallToolResult, ErrorData> {
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&result)
                .map_err(|err| rmcp::ErrorData::internal_error(format!("{err}"), None))?,
        )]))
    }

    /// The host's `game/state` snapshot (empty unless the app registered a snapshot hook).
    #[rmcp::tool(description = "Snapshot of the current game state as registered by the host app (app state, player entity/position/health, whatever the game exposes). Empty object if the host registered no snapshot hook. Call this before/after other tools to see what changed.")]
    async fn game_state(&self) -> Result<CallToolResult, ErrorData> {
        let result = self.brp(&self.method("state"), json!({})).await?;
        Self::text_result(result).await
    }

    /// Captures a screenshot of the game (PNG) and returns it as image content PLUS the game's
    /// ground-truth state as JSON. The capture is async (one render frame), so this polls
    /// `game/screenshot/get` briefly. Optional `crop` targets a region of interest.
    #[rmcp::tool(description = "Capture a screenshot of the game. Returns the PNG as image content PLUS the game's ground-truth state as JSON text (same payload as game_state), so you never need to read numbers off the HUD. Optional `crop` [x,y,w,h] captures just a region (read the rect off game/ui first) — cheaper and sharper than a full frame. Optional `max_dimension` (64..=4096) downscales the encoded PNG to fit that many pixels on the long edge — use ~640 for overview checks (did it render, is the menu up) to cut vision tokens ~4×; omit for full-resolution detail reads. A visible crosshair marks your mocked mouse cursor when running headless (red = idle, yellow = hovering, white = left held). If the response says unchanged:true, the pixels are IDENTICAL to the last image you were served — do not ask for it again; read the included state instead.")]
    async fn screenshot(
        &self,
        Parameters(ScreenshotParams { label, crop, max_dimension, debug_views }): Parameters<ScreenshotParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut base_params = json!({});
        if let Some(label) = label {
            base_params["label"] = json!(label);
        }
        if let Some(crop) = crop {
            if crop.len() != 4 || crop.iter().any(|v| !v.is_finite() || *v < 0.0) {
                return Err(ErrorData::invalid_params(
                    "crop must be [x, y, w, h] — four non-negative pixel numbers".to_owned(),
                    None,
                ));
            }
            base_params["crop"] = json!(crop);
        }
        if let Some(max_dimension) = max_dimension {
            base_params["max_dimension"] = json!(max_dimension.clamp(64, 4096));
        }

        let views: Vec<String> = debug_views
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();

        let mut blocks: Vec<ContentBlock> = Vec::new();
        for view in &views {
            let mut params = base_params.clone();
            params["debug_view"] = json!(view);
            let start = self.brp(&self.method("screenshot"), params).await?;
            let expected_path = start
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let mut png_base64 = None;
            for _ in 0..60 {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                let result = self.brp(&self.method("screenshot/get"), json!({})).await?;
                if result.get("ready").and_then(serde_json::Value::as_bool) != Some(true) {
                    continue;
                }
                let returned_path = result
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if returned_path != expected_path {
                    continue;
                }
                png_base64 = result
                    .get("png_base64")
                    .and_then(serde_json::Value::as_str)
                    .map(|s| s.to_owned());
                break;
            }
            let Some(png) = png_base64 else {
                return Err(rmcp::ErrorData::internal_error(
                    format!("screenshot timed out for debug_view {view:?} (is the game rendering?)"),
                    None,
                ));
            };
            blocks.push(ContentBlock::text(format!("debug_view: {view}")));
            blocks.push(ContentBlock::image(png, "image/png"));
        }

        if blocks.is_empty() {
            self.brp(&self.method("screenshot"), base_params).await?;
            for _ in 0..60 {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                let result = self.brp(&self.method("screenshot/get"), json!({})).await?;
                if result.get("ready").and_then(serde_json::Value::as_bool) == Some(true) {
                    let png_base64 = result
                        .get("png_base64")
                        .and_then(serde_json::Value::as_str)
                        .map(|s| s.to_owned());
                    if let Some(png) = png_base64 {
                        blocks.push(ContentBlock::image(png, "image/png"));
                    }
                    break;
                }
            }
        }
        Ok(CallToolResult::success(blocks))
    }

    /// Reports this harness's launch configuration: mode flags and surface ports.
    #[rmcp::tool(description = "Report this harness's launch configuration: no_render flag, brp_port, mcp_port, screenshots_dir, whether screenshots are available, and the offscreen target size (headless). Call this FIRST on any session — it tells you which tools are meaningful here (e.g. no_render clients have no screenshots and never load world visuals) and which port each surface is on when testing several clients at once.")]
    async fn client_info(&self) -> Result<CallToolResult, ErrorData> {
        let result = self.brp(&self.method("client_info"), json!({})).await?;
        Self::text_result(result).await
    }

    /// Serves the agent guides bundled into the harness binary at compile time: the headless
    /// playtesting playbook, the bug-reporting skill, AGENTS.md, and the README. Available with
    /// zero setup — no repo checkout needed; paths mentioned inside the guides refer to the
    /// harness repository.
    #[rmcp::tool(description = "Read the agent guides bundled with this MCP server: `playtest` (the headless playtesting playbook: launch recipes, input mocking, screenshots, failure modes), `bugreport` (bug-reporting skill), `agents` (harness architecture + invariants), `readme` (usage). Call with no arguments first for an index of guides and their sections; then read the whole guide or one section by number/title prefix (e.g. section \"6\" = Screenshots, \"12\" = prototype_19 specifics, \"reporting\"). Read the playtest guide BEFORE driving the app headlessly.")]
    async fn read_guide(
        &self,
        Parameters(ReadGuideParams { guide, section }): Parameters<ReadGuideParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let text = read_guide_body(guide, section)?;
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    /// Pre-flight check for a sequence of intended BRP calls. Params: `calls` — a list of bare
    /// method names or `{method, params}` objects. Reports per call whether it would pass, and
    /// for unknown methods or methods with a declared, currently-unmet precondition, why.
    /// Use this to plan a multi-step flow and catch state-machine mistakes cheaply.
    #[rmcp::tool(description = "Pre-flight check a sequence of intended calls BEFORE sending them. `calls` is a list of BRP method names (e.g. \"game/trigger\") or objects {\"method\": \"game/input\", \"params\": {...}}. Returns per-call ok:true, or ok:false with the reason (unknown method; declared precondition unmet — e.g. game/input requires an in-game local player, game/screenshot requires rendering). Methods without a declared precondition report ok:true with precondition:none — the harness can only vouch for what the host declared. Use this to plan a multi-step flow and catch state-machine mistakes cheaply.")]
    async fn plan_check(
        &self,
        Parameters(PlanCheckParams { calls }): Parameters<PlanCheckParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let result = self
            .brp(&self.method("plan_check"), json!({ "calls": calls }))
            .await?;
        Self::text_result(result).await
    }

    /// Server-side polling with a timeout: one call instead of a sleep/re-poll loop, so
    /// waiting for a state transition costs one tool round trip instead of N.
    #[rmcp::tool(description = "Wait until the app reaches a state, polling server-side — ONE call instead of a sleep/re-poll loop. source: game_state (the app's state snapshot) or game_ui (the UI dump). Conditions (at least one): path+equals (dot-separated key path into the payload vs an expected value, e.g. path \"game_state\" equals \"InGame\"), or — for game_ui — text_contains (any node's text, case-insensitive). Polls every interval_ms (default 200) until met or timeout_ms (default 10000) elapses; returns {met, attempts, elapsed_ms, last} where last is the final payload (for game_ui it is already filtered to the matching nodes). Prefer this over sleeping between calls.")]
    async fn wait_until(
        &self,
        Parameters(WaitUntilParams { source, path, equals, text_contains, interval_ms, timeout_ms }): Parameters<WaitUntilParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let source_method = match source.as_str() {
            "game_state" => "state",
            "game_ui" => "ui",
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown source {other:?} (expected game_state|game_ui)"),
                    None,
                ))
            }
        };
        let condition = match (path.as_deref().filter(|p| !p.is_empty()), equals.as_ref(), text_contains.as_deref().filter(|t| !t.is_empty())) {
            (None, None, Some(needle)) if source == "game_ui" => WaitCondition::UiText(needle.to_lowercase()),
            (path, Some(expected), _) => WaitCondition::PathEquals(path.map(str::to_owned), expected.clone()),
            _ => {
                return Err(ErrorData::invalid_params(
                    "provide path+equals, or text_contains (game_ui only)".to_owned(),
                    None,
                ))
            }
        };
        let interval = std::time::Duration::from_millis(interval_ms.unwrap_or(200).clamp(50, 2000));
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms.unwrap_or(10_000).clamp(100, 60_000));

        let mut attempts = 0u32;
        let started = std::time::Instant::now();
        loop {
            attempts += 1;
            let fetch_params = match &condition {
                WaitCondition::UiText(needle) => {
                    json!({"text_contains": needle, "refresh": true})
                }
                _ => json!({}),
            };
            let payload = self.brp(&self.method(source_method), fetch_params).await?;
            let met = match &condition {
                WaitCondition::UiText(_) => payload
                    .get("nodes")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|nodes| !nodes.is_empty()),
                WaitCondition::PathEquals(path, expected) => {
                    let empty = serde_json::Value::Null;
                    json_path(&payload, path.as_deref().unwrap_or(""))
                        .or(Some(&empty))
                        .is_some_and(|value| values_equal(value, expected))
                }
            };
            if met || std::time::Instant::now() >= deadline {
                let response = json!({
                    "met": met,
                    "attempts": attempts,
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                    "last": payload,
                });
                return Self::text_result(response).await;
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Compound UI click: dump → match a clickable node → move the mocked pointer to its
    /// center → press → release → return the post-click UI dump. One call replaces the
    /// dump/move/press/release/re-dump sequence.
    #[rmcp::tool(description = "Click a UI node by matching it in the game/ui dump: text_contains (case-insensitive substring of the node's text — buttons aggregate their label), entity, or rect (exact [x,y,w,h] from a previous dump). Exactly one selector. The flow executed server-side: dump game/ui → match a CLICKABLE node → move_to its center → press Left → release Left → return the POST-CLICK full game/ui dump (so you see the effect without another call). Fails with a readable error when no clickable node matches (read game/ui and pick from the clickable rows). step_delay_ms (default 50) spaces move→press→release so bevy's per-frame hover/click processing sees each step.")]
    async fn click_node(
        &self,
        Parameters(ClickNodeParams { text_contains, entity, rect, step_delay_ms }): Parameters<ClickNodeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let selectors = [&text_contains.is_some(), &entity.is_some(), &rect.is_some()]
            .into_iter()
            .filter(|provided| **provided)
            .count();
        if selectors != 1 {
            return Err(ErrorData::invalid_params(
                "provide exactly one of: text_contains | entity | rect".to_owned(),
                None,
            ));
        }
        let step_delay = std::time::Duration::from_millis(step_delay_ms.unwrap_or(50).clamp(0, 1000));

        // 1. Fresh full dump (refresh skips the unchanged suppression).
        let dump = self.brp(&self.method("ui"), json!({"refresh": true})).await?;
        let nodes = dump
            .get("nodes")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| ErrorData::internal_error("game/ui returned no nodes array", None))?;
        let matches = |node: &serde_json::Value| -> bool {
            if let Some(needle) = text_contains.as_deref().map(str::to_lowercase) {
                return node
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|text| text.to_lowercase().contains(&needle));
            }
            if let Some(want) = entity {
                return node.get("entity") == Some(&json!(want));
            }
            if let Some(want) = &rect {
                return node.get("rect") == Some(&json!(want));
            }
            false
        };
        let node = nodes
            .iter()
            .find(|node| {
                node.get("clickable").and_then(serde_json::Value::as_bool) == Some(true)
                    && matches(node)
            })
            .ok_or_else(|| {
                ErrorData::internal_error(
                    match text_contains.as_deref() {
                        Some(needle) => format!(
                            "no clickable node with text containing {needle:?} — call ui_tree and pick from the clickable rows (clickable_only:true keeps this read small)"
                        ),
                        None => "no clickable node matches — call ui_tree and pick from the clickable rows".to_owned(),
                    },
                    None,
                )
            })?;

        // 2. Move to the node's center. The rect is already in pointer-pixel space.
        let node_rect: Vec<f64> = node
            .get("rect")
            .and_then(serde_json::Value::as_array)
            .and_then(|values| values.iter().map(serde_json::Value::as_f64).collect())
            .ok_or_else(|| ErrorData::internal_error("matched node has no rect", None))?;
        let [x, y, w, h] = node_rect[..] else {
            return Err(ErrorData::internal_error(
                "matched node's rect is not [x, y, w, h]",
                None,
            ));
        };
        let center = json!({"input": "move_to", "x": x + w / 2.0, "y": y + h / 2.0});
        self.brp(&self.method("mouse"), center).await?;
        tokio::time::sleep(step_delay).await;

        // 3. Press + release through both mechanisms (ButtonInput and bevy_picking), with a
        // frame between them so click detectors see press and release as separate events.
        self.brp(
            &self.method("mouse"),
            json!({"input": "button", "button": "Left", "pressed": true}),
        )
        .await?;
        tokio::time::sleep(step_delay).await;
        self.brp(
            &self.method("mouse"),
            json!({"input": "button", "button": "Left", "pressed": false}),
        )
        .await?;

        // 4. The post-click UI dump is the effect read — no extra call needed.
        let post = self.brp(&self.method("ui"), json!({"refresh": true})).await?;
        let response = json!({
            "clicked": true,
            "node": node,
            "post_ui": post,
        });
        Self::text_result(response).await
    }

    /// Scripted device-mock input with internal timing: one call instead of a chain of mock
    /// calls with sleeps between them.
    #[rmcp::tool(description = "Execute a SCRIPTED SEQUENCE of device-mock inputs with internal timing — one call instead of a chain of keyboard_input/gamepad_input/mouse_input calls with waits. `steps`: each is a device-mock call ({\"keyboard\": {\"key\": \"KeyW\", \"pressed\": true}} or {\"gamepad\": {\"input\": \"button\", \"button\": \"South\", \"pressed\": true}} or {\"mouse\": {\"input\": \"move_to\", \"x\": 640, \"y\": 400}}) or a wait ({\"ticks\": 60} — 60 frames ≈ 1 s at 60 Hz; tick_ms overrides). The full p19-style choreography (hold W, wait, jump, release, turn) is one call. Level-triggered mocks stay held exactly as the sequence leaves them — budget explicit releases. Total wait budget: 3600 ticks. Afterwards observe with game_state or wait_until.")]
    async fn input_sequence(
        &self,
        Parameters(InputSequenceParams { steps, tick_ms }): Parameters<InputSequenceParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if steps.len() > 100 {
            return Err(ErrorData::invalid_params(
                format!("too many steps ({}) — max 100", steps.len()),
                None,
            ));
        }
        let tick = std::time::Duration::from_secs_f64(tick_ms.unwrap_or(16.7) / 1000.0);
        let mut executed = Vec::new();
        let started = std::time::Instant::now();
        for (idx, step) in steps.iter().enumerate() {
            let Some(object) = step.as_object() else {
                return Err(ErrorData::invalid_params(
                    format!("step {idx} must be an object"),
                    None,
                ));
            };
            if let Some(ticks) = object.get("ticks").and_then(serde_json::Value::as_u64) {
                let ticks = ticks.min(3600);
                tokio::time::sleep(tick.mul_f64(ticks as f64)).await;
                executed.push(json!({"step": idx, "waited_ticks": ticks}));
                continue;
            }
            let device = object
                .keys()
                .find(|key| matches!(key.as_str(), "keyboard" | "gamepad" | "mouse"))
                .ok_or_else(|| {
                    ErrorData::invalid_params(
                        format!(
                            "step {idx}: expected one of keyboard|gamepad|mouse|ticks, got {:?}",
                            object.keys().collect::<Vec<_>>()
                        ),
                        None,
                    )
                })?;
            let params = object.get(device).cloned().unwrap_or(json!({}));
            if !params.is_object() {
                return Err(ErrorData::invalid_params(
                    format!("step {idx}: {device} must be an object of mock params"),
                    None,
                ));
            }
            let method = self.method(device);
            self.brp(&method, params).await?;
            executed.push(json!({"step": idx, "device": device}));
        }
        let response = json!({
            "steps_executed": executed.len(),
            "elapsed_ms": started.elapsed().as_millis() as u64,
            "steps": executed,
        });
        Self::text_result(response).await
    }

    /// Declarative verification: the agent writes compact expectations instead of reading a
    /// full state/dump and reasoning over it; the harness returns pass/fail with only the
    /// mismatches (including actuals).
    #[rmcp::tool(description = "Assert expectations about the app's state/UI and get a compact pass/fail + mismatches (with actuals) — write expectations instead of reading full dumps and reasoning over them. `expectations`: each is {\"source\": \"game_state\"|\"game_ui\", \"path\": \"dot.separated.key\" (into the payload), \"op\": \"eq\"|\"ne\"|\"gt\"|\"gte\"|\"lt\"|\"lte\"|\"exists\"|\"text_contains\", \"value\": ...} — text_contains (game_ui) checks any node's text, case-insensitive; path may be omitted when op is text_contains. Numbers compare numerically. Returns {passed, passed_count, failed_count, failures: [{expectation, actual}], payloads} where payloads holds the fetched sources (game_state is small; game_ui is the full dump).")]
    async fn game_assert(
        &self,
        Parameters(AssertParams { expectations }): Parameters<AssertParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if expectations.is_empty() {
            return Err(ErrorData::invalid_params(
                "provide at least one expectation".to_owned(),
                None,
            ));
        }

        // Fetch each referenced source once.
        let sources: std::collections::HashSet<&str> = expectations
            .iter()
            .filter_map(|expectation| expectation.get("source").and_then(Value::as_str))
            .collect();
        let mut payloads = serde_json::Map::new();
        for source in &sources {
            let fetch_params = if *source == "game_ui" {
                json!({"refresh": true})
            } else {
                json!({})
            };
            payloads.insert(
                (*source).to_owned(),
                self.brp(&self.method(source.strip_prefix("game_").unwrap_or(source)), fetch_params)
                    .await?,
            );
        }

        let mut passed_count = 0u32;
        let mut failures = Vec::new();
        for expectation in &expectations {
            let source = expectation.get("source").and_then(Value::as_str).unwrap_or("");
            let path = expectation.get("path").and_then(Value::as_str).unwrap_or("");
            let op = expectation
                .get("op")
                .and_then(Value::as_str)
                .unwrap_or(if expectation.get("text_contains").is_some() { "text_contains" } else { "eq" });
            let payload = payloads.get(source).ok_or_else(|| {
                ErrorData::invalid_params(
                    format!("unknown source {source:?} (expected game_state|game_ui)"),
                    None,
                )
            })?;
            let actual = json_path(payload, path).cloned().unwrap_or(Value::Null);

            let passed = match op {
                "exists" => !actual.is_null(),
                "text_contains" => {
                    let needle = expectation
                        .get("text_contains")
                        .or_else(|| expectation.get("value"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_lowercase();
                    node_texts(payload).any(|text| text.to_lowercase().contains(&needle))
                }
                op => {
                    let Some(expected) = expectation.get("value") else {
                        failures.push(json!({
                            "expectation": expectation,
                            "error": format!("op {op:?} requires a value"),
                        }));
                        continue;
                    };
                    match op {
                        "eq" => values_equal(&actual, expected),
                        "ne" => !values_equal(&actual, expected),
                        "gt" | "gte" | "lt" | "lte" => {
                            let (Some(x), Some(y)) = (actual.as_f64(), expected.as_f64()) else {
                                failures.push(json!({
                                    "expectation": expectation,
                                    "actual": actual,
                                    "error": format!("op {op:?} requires numeric values"),
                                }));
                                continue;
                            };
                            match op {
                                "gt" => x > y,
                                "gte" => x >= y,
                                "lt" => x < y,
                                _ => x <= y,
                            }
                        }
                        _ => {
                            failures.push(json!({
                                "expectation": expectation,
                                "error": format!("unknown op {op:?} (expected eq|ne|gt|gte|lt|lte|exists|text_contains)"),
                            }));
                            continue;
                        }
                    }
                }
            };
            if passed {
                passed_count += 1;
            } else {
                failures.push(json!({ "expectation": expectation, "actual": actual }));
            }
        }

        let response = json!({
            "passed": failures.is_empty(),
            "passed_count": passed_count,
            "failed_count": failures.len(),
            "failures": failures,
            "payloads": payloads,
        });
        Self::text_result(response).await
    }

    /// Dumps the UI tree: labeled rects + text for every visible UI node, in screenshot pixel
    /// space, so clicks can target coordinates read off this dump instead of guessed from
    /// pixels.
    #[rmcp::tool(description = "Dump the UI tree as an accessibility-tree-style list: every visible UI node's rect [x,y,w,h] in the SAME screenshot pixel space game/mouse move_to consumes, its text (button labels), and interaction/hover state. Read THIS to find what to click and where, then use mouse_input move_to + button Left to click it. Much more reliable than estimating coordinates from the screenshot image. Also useful to verify text rendered (the dump shows the string regardless of font issues). Unchanged suppression: a re-read whose filtered node list is identical to the previous one returns {unchanged: true, node_count, pointer} WITHOUT the nodes — pass refresh:true to force a full dump. Filters: clickable_only (the find-the-button read), text_contains (substring, case-insensitive).")]
    async fn ui_tree(
        &self,
        Parameters(UiTreeParams { clickable_only, text_contains, refresh }): Parameters<UiTreeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut params = json!({});
        if clickable_only == Some(true) {
            params["clickable_only"] = json!(true);
        }
        if let Some(text) = text_contains.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            params["text_contains"] = json!(text);
        }
        if refresh == Some(true) {
            params["refresh"] = json!(true);
        }
        let result = self.brp(&self.method("ui"), params).await?;
        Self::text_result(result).await
    }

    /// Mocks real gamepad button/stick state (not an action-level mock — see the tool
    /// description). Held until explicitly released/reset.
    #[rmcp::tool(description = "Mock a real gamepad's button/stick state directly, so it flows through the actual binding/dead-zone/context resolution the game's input crate does for a human's controller — unlike action-level mocks, this reaches UI navigation (menus, pause screens) too. `input`: `button` (name one of the 19 standard GamepadButton names, `pressed` true/false — held until you release it, like a real controller, not duration-based), `axis` (name one of the 6 standard GamepadAxis names, `value` roughly -1..1), or `reset` (releases/zeros everything — use this between unrelated test scenarios so a forgotten release doesn't linger). Example: press Start to open a pause menu, then South to activate whatever's focused.")]
    async fn gamepad_input(
        &self,
        Parameters(GamepadInputParams { input, button, pressed, axis, value }): Parameters<GamepadInputParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = match input.as_str() {
            "button" => {
                let button = button.ok_or_else(|| {
                    ErrorData::invalid_params("missing button for input=\"button\"", None)
                })?;
                json!({"input": "button", "button": button, "pressed": pressed.unwrap_or(true)})
            }
            "axis" => {
                let axis = axis.ok_or_else(|| {
                    ErrorData::invalid_params("missing axis for input=\"axis\"", None)
                })?;
                let value = value.ok_or_else(|| {
                    ErrorData::invalid_params("missing value for input=\"axis\"", None)
                })?;
                json!({"input": "axis", "axis": axis, "value": value})
            }
            "reset" => json!({"input": "reset"}),
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown input {other:?} (expected button|axis|reset)"),
                    None,
                ))
            }
        };
        let result = self.brp(&self.method("gamepad"), params).await?;
        Self::text_result(result).await
    }

    /// Mocks real keyboard key state (`ButtonInput<KeyCode>`). Held until explicitly
    /// released/reset.
    #[rmcp::tool(description = "Mock a real keyboard key, held until released — the same ButtonInput<KeyCode> resource the game's input bindings read for a physical key. `key` is the exact Rust KeyCode variant name (KeyW, KeyA..KeyZ, Digit0..Digit9, Escape, Space, Enter, Tab, ArrowUp/Down/Left/Right, ShiftLeft/Right, ControlLeft/Right, AltLeft/Right, etc — every KeyCode variant works). `pressed` true (default) or false. Or pass `reset: true` to release every held key.")]
    async fn keyboard_input(
        &self,
        Parameters(KeyboardInputParams { reset, key, pressed }): Parameters<KeyboardInputParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = if reset == Some(true) {
            json!({"reset": true})
        } else {
            let key = key.ok_or_else(|| {
                ErrorData::invalid_params("missing key (or pass reset: true)", None)
            })?;
            json!({"key": key, "pressed": pressed.unwrap_or(true)})
        };
        let result = self.brp(&self.method("keyboard"), params).await?;
        Self::text_result(result).await
    }

    /// Mocks real mouse button/motion/wheel state plus cursor position, driving both raw input
    /// resources and `bevy_picking`'s real event pipeline so UI clicks/hover work too.
    #[rmcp::tool(description = "Mock real mouse input — buttons, motion, absolute cursor position, and wheel — through both the raw ButtonInput<MouseButton>/AccumulatedMouseMotion/AccumulatedMouseScroll resources the game's input bindings read AND bevy_picking's real PointerInput event pipeline, so this can click actual UI buttons (not just drive mouse-bound gameplay actions). `input`: `button` (`button`: Left|Right|Middle|Back|Forward, `pressed` true/default or false — held until released), `motion` (`dx`/`dy` relative delta, like a mouse-look turn), `move_to` (`x`/`y` absolute position in the screenshot pixel space — use this to click something at a known pixel from a screenshot or a game/ui dump), `wheel` (`x`/`y` scroll amount, `unit`: Line|Pixel), `reset` (releases every button, zeros the accumulators). Level-triggered: buttons stay held until you release or reset.")]
    async fn mouse_input(
        &self,
        Parameters(MouseInputParams { input, button, pressed, x, y, dx, dy, unit }): Parameters<MouseInputParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = match input.as_str() {
            "button" => {
                let button = button.ok_or_else(|| {
                    ErrorData::invalid_params("missing button for input=\"button\"", None)
                })?;
                json!({"input": "button", "button": button, "pressed": pressed.unwrap_or(true)})
            }
            "motion" => {
                json!({
                    "input": "motion",
                    "dx": dx.or(x).unwrap_or(0.0),
                    "dy": dy.or(y).unwrap_or(0.0),
                })
            }
            "move_to" => {
                let x = x.ok_or_else(|| {
                    ErrorData::invalid_params("missing x for input=\"move_to\"", None)
                })?;
                let y = y.ok_or_else(|| {
                    ErrorData::invalid_params("missing y for input=\"move_to\"", None)
                })?;
                json!({"input": "move_to", "x": x, "y": y})
            }
            "wheel" => {
                json!({
                    "input": "wheel",
                    "x": x.unwrap_or(0.0),
                    "y": y.unwrap_or(0.0),
                    "unit": unit.unwrap_or_else(|| "Line".to_string()),
                })
            }
            "reset" => json!({"input": "reset"}),
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown input {other:?} (expected button|motion|move_to|wheel|reset)"),
                    None,
                ))
            }
        };
        let result = self.brp(&self.method("mouse"), params).await?;
        Self::text_result(result).await
    }
}

// `router = self.router`: the per-instance router includes the host's `extra_tools` (dynamic
// routes added in `GameTools::new`), so `tools/list` and `tools/call` serve them alongside the
// compile-time built-ins.
#[rmcp::tool_handler(router = self.router)]
impl rmcp::ServerHandler for GameTools {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::default();
        info.instructions = Some(
            "Tool API for a Bevy app (dev/QA only). Inspect with client_info, game_state, \
             ui_tree, and screenshot; drive it with keyboard_input, gamepad_input, and \
             mouse_input."
                .into(),
        );
        info
    }
}
