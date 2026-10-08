# 1. Extract the agent tool API into a reusable `bevy_mcp_harness` crate

Date: 2026-10-07

## Status

Accepted — implemented. `crates/client/src/dev/tool_api.rs` (prototype_19) reduced from
2338 to ~560 lines (game-specific extension only); the generic half became this crate,
published on crates.io as `bevy_mcp_harness`.

## Context

prototype_19 grew a battle-tested agent/QA tool surface (ADR 0009 there, plus 0010–0012):
BRP as the data layer, custom `game/*` methods, an in-process MCP server, headless rendering,
and a device-level input-mocking stack. Two forces pushed for extraction:

1. **The next game shouldn't re-earn it.** The tool API embodies a large set of
   only-learned-by-breaking things (mouse motion must be injected as events, gamepad reads
   the `analog` map, the accumulated-mouse resources are overwritten every frame, screenshot
   polling with `unchanged` suppression, `game/ui`'s accessibility dump). Every Bevy project
   wanting agent playtesting would otherwise copy-paste ~2300 lines and re-learn the traps.
2. **The game-specific half was clearly separable.** Of the methods, only a handful were
   actually p19-specific (`game/input`'s action-level `ActionMock`, `game/trigger`,
   `game/select`, `game/levels`, `game/select_level`); everything else read generic Bevy
   surfaces (`ButtonInput`, `Gamepad`, `bevy_picking`, `Screenshot`, `bevy_ui`).

Constraints discovered up front:

- bevy minor-version drift matters (0.19.0 → 0.19.1 changed `RenderTarget::as_image()`'s
  return type); the crate documents a bevy **0.19.1+** compatibility line in its README.
- p19's client gates the tool API behind a `dev-tools` cargo feature (cheat surface, untrusted
  client) — the extraction must not change that gating model.
- The MCP half must stay exactly as the prototype shaped it: an in-process rmcp Streamable
  HTTP server on its own thread whose tools are thin proxies to BRP over loopback HTTP, with
  **all** `World` access in BRP handler systems (the MCP handlers are async and run outside
  Bevy's world). This is the prototype's ADR 0009 layering, re-affirmed rather than
  redesigned — observed failure modes (agents mis-sequencing calls, mis-reading pixels) were
  grounding failures, not layering failures.

## Decision

Extract the generic half into `bevy_mcp_harness` as a normal cargo dependency
(`BevyMcpHarnessPlugin` + `McpHarnessConfig`), and move the game-specific half into the host
app through a small extension surface (then just custom BRP methods; later formalized —
see [0006](./0006-extension-ergonomics-from-the-first-host-adoption.md)):

- **Crate provides**: BRP server ownership (guarded `is_plugin_added` so a host that added
  `RemotePlugin` itself — p19's Skein does — isn't double-bound), the `game/state`,
  `game/client_info`, `game/cameras`, `game/screenshot` + `game/screenshot/get`, `game/ui`,
  `game/gamepad`, `game/keyboard`, `game/mouse` methods, the agent cursor overlay, the MCP
  server + built-in tools, ports/flags (15702/15710; `--brp-port`/`--mcp-port`/`--no-render`),
  and the fleet-isolation rule for capture directories.
- **Host provides**: everything bound to its own crates — action-level mocks, game events,
  combat targeting, level lists — via custom BRP methods and (later) `extra_tools` MCP tools.
- **Dropped from the crate** (p19-only): `game/input`, `game/trigger`, `game/select`,
  `game/levels`, `game/select_level`, and the MCP `inject_input` tool. Documented as
  "prototype_19 specifics" in the bundled playtest guide so the learnings stay visible.

## Alternatives considered

- **Keep everything in the game, share by copy-paste.** Rejected: the traps (see Context)
  are exactly what a copy-paste loses; and two codebases drift.
- **Make the crate a framework the game compiles its specifics into** (trait the game
  implements, harness registers). Rejected for now: bevy_remote's per-request `RemoteMethods`
  lookup already makes late registration trivial, and a trait would couple the harness to
  host types at compile time. The one-liner `register_game_method` helper later captured the
  same ergonomics without the coupling.
- **Extract including the p19-specific methods, parameterized by config.** Rejected: action
  mocks need `bevy_enhanced_input` + ahoy types; level lists need lightyear messages. A
  generic crate depending on those would invert the dependency direction (every Bevy project
  paying for lightyear to get a screenshot tool).

## Consequences

- p19's client dropped seven optional dependencies (`rmcp`, `axum`, `tokio`, `reqwest`,
  `base64`, `image`, plus `schemars` until [0005-era] derive needs brought it back); the
  harness owns that surface once, versioned and tested.
- The crate inherits the prototype's hard-won gotchas as **load-bearing doc comments** rather
  than behavioral code — the next agent reads why `analog_mut` and not `digital_mut` before
  "fixing" it (this actually happened once in p19's history: a `digital` rewrite compiled,
  ran clean, and silently broke UI navigation).
- The first real host adoption (p19 itself) immediately surfaced ten API-friction items,
  recorded in `docs/agents/api-friction.md` and resolved in
  [0005](./0005-extension-ergonomics-from-the-first-host-adoption.md) /
  [0003](./0003-host-owned-vs-harness-owned-headless-rendering.md).
- Publishing to crates.io made the *bundled guides* the natural onboarding channel (see
  [0004](./0004-agent-guides-bundled-into-the-binary.md)): an agent that can only see the MCP
  server can read the playbook without ever cloning this repo.
