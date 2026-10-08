# 10. Batched debug views in one tool call (`debug_views` parameter)

Date: 2026-10-08

## Status

Accepted — implemented (the `debug_views` parameter on the MCP `screenshot` tool; a
path-matched poll loop per view). Verified live on prototype_19 with all three view classes
in one call: `["physics", "wireframe", "depth"]` returned 3 correctly-attributed image
blocks (physics 623,748 B / 4,096 green px; wireframe 623,326 B / 2,051 green px; depth
16,868 B / grayscale).

## Context

ADR 0009 gave screenshots a `debug_view` parameter: one call → one capture in one debug
mode. An agent inspecting rendering internals typically wants several views of the *same
scene state* — depth to check occlusion, normals to check shading, wireframe to check
geometry — and comparing them only makes sense if the game hasn't changed between captures.

With the single-view tool, "three views of the same moment" costs three MCP round trips,
each with its own capture/poll cycle, and the game advances between them (the player falls,
an NPC moves, an animation ticks). The captures are then of *different* scene states, which
defeats the comparison the agent was trying to make. Each round trip also costs session
overhead (a full tool-call envelope per capture).

The latency structure makes this worse than it sounds. Per ADR 0009, overlay and wireframe
captures are **deferred** (apply → warm-up frames → capture → restore), so a single capture
already takes several frames; a three-view sequence is three deferred pipelines back to
back. And there is a race: the BRP handler returns a `capturing` path, the deferred capture
completes a few frames later, and a naive poll for "the newest PNG" can attribute the
*previous* view's capture to the current view.

## Decision

The MCP `screenshot` tool accepts `debug_views: Option<Vec<String>>` in addition to the
existing single `debug_view`. Semantics:

- **`debug_views` absent** → the original behavior: one capture, one image block.
- **`debug_views: [..]`** → for each view in order:
  1. Call BRP `game/screenshot` with that view's `debug_view`; the response's `capturing`
     path is the **expected path** for this view.
  2. Poll BRP `game/screenshot/get` until the newest capture's path **equals the expected
     path** and `ready: true`, then take that PNG.
  3. Push one image content block preceded by a `debug_view: <name>` text block.
- One MCP call returns N images; the game state drifts only by the frames between
  consecutive captures within the batch (unavoidable — each deferred capture needs its own
  warm-up), not by N full tool-call round trips.

**Path matching is the correctness core.** ADR 0009's single-view flow could poll for "a
newer file" because there was only one capture in flight. In a batch, three captures are
in flight in sequence; polling for "newest" would hand back the *previous* view's PNG
whenever the current view's deferred capture hasn't finished yet (observed: the tool
returned the depth PNG when normals was requested). Matching on the exact path returned by
this view's own `capturing` response eliminates the race: the poll cannot complete until
*this view's* file exists.

**The physics view composes with batching.** ADR 0009 left physics as a persistent toggle;
in a batch it is just another view — toggled on, captured, left on (documented behavior).
The batch loop treats it like any other view; no special-casing in the MCP layer. The
`physics` name is filtered out of the overlay-mode parse (alongside `wireframe`) and
handled by its own BRP branch (ADR 0009's structure, unchanged).

**The camera-resolution bug found during live verification was fixed as part of this
work**: `entities_on_screen` resolved the highest-order *any* camera, which on prototype_19
is the UI camera — an orthographic projection that cannot project 3D world positions (0
entities returned). It now filters for `Camera3d` (ADR 0008's contract, corrected), and
projects `Aabb` corners transformed through each entity's `GlobalTransform` — the AABB is
local-space, and projecting local coordinates directly placed every entity behind the
camera (0 entities). Both bugs were invisible until a live in-game check; both are covered
by the same live-verified flow.

## Alternatives considered

- **One MCP call capturing all views server-side in a single frame** (N readbacks of the
  same frame): rejected — the overlay/wireframe modes need *different camera state* per
  capture (per-camera overlay component + prepass markers; a global wireframe config), and
  the warm-up frames mean the pipelines can't all be live on the same frame. N sequential
  deferred captures is the honest structure; the batch just removes the MCP overhead
  between them.
- **A `game/screenshot/batch` BRP method doing the loop inside the game**: rejected — the
  loop needs MCP-layer knowledge (content blocks, tool-call envelopes); the BRP layer
  should stay a thin capture primitive. The MCP tool composing it keeps BRP simple and the
  agent-facing API rich.
- **Polling for "newest PNG" per view (no path matching)**: rejected — this is exactly the
  race described above; the first live batch test returned the previous view's capture.
  Path matching costs one string comparison per poll and removes the entire failure class.
- **Parallel captures** (spawn all N captures, then poll): rejected — the views mutate
  shared camera state (overlay component, prepass markers, wireframe config); interleaved
  captures would race on the same camera's overlay and restore. Sequential captures with
  restore-per-capture are correct by construction.

## Consequences

- One MCP call → N images: the token cost of N image blocks is unchanged (PNGs still
  dominate), but the *session* cost drops from N tool calls to 1, and the captures are
  temporally adjacent (same scene within the batch's own frame span).
- Latency per view stays what ADR 0009 established (deferred capture with warm-up); a
  3-view batch costs roughly 3× a single capture in wall time, not 3× plus 3 round trips.
- The race class ("capture X's poll returns capture Y's PNG") is closed by construction —
  path matching cannot complete early.
- The first-ever batch in a session still pays the pipeline-compile cost for each view's
  first capture (ADR 0009's warm-up); subsequent batches reuse the compiled pipelines and
  are much faster (verified: the second 3-view batch took ~1.1 s vs ~18 s for the first).
- `debug_view` (singular) is unchanged for the common "just depth, once" case;
  `debug_views` (plural) is the batch form. The tool description documents both.
- The `entities_on_screen` corrections (Camera3d filter, local→world AABB transform) are
  part of ADR 0008's contract, retroactively — the entity table embedded in screenshot
  responses and the standalone method both benefit.
