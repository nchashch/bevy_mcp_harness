# 11. Always-persist full-resolution captures, and the annotation workflow

Date: 2026-10-09

## Status

Accepted — implemented (captures save the full-resolution, uncropped frame; `crop`/
`max_dimension` shape only the *served view*, applied at poll time; the `<capture>.json`
sidecar carries the per-frame `entities` table and the `alignment` block; guide §6a
documents the annotation convention). Verified live: a `max_dimension: 640` capture serves a
640×400 view while the file stays 1280×800; a crop+downscale view is 200×150 with the file
still full-res; an annotation drawn from the sidecar's `entities` bbox lands on the
full-res file with no scaling math.

## Context

Two consumers read a screenshot, and they want different things from it:

- **The agent (vision model)** — wants the cheapest image that answers its question. It is
  the most expensive reader: every pixel of `png_base64` costs vision tokens, and it looks
  at captures repeatedly while a test runs. ADR 0007's `max_dimension` (downscale the
  encoded PNG) and the `crop` param exist for this.
- **The human reading the report** — wants the highest-fidelity evidence. A finding
  illustrated by a 640-pixel-wide downscaled capture is a worse artifact than the
  full-resolution frame; a cropped sub-rect hides context the reader needs to judge the
  finding ("what else was on screen?").

Before this decision these two readers were conflated: the `max_dimension` downscale and the
`crop` were applied **at save time**, so the persisted file *was* the downscaled/cropped
view. The human-browsable record permanently inherited the agent's token-saving choices.
Worse, this silently poisoned the annotation workflow (a natural extension of ADR 0008's
entity-to-pixel correlation: draw the entity boxes/labels onto the capture to illustrate a
finding). The coordinate tables — `entities`, `game/ui` rects — are in full-resolution
capture space, but the file on disk could be half-size or a sub-rect; an annotator drawing
table coordinates onto the file landed at the wrong scale or the wrong place, with nothing
in the response to tell it so. Two failure modes were identified: the sidecar carried no
per-frame `entities` table at all (only the poll response did — and `entities_on_screen`
projects the *current* frame, so the data was unrecoverable post-hoc), and the
crop/downscale misalignment was silent.

## Decision

**Split the two readers.** The file on disk always records the full-resolution, uncropped
capture; the agent's token-efficient look is a *derived view* served at poll time.

- `save_encoded_to_disk` encodes the captured frame unmodified. No crop, no resize — ever.
- `crop` / `max_dimension` move to **poll time**: `encode_served_view` decodes the full-res
  file, applies the request's crop (clamped to bounds) then the downscale (Lanczos3, aspect
  preserved — crop before resize, so a region keeps full effective resolution), and the
  result is what `png_base64` serves. The request params persist in a path-matched
  `LastCaptureParams` resource (written by `game/screenshot` at request time); a poll of a
  file that isn't the most recent request's path serves the full frame — a stale entry can't
  shape the wrong capture.
- The **`alignment` block** (in `game/screenshot/get` responses and the sidecar) describes
  both renderings: `png_size` — the file, which the coordinate tables now map onto **1:1**
  (no scaling math for annotators); `capture_size` — the render target; and `view` — the
  served base64's geometry (`size`, `crop`, `max_dimension`,
  `coordinate_scale` = multiplier from table coordinates to view pixels), for correlating
  what the agent saw.
- The **sidecar is a self-contained annotation source**: it now carries the per-frame
  `entities` table and the `alignment` block beside `state` and the PNG path. A later
  session, a human, or the report flow annotates an existing capture without any
  harness calls — and without the risk of re-deriving geometry for the wrong frame.
- The **annotation convention** lives in the bundled playtest guide (§6a) and is
  cross-referenced from the bugreport guide's Evidence section:
  1. Coordinates from the tables, never from the image — `entities` bboxes and `game/ui`
     rects; reasoning picks *which* row to illustrate, the drawing is mechanical.
  2. Annotate the file on disk (full-res, 1:1), not the served view.
  3. Draw with a real imaging library (PIL/Pillow) — boxes + text labels; working example in
     the guide.
  4. Annotate a *copy* named `<name>-annotated.png` beside the untouched original; report
     captions state what the marks mean and where the coordinates came from.
  5. Annotations illustrate; they never replace the data — exact numbers go in the report
     prose.

## Alternatives considered

- **Save both files** (full-res `X.png` + downscaled `X-view.png`): rejected — file clutter
  in the persistent record for something one render pass computes on demand; the view is
  ephemeral by nature (the agent looks at it once), the file is permanent.
- **Crop still applied to the file** (only downscale moved to poll time): rejected — the
  same "the record inherited the agent's framing choice" problem; a cropped file loses
  context no post-hoc process can recover. "Original always saved" reads most naturally as
  the whole frame.
- **An in-harness annotation tool** (BRP method or MCP tool that draws boxes): rejected —
  annotation is report-authoring, not capture; the agent has PIL and the ground-truth
  coordinates, so a harness feature would duplicate a drawing library behind a
  JSON-RPC call (and servers embedding the harness would carry an imaging surface they
  don't need). The harness's job is the *geometry*; drawing belongs to the annotator.
- **Coordinate tables in view space** (serve entities already scaled to the view): rejected
  — two spaces for one truth invites drift, and the view is the less stable consumer
  (changes with `max_dimension`); tables stay in capture space, and the one derived number
  an agent might want (`view.coordinate_scale`) is precomputed in `alignment`.
- **Guides only, no API change** (tell the agent to capture full-res separately, annotate
  that, downscale afterwards): rejected — it makes the workflow a convention to remember
  instead of a property of the tool; and the missing sidecar `entities` made post-hoc
  annotation impossible regardless.

## Consequences

- The persistent record is uniform: every capture on disk is the full frame at capture
  resolution. Human readers of reports get high-res evidence; the raw capture next to an
  annotated copy shows exactly what rendered.
- Token costs are unchanged — the served view carries the same crop/downscale economies as
  before; only its construction moved from save time to poll time.
- Poll latency gains a decode+resize per *first full serve* of a capture (unchanged-
  suppression still short-circuits re-polls on the file's bytes before any decode);
  measured negligible (~10s of ms per capture, ~3 captures per batch).
- The annotator's workflow simplifies to draw-on-the-file: ADR 0008's pixel space and the
  persistent artifact are now the same space. The guide's example no longer multiplies by a
  scale factor.
- `alignment` was reshaped (unreleased since 0.3.0 — the block is one release old); the
  top-level `coordinate_scale` moved into `view`, since the file it used to describe is
  now always 1:1.
- Open: windowed hosts still have no `capture_size` (the harness never reads window state),
  so a windowed, uncropped, downscaled *view* reports `coordinate_scale: null`. Harmless for
  the annotation flow (the file is 1:1 regardless); noted in the guide.
