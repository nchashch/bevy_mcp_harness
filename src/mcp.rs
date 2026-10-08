//! The in-process MCP server (rmcp, Streamable HTTP, stateless) — the protocol surface whose
//! tools proxy to the BRP methods in [`crate::brp`] over loopback HTTP. The MCP layer owns only
//! the protocol surface (tool listing + schemas), never the `World` (the handlers are async and
//! run outside Bevy's world; all `World` access stays in BRP's systems).

use bevy::log::{error, info};
use rmcp::handler::server::router::tool::{ToolRoute, ToolRouter};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData, ServerConfig, Tool};
use serde_json::json;
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

/// The `gamepad_input` tool's parameters — see [`crate::brp::gamepad_method`]'s doc comment for
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

/// The `keyboard_input` tool's parameters — see [`crate::brp::keyboard_method`]'s doc comment.
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

/// The `mouse_input` tool's parameters — see [`crate::brp::mouse_method`]'s doc comment for the
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
    /// dumps (e.g. a button's rect). A crop costs fewer vision tokens and keeps full effective
    /// resolution on the region of interest. Clamped to frame bounds.
    pub crop: Option<Vec<f64>>,
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
        let result = self.brp(&self.method("game/state"), json!({})).await?;
        Self::text_result(result).await
    }

    /// Captures a screenshot of the game (PNG) and returns it as image content PLUS the game's
    /// ground-truth state as JSON. The capture is async (one render frame), so this polls
    /// `game/screenshot/get` briefly. Optional `crop` targets a region of interest.
    #[rmcp::tool(description = "Capture a screenshot of the game. Returns the PNG as image content PLUS the game's ground-truth state as JSON text (same payload as game_state), so you never need to read numbers off the HUD. Optional `crop` [x,y,w,h] captures just a region (read the rect off game/ui first) — cheaper and sharper than a full frame. A visible crosshair marks your mocked mouse cursor when running headless (red = idle, yellow = hovering, white = left held). If the response says unchanged:true, the pixels are IDENTICAL to the last image you were served — do not ask for it again; read the included state instead.")]
    async fn screenshot(
        &self,
        Parameters(ScreenshotParams { label, crop }): Parameters<ScreenshotParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut params = json!({});
        if let Some(label) = label {
            params["label"] = json!(label);
        }
        if let Some(crop) = crop {
            if crop.len() != 4 || crop.iter().any(|v| !v.is_finite() || *v < 0.0) {
                return Err(ErrorData::invalid_params(
                    "crop must be [x, y, w, h] — four non-negative pixel numbers".to_owned(),
                    None,
                ));
            }
            params["crop"] = json!(crop);
        }
        self.brp(&self.method("game/screenshot"), params).await?;
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let result = self.brp(&self.method("game/screenshot/get"), json!({})).await?;
            if result.get("ready").and_then(serde_json::Value::as_bool) != Some(true) {
                continue;
            }
            let state = result.get("state").cloned().unwrap_or(json!(null));
            let path = result.get("path").and_then(serde_json::Value::as_str).unwrap_or("");
            let state_json = serde_json::to_string_pretty(&json!({
                "path": path,
                "state": state,
            }))
            .map_err(|err| rmcp::ErrorData::internal_error(format!("{err}"), None))?;
            if result.get("unchanged").and_then(serde_json::Value::as_bool) == Some(true) {
                return Ok(CallToolResult::success(vec![
                    ContentBlock::text(format!(
                        "unchanged: the newest capture has PIXEL-IDENTICAL content to the last image you were served — it is not attached again.\n{state_json}"
                    )),
                ]));
            }
            let png_base64 = result
                .get("png_base64")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| rmcp::ErrorData::internal_error("screenshot missing data", None))?;
            let mut blocks = vec![ContentBlock::image(png_base64.to_string(), "image/png")];
            blocks.push(ContentBlock::text(state_json));
            return Ok(CallToolResult::success(blocks));
        }
        Err(rmcp::ErrorData::internal_error(
            "screenshot timed out (is the game rendering?)",
            None,
        ))
    }

    /// Reports this harness's launch configuration: mode flags and surface ports.
    #[rmcp::tool(description = "Report this harness's launch configuration: no_render flag, brp_port, mcp_port, screenshots_dir, whether screenshots are available, and the offscreen target size (headless). Call this FIRST on any session — it tells you which tools are meaningful here (e.g. no_render clients have no screenshots and never load world visuals) and which port each surface is on when testing several clients at once.")]
    async fn client_info(&self) -> Result<CallToolResult, ErrorData> {
        let result = self.brp(&self.method("game/client_info"), json!({})).await?;
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

    /// Dumps the UI tree: labeled rects + text for every visible UI node, in screenshot pixel
    /// space, so clicks can target coordinates read off this dump instead of guessed from
    /// pixels.
    #[rmcp::tool(description = "Dump the UI tree as an accessibility-tree-style list: every visible UI node's rect [x,y,w,h] in the SAME screenshot pixel space game/mouse move_to consumes, its text (button labels), and interaction/hover state. Read THIS to find what to click and where, then use mouse_input move_to + button Left to click it. Much more reliable than estimating coordinates from the screenshot image. Also useful to verify text rendered (the dump shows the string regardless of font issues).")]
    async fn ui_tree(&self) -> Result<CallToolResult, ErrorData> {
        let result = self.brp(&self.method("game/ui"), json!({})).await?;
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
        let result = self.brp(&self.method("game/gamepad"), params).await?;
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
        let result = self.brp(&self.method("game/keyboard"), params).await?;
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
        let result = self.brp(&self.method("game/mouse"), params).await?;
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
