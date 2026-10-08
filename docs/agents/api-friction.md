# API friction log

Friction and gaps in `bevy_mcp_harness`'s public API, observed while migrating
prototype_19 onto it (`crates/client/src/dev/tool_api.rs`,
`crates/server/src/tools.rs`). Each entry says what was awkward, what the
migration did about it, and what a future harness fix could look like. Fix here,
delete the entry.

## 1. Host-owned offscreen targets need manual plumbing ✅ fixed

p19's headless camera machinery (`OffscreenRenderTarget`, `NoRenderMode`, the
retarget/bootstrap/clear-order systems) is **not** dev-only — the render-less
headless branch runs regardless of the `dev-tools` feature, so it cannot move
into the optional harness dependency. The harness assumed it always owned the
whole offscreen stack, which would have meant either duplicating the texture or
dragging the harness (and its rmcp/tokio/axum deps) into player builds.

**Done**: `McpHarnessConfig::offscreen_target: Option<Handle<Image>>` — the host
wraps nothing; the harness inserts its `OffscreenRenderTarget` around the
host's handle and adds only the cursor overlay.

**Residual**: the two-resource shape (host keeps its own resource/type *and* the
harness inserts one wrapping the same handle) is duplicated state by agreement.
A standard `TargetHandle` resource name or a getter could remove the host's
copy.

## 2. `no_render`/shim duplication when the host owns the target ⚠️ open

With `offscreen_target` + `no_render`, both the host's and the harness's
`shim_camera_computed` run (p19 keeps its own because it must work without the
harness). Benign today — both write the same `computed.target_info` — but two
systems silently doing one job is fragile.

**Possible fix**: when `offscreen_target` is host-provided, skip the harness
shim (document that the host owns `target_info` feeding) or expose the shim as a
public fn the host can call instead of re-owning it.

## 3. Screenshots-dir isolation logic is private ⚠️ open

`default_screenshots_dir` (base dir + per-client `client-<port>` isolation
under fleet testing) is private. p19 had to reimplement it to keep its own
convention (`docs/agents/playtests/dist/screenshots/`). The isolation rule is
easy to get wrong and its absence silently cross-contaminates
`game/screenshot/get` between clients.

**Possible fix**: public `fn isolated_screenshots_dir(base: PathBuf, brp_port:
u16) -> PathBuf`, or split `screenshots_dir` into `screenshots_base_dir` +
isolation-always-applied.

## 4. `from_env` hardcodes client defaults ⚠️ open

`McpHarnessConfig::from_env` reads `--brp-port/--mcp-port/--no-render` with the
client defaults (15702/15710). A server binary (p19: 15701/15711) can't use it —
it re-implements flag parsing. Also `from_env` doesn't read `offscreen_*`
(hosts set those in code anyway, so maybe fine).

**Possible fix**: `from_env_with_defaults(brp_default, mcp_default)` or
`FromEnv::builder().brp_default(..)`.

## 5. Custom BRP method registration is raw bevy_remote plumbing ✅ acceptable, revisit

Registering one method is three lines of `register_system` +
`RemoteMethods::insert(RemoteMethodSystemId::Instant(..))`, and p19 repeated it
5×. Also easy to get subtly wrong (must run where `&mut App` lives; must happen
after the harness added `RemotePlugin`).

**Possible fix**: `app.register_game_method("game/x", system)` on the harness
(or a free fn taking `&mut App`) — one line per method, correct by construction.

## 6. Hosts need `schemars`/`serde` as direct deps for `HarnessTool` args ⚠️ open

`HarnessTool::new` is generic over `P: DeserializeOwned + JsonSchema`, so the
host's `Cargo.toml` needs `schemars` (p19 kept it as an optional dep just for
one args struct).

**Possible fix**: re-export `schemars` (and `serde_json`) from the harness root
(`pub use schemars;`) so hosts use `bevy_mcp_harness::schemars::JsonSchema`.

## 7. Built-in `game/*` naming is game-flavored for non-game hosts ⚠️ open

The p19 server now serves `game/state` containing a *server* snapshot — the
name is wrong-ish, and `game/screenshot`/`game/mouse`/`game/gamepad` are
meaningless on a headless server (they error or sit idle, harmlessly but
confusingly in `tools/list`).

**Possible fix**: optional config to rename the method prefix (server →
`server/*`) and/or a feature set mask (`tools: { screenshots: false, .. }`) so
`tools/list` only shows meaningful tools.

## 8. `game/client_info` payload is fixed ✅ worked around, revisit

p19's old `client_info` carried game-specific mode flags (`vr`,
`headless_render`, `no_common_assets`, `mcp`). The harness's payload is
closed, so the migration folded those into the `game/state` snapshot under
`"launch"` instead — works, but the natural home was `client_info`.

**Possible fix**: let the host append a `host` object to the `client_info`
payload (e.g. a second hook, or reuse `state_snapshot`'s output under a key).

## 9. `clickable` detection is `Interaction`-only — markup-driven UIs dump as non-clickable ⚠️ open

Observed live in the p19 migration: the bevy_markup main menu's "Connect"
button dumps **without** `clickable` (18 nodes, no flags) because p19's UI
drives clicks through `data-on-click` element signals, not
`bevy_ui::Interaction` (old p19 dump keyed clickable off `ElementSignals`).
`game/ui` is still useful there (rects/text), and `pointer_hovered` works via
picking, but the "read the row, click its rect" loop loses its strongest signal
for markup-driven UIs.

**Possible fix**: make the clickable signal pluggable
(`McpHarnessConfig.clickable_fn: Option<fn(&World, Entity) -> bool>`) or detect
common conventions in addition to `Interaction` (behind a cargo feature for
bevy_markup so the harness doesn't depend on it).

## 10. Minor: `#[tool_handler]`-style ergonomics, docs paths

- `HarnessTool` callbacks returning `Err(String)` surface as JSON-RPC
  `internal_error` (code -32603) rather than MCP `isError: true` content —
  agents handle both, but `isError` content is friendlier for "expected"
  failures (e.g. "no local player connected" is a normal state, not a server
  error).
- `read_guide`'s bundled guides mention repository paths (`docs/agents/...`,
  `crates/client/...`) that don't exist in a host checkout — harmless (the
  p19 sections are clearly scoped) but worth a one-line preface when serving
  ("paths refer to the harness repository").
