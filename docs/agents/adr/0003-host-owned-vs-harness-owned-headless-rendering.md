# 3. Host-owned vs harness-owned headless rendering (`OffscreenMode`)

Date: 2026-10-08

## Status

Accepted — implemented (`McpHarnessConfig::offscreen: OffscreenMode::{Windowed, HostManaged
(handle), Owned(size)}`, the crate-internal `CaptureTarget` resource). Supersedes the
brief-lived `offscreen_target: Option<Handle<Image>>` config field from the first p19
migration attempt, which produced a public resource duplicate.

## Context

The prototype's headless mode is more than "render into a texture": a stack of interlocking
pieces makes a window-less app produce correct frames — the shared offscreen texture
(with `COPY_SRC` for screenshot readback), camera retargeting that also forces
`Projection::set_changed()` (otherwise `camera_system` never recomputes `target_info` for
late-spawned cameras and they render nothing, permanently), exactly-one-clearer +
`IsDefaultUiCamera`-drawn-last ordering invariants, the render-less `target_info` shim, and
the agent cursor overlay.

The migration to a reusable crate hit a hard constraint: **p19's headless machinery is not
dev-only**. Its render-less branch runs regardless of the `dev-tools` feature (game code —
`lifecycle/loading`, the player spawn gate — reads `NoRenderMode`), so it cannot move into an
optional harness dependency without either dragging the harness (and its rmcp/tokio/axum
deps) into player builds, or duplicating the stack.

The first attempt settled for duplication: config took `offscreen_target:
Option<Handle<Image>>`, the harness inserted its own `OffscreenRenderTarget` wrapping the
host's handle, and the host kept its own resource. Two public resources, one handle — and a
subtle ownership question (whose retarget systems run?) papered over by "both run, it's
benign."

While verifying this path, two landmines surfaced that shaped the final design:

- **`init_asset::<A>` is not idempotent.** It constructs a *fresh* `Assets<A>` and
  `insert_resource` *replaces* an existing store — divorcing it from handles the server
  already issued. Observed as `index out of bounds: the len is 1 but the index is 5` in
  bevy_asset's `handle_internal_asset_events`, killing a fully rendered host at frame 0.
- **Visibility propagation arrives via the render app** (`RenderPlugin` → `CameraPlugin` →
  `VisibilityPlugin`). A render-less host never gets it: every UI node stays
  `InheritedVisibility(false)`, `game/ui` dumps nothing, and picking is blind.

## Decision

One enum, three ownership shapes — `McpHarnessConfig::offscreen: OffscreenMode`:

- **`Windowed`** — captures from the primary window; no machinery. (The prototype only ever
  had this as the non-`--mcp` fallback; the crate makes it a first-class mode, and
  `game/mouse`'s pointer targeting follows — offscreen or window.)
- **`HostManaged(handle)`** — for hosts with existing headless machinery that must also run
  without this crate compiled in (p19's case). The host hands its texture's handle via
  config; the harness stores it **only** in a crate-internal `CaptureTarget` resource (which
  *all* harness systems — screenshots, the `game/ui` camera filter, the mocked pointer, the
  cursor overlay — read), inserts **no public resource of its own**, and adds only the cursor
  overlay. The host keeps ownership of retargeting, the bootstrap UI camera, clear/order
  management, and the `target_info` shim.
- **`Owned(size)`** — the harness owns the full stack: creates the target, spawns the
  bootstrap UI camera, adds the retarget chain, and (with `no_render`) the shim. The
  standalone-headless case (this crate's own example).

Plus the two defensive measures the verification runs forced:

- All `init_asset` calls are `contains_resource`-guarded, with the replace-store pitfall
  documented at the call site.
- `VisibilityPlugin` (and the `Assets<Mesh>`/`Assets<SkinnedMeshInverseBindposes>` stores its
  bounds systems unconditionally validate) added when missing — guarded the same way.

## Alternatives considered

- **Harness owns everything, always** (p19 adopts the harness's `OffscreenRenderTarget`
  type wholesale, deletes its own machinery). Cleanest code, rejected: it would make the
  harness dependency mandatory in p19 player builds (the camera machinery is not
  feature-gated), contradicting the dev-tools gating model, and would tie the game's camera
  code to an external QA crate.
- **Move p19's machinery behind its own non-dev module and keep two types** (the first
  attempt's shape): works, but two public resources with one handle is duplicated state and
  invites the "whose retarget runs" question. The `CaptureTarget` internal resource removes
  the public duplication; the remaining duplication (host resource + internal view) is
  inherent to host ownership.
- **Trait-based pluggable capture target** (`dyn CaptureSource`): over-engineered — there is
  exactly one target shape (an image handle) and exactly two owners.

## Consequences

- p19's migration ended with **zero changes** to its camera machinery, and the harness adds
  only the cursor overlay in HostManaged mode. Both repos compile their player paths without
  the harness.
- The shim is owned by whoever owns the cameras: the harness adds `shim_camera_computed` only
  in `Owned`+`no_render`; a `HostManaged` host feeds its own (p19's pre-existing shim).
- `init_asset`'s replace-store semantics are now a documented trap in this crate (guarded
  call sites) — the kind of thing that otherwise gets rediscovered by every consumer.
- Cost: `OffscreenRenderTarget` (public, `Owned`-mode host-facing) and `CaptureTarget`
  (internal) are near-duplicates. Accepted — one is API, the other is plumbing; a trait or a
  type alias would obscure ownership.
