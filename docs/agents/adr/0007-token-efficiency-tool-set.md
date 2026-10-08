# 7. Token-efficiency tool set: suppression, server-side waiting, compound actions, downscale, sequences, assertions

Date: 2026-10-08

## Status

Accepted — implemented (`game/ui` filters + `unchanged` suppression; the `wait_until`,
`click_node`, `input_sequence`, `game_assert` MCP tools; `game/screenshot`'s
`max_dimension`). All verified live against the rendered example and prototype_19.

## Context

"More playtesting bang per token" — auditing where an agent's tokens actually go in an
observed session pointed at six cost centers, none of which was plan search (see
[0006](./0006-pre-flight-preconditions-and-no-pddl-planner.md) for why the planner idea was
declined):

| Cost center | Observed |
|---|---|
| Re-reading unchanged state after every input | `game/ui` re-dumped after every click/keypress; the most expensive repeated read |
| Blind polling loops ("wait for the lobby to load") | No sleep primitive for an MCP-only agent → blind sleeps or N re-calls |
| Multi-call UI interactions | "click Connect" = ui_tree → move_to → press → release → ui_tree ≈ 5 calls + reasoning |
| Vision tokens on overview checks | A full 1280×800 PNG (~1 MB base64) to answer "did it render" |
| Scripted input choreography | Walk/jump/turn combos: one call per step plus timing waits between |
| Verification cost | Reading full state/dumps and reasoning over them, per assertion |

## Decision

- **`game/ui` unchanged-suppression + filters.** The filtered node list is hashed
  (`DefaultHasher` over the serialized `nodes` array); identical to the previous read →
  `{unchanged: true, node_count, pointer, hovered_entities}` **without** `nodes`. The filter
  is part of the hash — a different filter is a different read. Filters: `clickable_only`
  (the find-the-button read) and `text_contains` (case-insensitive substring), applied
  before the ancestor-fold so a kept interactive row still folds its kept-label children.
  `refresh: true` forces a full dump (the agent may have lost the earlier one to context
  compaction). The MCP `ui_tree` tool passes the filters through.
- **`wait_until`** — server-side polling with a timeout: `{source: game_state|game_ui,
  path, equals, text_contains, interval_ms, timeout_ms}` → `{met, attempts, elapsed_ms,
  last}`. Conditions are deliberately dumb (JSON-path equals with numeric-lenient
  comparison; UI text substring): the agent decides *what to wait for*, the harness handles
  *time*. `last` carries the final payload so a timeout answer still leaves the agent
  grounded without another call.
- **`click_node`** — compound UI click: fresh dump → match a **clickable** node
  (`text_contains`/`entity`/`rect`, exactly one) → `move_to` its center → press Left →
  release Left (steps spaced `step_delay_ms`, default 50, so bevy's per-frame hover/click
  processing sees each step) → return the **post-click full dump** as the effect read.
  Fails readably ("no clickable node with text containing … — call ui_tree…") when nothing
  matches.
- **`screenshot` `max_dimension`** (clamped 64..=4096) — downscales the **encoded** PNG to
  fit the long edge (aspect preserved, Lanczos3), *after* the crop. The capture itself stays
  full-resolution, so a cropped region keeps full effective resolution; a `max_dimension`
  overview read costs ~4× fewer vision pixels (verified: 1280×800 → 18.7 KB vs 640×400 →
  6.6 KB encoded).
- **`input_sequence`** — scripted device-mock choreography: steps are device-mock calls
  (`{"keyboard": {…}}`/`{"gamepad": {…}}`/`{"mouse": {…}}`) or waits (`{"ticks": n}`,
  ~16.7 ms each at 60 Hz, `tick_ms` overridable; 100-step / 3600-tick budgets). Level-
  triggered mocks stay held exactly as the sequence leaves them — the tool's description
  tells the agent to budget explicit releases.
- **`game_assert`** — declarative verification: `{source, path, op, value}` /
  `{source: game_ui, text_contains}` expectations with `eq`/`ne`/`gt`/`gte`/`lt`/`lte`/
  `exists`/`text_contains` (numeric-lenient comparison shared with `wait_until`). Returns
  `{passed, passed_count, failed_count, failures: [{expectation, actual}], payloads}` —
  only mismatches come back with actuals; each referenced source is fetched exactly once.

**Layering rule that fell out (and is now the standing boundary):** tools that *orchestrate
or sleep* — `wait_until`, `click_node`, `input_sequence` — are **MCP-only**. A BRP handler
runs inside the app's frame loop, so it cannot sleep (the whole app would stall) and cannot
iterate over time; the rmcp tool thread can do both freely. Instant, single-shot tools stay
BRP methods (proxyable by any transport); orchestration lives in the MCP layer. The p19
server's `disabled_tools` mask interacts cleanly — orchestration tools hide with the rest.

## Alternatives considered

- **MCP resources instead of `read_guide`** — see
  [0004](./0004-agent-guides-bundled-into-the-binary.md); same reasoning applies here for
  `wait_until`-style polling surfaces.
- **Generic wait conditions over arbitrary BRP methods** (`{"method": "world/query",
  "expect": …}`): rejected — `game/state`/`game/ui` are the curated, small, stable surfaces
  (ADR 0002's boundary); polling raw queries re-opens the noise problem.
- **Input-sequence scheduling inside the app** (a runner system driven by ticks rather than
  wall-clock sleeps): more faithful to fixed-tick games, but requires a harness-side system
  with per-host tick-rate knowledge; wall-clock `tick_ms` with the 60 Hz default matches how
  hosts already document ticks (`ticks` ≈ 16.7 ms) and kept the feature host-agnostic.
  Revisit if fixed-tick determinism matters for a host's choreography tests.
- **Enforced `game_assert`** (auto-fail a session on a failed expectation): rejected —
  assertions are observations; the agent decides what a failure means.
- **Screenshot thumbnails by default**: rejected — silently lossy; the agent opts in per
  call with `max_dimension`.

## Consequences

- The common session shapes collapsed measurably: re-reads after inputs cost ~40 tokens
  (unchanged) instead of a full dump; "wait for InGame" is one call (verified: the p19
  connect→play transition was caught in 322 ms / 2 attempts); "click Connect" is one call
  (verified: matched the button, clicked through the real pointer pipeline, `game_state`
  flipped to `Lobby`); a choreographed hold-release is one call (verified: the host's own
  observer logged the exact hold window).
- **A latent bug surfaced during verification**: the built-in MCP tools double-prefixed
  their BRP targets (`game/game/ui`) since the `method_prefix` refactor — every tool call
  would have 404'd on a non-default prefix and worked by luck on the default. Fixed to bare
  names under `self.method()`; all tools re-verified. (Found because the new `ui_tree`
  params path shared the same helper.)
- `unchanged` suppression interacts with *any* node-field change — including
  `pointer_hovered` flipping when the mocked pointer moves over a button. That is correct
  (the dump genuinely changed) and doubles as implicit hover feedback, but agents should
  not be surprised that a pointer move alone can flip a suppressed re-read to a full dump.
- BRP-only agents (raw curl, no MCP) cannot use the three orchestration tools; the playbook
  documents `wait_until`/`click_node` as MCP-only and the BRP polling recipe (curl +
  `sleep`) remains the fallback. A future BRP-side long-poll variant would need a
  non-blocking design (task + watch) and was deferred.
- Tool count on `tools/list` grew to 14 for p19 (13 built-ins + `inject_input`) — schema
  rows cost tokens per session, but each new tool replaces a strictly larger recurring
  token pattern.
