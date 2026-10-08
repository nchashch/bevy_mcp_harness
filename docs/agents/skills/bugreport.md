# Skill: Filing bug reports

> Note: file paths in these guides (`docs/agents/…`, `crates/client/…`) refer to the
> **bevy_mcp_harness repository**, not the app being tested — the guides are served from the
> harness binary itself.

Read this before filing a bug report. Bug reports are the project's permanent
defect ledger: every reproducible flaw, regression, misbehavior, or design-level
hazard gets one Markdown file, numbered, dated, pinned to the exact code state it
was found in — and **retained forever, even after the bug is fixed** (fixed
reports are marked `Fixed` in place, never deleted). Fixed reports are regression
armor: they encode the repro, the root cause, and the fix so the same class of
bug is cheap to recognize the next time. Supplements (does not replace)
`AGENTS.md` and `docs/agents/skills/playtest.md` (whose harness drives the reproductions).

This skill is **generic** — it works for this harness crate itself and for any
Bevy project that hosts it. prototype_19-specific examples and conventions are
collected in §9.

If the host repo has a human-only documentation area, **never write into it** —
bug reports go in the agent-facing area only. **[p19]** enforces this as
`docs/humans/` (see §9).

## 1. When to file

- Any *reproducible* defect: crash, wrong behavior, desync, data loss, security
  hole, performance cliff, broken tooling, doc/code mismatch that cost real
  debugging time.
- Design-level hazards (e.g. a shared spawn point stacking joining players) are
  filed too: `Status` `Open`, with the Summary saying it is a by-design
  question, until the owner rules on it (then `By design` or a fix, per §7).
- **One bug per file.** Two symptoms with one root cause = one file (say so);
  two unrelated causes = two files, cross-referenced.
- File even when the bug is *already fixed by the time you write* — the report
  then documents the failure window and the fix (that is how `Fixed` reports
  accumulate history).

## 2. Layout and numbering

- One file per bug: `<bug-reports-dir>/bug_XXXX.md` (`bug_0001`, `bug_0002`, …),
  GitHub-flavored Markdown, tracked in git. GitHub renders it directly; there is
  no build step.
- **Find the next free number; never reuse or renumber.** Numbers are permanent
  identities — reports are retained forever, fixed or not, so other documents
  can cite `bug_0007` stably.
- The bug-reports directory carries a `README.md` explaining the directory and
  its history, with a ledger table (bug, summary, status). Add a row when you
  file a bug and update it whenever the report's `Status` changes; the report's
  own `Status` row stays authoritative. **[p19]**:
  `docs/agents/bug_reports/` + `docs/agents/bug_reports/README.md`.

## 3. Metadata table (top of every report)

Follow the playtest-report house style (`docs/agents/skills/playtest.md` §9): one H1
`# Bug 0007 — <one-line summary>`, then a two-column `| Field | Value |` table,
`##` section headings in the §4 order, fenced code blocks for commands and logs.
Escape a literal `|` in a table cell as `\|` and a literal `<word>` outside code
as `\<word>`.

Required fields:

| Field | Content |
|---|---|
| `Bug` | `bug_XXXX` |
| `Date discovered` | When first observed |
| `Commit (state actually running)` | Exact local `git log -1 --format="%h %s"` **plus a per-file list of uncommitted changes that were in the running binary** — pushed or not. The report must reflect the code that actually ran. Once fixed, append `**Fixed in** <hash> "<title>"` |
| `Discovered by` | Session/agent/human |
| `Component` | Module(s) involved (crate paths: for this harness `bevy_mcp_harness::brp`/`::mcp`/`::headless`; for the host game its own modules) |
| `Severity` | S1–S4 (see §6) |
| `Status` | `Open` → `Investigating` → `Fix in progress` → **`Fixed in <commit>`** / `Won't fix` / `By design` — updated **in place** as the bug moves |
| `Related` | Playtest reports (by number + finding, e.g. "playtest 0015 F3"), other bug numbers, AGENTS.md gap entries |

## 4. Required sections

1. **Summary** — one paragraph: what is broken, where, and the user-visible
   consequence. No root-cause speculation here.
2. **Steps to reproduce** — exact, copy-pasteable, from a cold start: launch
   lines, every harness call with its *actual* JSON payload, waits/sleeps, and
   the observation command that shows the failure. A repro someone cannot paste
   into a shell is not a repro. Prefer the QA harness (`game/state`, `game/ui`,
   BRP `world.query`) over pixel/screenshot inspection (see `docs/agents/skills/playtest.md`
   §6 — data over pixels), and note which host mode it applies to (render-less
   vs rendered-headless vs windowed — several bugs are mode-specific).
3. **Expected vs actual** — one line each, concrete (values, positions, log
   lines).
4. **Evidence** — verbatim log lines with timestamps, BRP dumps,
   entity/component listings. Attach what you saw, not a paraphrase. When a
   screenshot is genuinely the evidence (a rendering defect), attach an
   **annotated copy** — marks drawn from the capture's machine-readable geometry
   (`entities` table, `game/ui` rects) onto the full-resolution file on disk,
   which the coordinate tables map onto 1:1, saved as `<name>-annotated.png`
   beside the untouched original, with the caption stating what the marks mean
   and where the coordinates came from. Annotation is the illustration; the exact
   numbers (bounding box, depth, state values) belong in the report prose.
   Convention and working example: `docs/agents/skills/playtest.md` §6a.
5. **Root cause** — only what is *confirmed*. Hypotheses go here explicitly
   labeled `Hypothesis (not confirmed)` with the discriminating experiment that
   would confirm them. If you ran an ablation (stash the candidate fix, re-test),
   say so — ablations are what separate "the change next door caused it" from
   "my change caused it".
6. **Fix** (once fixed) — commit hash, files changed, one paragraph on the
   mechanism, and the verification evidence (the repro run going green).
7. **Follow-ups** — secondary bugs found while diagnosing, doc corrections
   applied, scope deliberately deferred (say why and where it is tracked).

## 5. Best practices (the canons)

- **Reproduce before filing.** Read `docs/agents/skills/playtest.md` and drive the app
  through the harness; verify the build actually rebuilt (a stale binary
  silently tests old code — this has cost multiple sessions). An unreproducible
  bug is still filed, marked `Not reproduced`, with everything attempted listed —
  that inventory is what eventually cracks it.
- **Distinguish observation from conclusion.** "HP stayed 100 after the attack"
  is an observation; "attacks don't work" is a conclusion that was wrong twice
  in prototype_19's history (the attacks were silently dropped by caster
  resolution; then `Selected` was never set headlessly). Write observations;
  keep conclusions in Root cause with their evidence.
- **Corrections, not rewrites.** If a filed root cause turns out wrong, *append*
  a correction note with the new evidence and strike/annotate the old text —
  never silently rewrite. The wrong-turn is diagnostic information (it records
  which experiment discriminated).
- **Attribute races and multi-cause failures by ablation.** When two changes are
  in flight, stash one and re-run; say in the report which commit was stashed
  and what the control run showed.
- **A fix updates four places**: the bug's `Status`/`Fix` section, its row in
  the bug-reports ledger, `AGENTS.md` (the sections that described the buggy
  behavior), and — if the repro revealed a harness/API gap — `docs/agents/skills/playtest.md`
  or the harness itself (a custom BRP method / `HarnessTool` on the host, or an
  upgrade here). A bug fix that leaves stale documentation behind is an
  unfinished fix.
- **Severity honestly, not aspirationally.** Severity reflects player/user
  impact today, not how interesting the bug is.
- **Fixed means verified.** `Fixed in <commit>` requires the repro run observed
  green after the fix — "it compiles" is not verification. If verification is
  blocked, the report stays `Fix in progress` with the blocker named.

## 6. Severity guide

| Level | Meaning |
|---|---|
| S1 | Crash, data loss, or the app unusable for its primary flow |
| S2 | Core gameplay/product flow broken, no crash |
| S3 | Degraded experience, correct behavior reachable another way |
| S4 | Cosmetic, tooling friction, doc/code mismatch |

## 7. Status lifecycle

`Open` → (`Investigating`) → (`Fix in progress`) → `Fixed in <hash>`; or
`Won't fix` / `By design` (with the owner's ruling quoted). Reopening is
allowed: flip back to `Open` with a note on the new evidence and the new commit —
the number never changes.

## 8. Cross-references

- Playtest reports live beside the bug reports (see `docs/agents/skills/playtest.md` §9);
  cite as "playtest NNNN F<finding>".
- Bugs that produced harness/tooling fixes should name the tool method or tool
  that now covers them — e.g. prototype_19's `game/select` exists because
  headless clients cannot aim a crosshair, and this harness's `game/ui` +
  `game/mouse move_to` pair exists so an agent never has to guess pixel
  coordinates from a screenshot.
- Backfilling known-open issues into this directory is encouraged — defects
  documented in prose elsewhere (AGENTS.md gaps, README caveats) deserve
  numbered, dated reports with repros.

## 9. prototype_19 specifics (examples of the conventions above)

These are kept as worked examples of the canons — the numbers cited are stable
identifiers in prototype_19's ledgers (`docs/agents/bug_reports/`,
`docs/agents/playtests/`, `docs/agents/skills/`), and the harness in this
repository was extracted from that project's `crates/client/src/dev/tool_api.rs`.

- **Directory layout**: `docs/agents/bug_reports/bug_XXXX.md` +
  `docs/agents/bug_reports/README.md` (ledger); playtests in
  `docs/agents/playtests/`; skills in `docs/agents/skills/`; ADRs in
  `docs/agents/adr/`. `docs/humans/` is human-only.
- **Worked severity examples**: S1 — death-path client panic (playtest
  0015/0016); S2 — caster resolution silently dropping every attack/kill;
  S3 — joiner hover (spawn stacking), room-filtering leaks; S4 — `Npc` type
  invisible to BRP queries, stale doc comments.
- **Worked observation-vs-conclusion example**: "attacks don't work" was the
  wrong conclusion twice — once because caster resolution silently dropped
  attacks, once because `Selected` was never set headlessly (which is why
  `game/select` exists).
- **Worked ablation examples**: playtest 0014/0015 (stash one in-flight change
  and re-run; 0015's F3 would have been misfiled otherwise). The
  "misleading `ServerMutateTicks` error" that looked like the root cause was
  collateral — the actual cause was `--no-render` missing `SyncWorldPlugin`
  (playtest 0016), which unblocked bug_0002's death-path panic verification.
- **Worked correction example**: playtest 0014's joiner-hover misdiagnosis,
  re-verified and corrected in playtest 0018.
- **Component field style**: `p19_server::combat`,
  `p19_client::lifecycle/networking`, `dev::tool_api` — exact module paths of
  the code that actually ran.
- **Repro env details**: `BEVY_ASSET_ROOT` for isolated asset sets
  (`docs/agents/skills/playtest.md` §12), client modes (`--no-render` vs rendered vs
  windowed — several p19 bugs were mode-specific), the `dev-tools` cargo
  feature gate for the tool API.
- **Known open items at extraction time** (documented in p19's AGENTS.md
  "Known gaps" rather than yet filed as bugs): disconnected players' characters
  never despawned (zombie `ClientInGame` blocking clean reconnect), shared
  spawn point stacking joiners (by-design question, playtest 0018), replay
  movement-rate mismatch (ADR 0013), KCC coasting (no ground friction),
  VR controller-grip tracking suspect (bevy_xr_utils 0.6.0 has
  `suggest_action_bindings` commented out).
