# 2. Generalization boundary: what stays generic, what the host provides

Date: 2026-10-07

## Status

Accepted — implemented; partially extended by
[0005](./0005-extension-ergonomics-from-the-first-host-adoption.md) (`client_info_host`,
`clickable`, `method_prefix`/`disabled_tools`) after the first host adoption showed where the
boundary was drawn too tight.

## Context

Extracting the prototype's tool API (see [0001](./0001-extract-the-agent-tool-api-into-a-
reusable-crate.md)) forced a method-by-method decision: what does a *generic Bevy app*
surface look like, versus what is this-game state? Two failure modes to avoid:

1. **Leaking game concepts into the crate** — a "generic" `game/state` that knows about HP,
   GCD timers, and ahoy bindings would make the crate unusable for anything else and pin its
   dependency list to the prototype's stack (lightyear, avian, bevy_enhanced_input).
2. **Over-abstracting** — a plugin-config framework where hosts declare components and the
   harness builds snapshots generically. Reflective magic here would be a large API paid for
   by every host before any of them needed it.

Same question per input surface: the prototype's action-level `game/input` (`ActionMock` on
replicated BEI action entities) is genuinely game-specific (ahoy action types, replicated
contexts, per-event camera vs movement paths) — but the *device-level* mocks
(`game/gamepad`/`game/keyboard`/`game/mouse`) are pure-Bevy and reach everything the
action-level one did, through the app input crate's real binding resolution.

## Decision

- **`game/state` becomes a host-registered hook** (`McpHarnessConfig::state_snapshot`):
  `Arc<dyn Fn(&mut World) -> serde_json::Value>`. The harness fuses it into every
  `game/screenshot/get` response and writes it to the `<capture>.json` sidecar — the
  screenshot-arrives-with-ground-truth design is preserved; only the *content* moved to the
  host. Without a hook, `game/state` returns an empty object.
- **`game/client_info` is a closed, harness-known payload** (ports, `no_render`, capture
  target size, screenshots dir) — slimmed from the prototype's version, which mixed in game
  mode flags. (Extended later: `client_info_host` adds a `"host"` key — see 0005.)
- **`game/ui` stays fully generic**: laid-out nodes in `UiStack` order, rects in screenshot
  pixel space, text via `Text`/`TextSpan` aggregation, `clickable` keyed to
  `bevy_ui::Interaction` (the generic convention; markup-style click hooks came later via
  the `clickable` hook — see 0005). The prototype's version keyed clickable off bevy_markup's
  `data-on-click` signals — game-specific, replaced; the replacement initially cost markup
  UIs their `clickable` flag, fixed in 0005.
- **Pointer targeting generalizes**: the mocked pointer acts on the offscreen texture when
  configured, else the primary window (the prototype errored outside `--mcp` mode; the
  crate's `move_to`/clicks work in windowed sessions too).
- **Device-level mocks stay; action-level mocks don't.** `game/gamepad` writes the
  `Gamepad`'s `analog` map on a synthetic entity; `game/keyboard` writes
  `ButtonInput<KeyCode>` via `KeyCode`'s own serde; `game/mouse` writes real
  `MouseMotion`/`MouseWheel` events plus `bevy_picking`'s `PointerInput`. All three flow
  through whatever input crate the host uses, unchanged from the prototype's semantics
  (including the load-bearing traps: analog-not-digital, events-not-accumulators).
- **The `game/*` name prefix becomes configurable** (`method_prefix`) after a server host
  showed the need (see 0005) — default `"game"`.

## Alternatives considered

- **Reflective generic state**: `game/state` auto-dumps every `Reflect`-registered
  component of configured types. Rejected: reflection-based dumps are unbounded and noisy
  (the whole point of `game/state` per the prototype's ADR 0011 is a *curated*, small,
  structured view); BRP's own `world.query` already covers "everything reflected" for hosts
  who want breadth.
- **Keep `game/client_info` extensible from day one**: rejected initially as speculative;
  the first host proved otherwise (a game *does* have mode flags an agent needs — VR,
  headless-render) and it was added in 0005. Recorded as the boundary-error case study.
- **Action-level mocking behind a cargo feature** (`enhanced-input` feature of the harness):
  rejected — the harness would then ship a `bevy_enhanced_input` dependency to every host,
  and hosts using different input crates gain nothing.

## Consequences

- The crate compiles against plain `bevy` + its own tooling deps; no game crates anywhere.
- `game/state`'s usefulness is entirely the host's responsibility — without a hook it is
  empty, which is honest (the harness cannot know game state) but means the data-first
  workflow degrades to BRP `world.query` until the host registers a hook. Documented in the
  host-integration checklist.
- `game/ui`'s `clickable` regression for markup UIs (fixed in 0005) is the cautionary
  example for this ADR's boundary: "generic convention only" is right for v1 but the hook
  point should have been designed in from the start. The `clickable` hook is the template
  for any future convention-extensible field.
