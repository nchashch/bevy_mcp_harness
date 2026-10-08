# 5. Extension ergonomics from the first real host adoption (prototype_19)

Date: 2026-10-08

## Status

Accepted — implemented. All ten friction items from the p19 migration
(`docs/agents/api-friction.md`) resolved in this release line; the log records what changed
and what remains (one partial: the schemars derive limitation).

## Context

Extracting the crate ([0001](./0001-extract-the-agent-tool-api-into-a-reusable-crate.md)) and
running it under p19 (client **and** dedicated server) surfaced ten concrete points where the
API was awkward, annoying, or silently wrong. Each one cost real debugging time during the
migration or would cost it for every future host. The two most consequential:

1. **Host-owned headless rendering** — p19's camera machinery cannot move into the optional
   dependency (see [0003](./0003-host-owned-vs-harness-owned-headless-rendering.md) for the
   full design), and the first attempt left two public resources wrapping one handle.
2. **A dedicated server is not a game** — the p19 server hosts the harness too (authoritative
   state for desync debugging), and three assumptions baked into the API broke: `from_env`
   hardcoded client port defaults (15702/15710; the server is 15701/15711), the method prefix
   `game/*` read wrong on a server (`server/state` was its old API), and half the built-in
   tools (screenshot, ui_tree, input mocks) are meaningless without a render/window yet still
   showed in `tools/list`.

The rest, in brief: screenshots-dir fleet isolation logic was private (the host reimplemented
the `client-<port>` rule); custom BRP method registration was three lines of raw
`bevy_remote` plumbing repeated five times, easy to get subtly wrong; `client_info`'s payload
was closed but games have mode flags agents need; `game/ui`'s `clickable` was
`Interaction`-only, which made markup-driven UIs (bevy_markup's `data-on-click` convention)
dump without any clickable flag; `HarnessTool` errors surfaced as protocol-level `-32603`
instead of `isError: true` content the agent can read and react to; and hosts needed
`schemars` as a direct dependency for `HarnessTool` argument derives.

## Decision

Fix all ten, API-first rather than documentation-first:

- `McpHarnessConfig::offscreen: OffscreenMode` — see
  [0003](./0003-host-owned-vs-harness-owned-headless-rendering.md).
- `from_env_with_defaults(brp_default, mcp_default)` — server-grade flag parsing with
  caller-chosen defaults; `from_env` delegates with client defaults.
- `isolated_screenshots_dir(base, brp_port)` made public — hosts with their own screenshots
  convention get the fleet-isolation rule for free.
- `register_game_method(app, name, system)` — the one-liner; warns when `RemoteMethods` is
  missing (order-of-addition errors become visible instead of silent).
- `client_info_host: Option<StateSnapshotFn>` — the host's mode flags merged under a
  `"host"` key in `game/client_info`; p19's flags moved there from the state snapshot.
- `clickable: Option<ClickableFn>` — pluggable clickable convention for `game/ui`
  (`fn(&World, Entity) -> bool`); `Interaction` holders remain always-clickable. p19
  registers the bevy_markup `data-on-click` signal check — verified live: all four markup
  menu buttons dump `clickable: true`.
- `method_prefix` (default `"game"`) — p19's server serves `server/state` again, matching its
  pre-migration API; `disabled_tools` — hidden from `tools/list` **and** rejected on call
  (the server hides `screenshot`/`ui_tree`/input mocks; a masked tool's call fails with
  `tool not found`).
- `HarnessTool` errors (`Err(String)`) return `CallToolResult::error` (`isError: true`
  content) instead of a protocol-level `-32603` — an expected failure ("no local player
  connected") is a normal tool outcome the agent reads, not a server fault.
- `pub use schemars; pub use serde_json;` — re-exported for non-derive host use. **The derive
  case is only partially fixable**: `#[derive(JsonSchema)]` expands to `schemars::` crate
  paths, so derive-based hosts still need a direct `schemars` dependency. Documented on the
  re-export and in p19's manifest rather than papered over.

## Alternatives considered

- **Fix by documentation** (each item becomes a "known limitation" note). Rejected: every
  item was an API shape problem a host hits on day one; ten documented papercuts are ten
  reasons to fork.
- **A host-facing builder for the whole config** (`McpHarnessConfig::builder()`): deferred —
  the struct is already public and plain; a builder adds a second construction path to keep
  in sync without solving anything the fields don't.
- **Preconditions for the `plan_check` feature surfaced during this work** were large enough
  to get their own record — see [0006](./0006-pre-flight-preconditions-and-no-pddl-planner.md).

## Consequences

- p19's client/server code shrank again: the five `game/*` registrations are one-liners, the
  server constructs its config in twelve lines (`from_env_with_defaults` + prefix + mask +
  hook), and its reimplementation of the isolation rule is one `isolated_screenshots_dir`
  call.
- The friction log (`docs/agents/api-friction.md`) is retained as a record with per-item
  status lines — the pattern to keep: friction found while adopting the crate goes in the
  log; fixes land in the same release when feasible, and the log notes what's still open
  (currently: the schemars derive dependency).
- The `method_prefix`/`disabled_tools` pair makes the crate honest for non-game hosts, which
  matters because the server adoption is the case that keeps the harness honest about being
  a *Bevy* tool, not a *game* tool.
