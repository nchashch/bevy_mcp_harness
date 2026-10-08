# 6. Pre-flight preconditions (`plan_check`) — and no PDDL planner

Date: 2026-10-08

## Status

Accepted — implemented (`PreconditionFn`,
`register_game_method_with_precondition`, the `{prefix}/plan_check` BRP method + `plan_check`
MCP tool; the built-in `screenshot` declares one; prototype_19 declares preconditions for all
five of its methods). A full planner (the original proposal: something like the `ferroplan`
PDDL crate exposed as an API) was considered and **declined** for now.

## Context

Observed agent failure modes across the p19 sessions were consistently *not* search-shaped.
The agents mis-sequenced calls because the game's implicit state machine was invisible:
`select_level` is silently rejected while a level is loaded; `InGameRequest` is dropped while
the server is `Loading`; `game/input` needs a connected, in-game player with a replicated
input context; `attack`/`kill` need a selected target. Every one of these was caught *after*
the failed call — the agent burns a turn, reads an error, retries. With a multi-step flow
(connect → levels → select_level → play → input), one wrong assumption wastes a whole
sequence, and the knowledge ("you must X before Y") lived only in the playbook prose.

The original proposal was to expose a classical planner (PDDL, FF heuristic — e.g.
`ferroplan`) so agents could plan playtests. Analysis said the planning step is not where the
value is:

- A planner needs a formal domain: actions with preconditions/effects, symbolic state. For a
  game under test, nobody has one; authoring and maintaining a PDDL model in sync with the
  game is a new, permanent cost that replaces "prompting the agent" with something worse.
- Game QA state is continuous (positions, timers, physics) and observed through a lossy
  sensor (`game/state` sampled at poll time, netcode in between) — a poor fit for
  deterministic-effects planning.
- The observed mistakes were precondition violations, not search failures. ~80% of the
  planner's value is available from just *declaring and checking preconditions* — no domain
  model, no search.

## Decision

- **`PreconditionFn`** — `Arc<dyn Fn(&World, Option<&serde_json::Value>) -> Result<(),
  String> + Send + Sync>`: a `&World` check plus the *intended params* (event-shaped methods
  key requirements off the params — `game/trigger`'s requirements differ per event).
- **`register_game_method_with_precondition`** — the one-liner registration with an optional
  declared precondition; preconditions live in a `GamePreconditions` resource keyed by full
  method name.
- **`{prefix}/plan_check`** BRP method + **`plan_check`** MCP tool: given
  `calls: ["game/trigger", {"method": "game/input", "params": {...}}, …]`, returns per-call
  `ok: true` or `ok: false` with the `reason` (unknown method → the registered list; declared
  precondition unmet → its message). Methods without a declared precondition report
  `ok: true, precondition: "none"` — the harness vouches only for what was declared.
- **The built-in `screenshot` declares one precondition** (rendering enabled + a capture
  target exists) — the one built-in whose failure an agent otherwise discovers by burning a
  poll cycle.
- **Advisory by design**: declared preconditions are surfaced by `plan_check` only; calling
  the method still runs its own checks, which remain the source of truth. No enforcement
  wrapper, no double-error paths.
- **The tool description teaches the use**: "Use this to plan a multi-step flow and catch
  state-machine mistakes cheaply," and the playtest guide's flow adds a pre-flight step.

## Alternatives considered

- **A PDDL planner exposed as a tool** (the original proposal): declined — see Context. Not
  rejected forever: the extension points already allow a companion crate that consumes
  `game/state`-shaped snapshots and plan-shaped custom methods, if a host ever produces a
  stable symbolic model worth planning over. The harness's job is grounding — honest state,
  typed actions, readable failures — and that is what made the sessions work.
- **Enforce preconditions at call time** (wrap every registered system; reject calls whose
  precondition fails): rejected for now — it doubles the error paths (a call could fail with
  "precondition" while the handler would have succeeded on a race-free re-read), and the
  in-method checks are already the experienced truth. Revisit if agents are observed to
  ignore `plan_check` and burn calls anyway.
- **Checklist/task-queue state in the harness** (a `plan` tool recording steps, `check_off`
  as the agent progresses): plausible future work for surviving context loss on long
  playtests; deliberately not built — agents already keep plans in their own context, and
  the failure mode (context loss mid-playtest) is better served by the reporting discipline
  (§9 of the playtest guide) than by stateful server-side planning.

## Consequences

- Agents can pre-flight a whole intended sequence in one call and learn *why* each step
  would fail — the state machine becomes machine-checkable at the surface where it matters,
  without anyone writing a formal domain. Verified live on p19: fresh client reports
  `levels`/`select_level`/`input` failing with their reasons; after connect, `play` flips
  ok while `attack` still fails "no target selected"; in-game, `input` flips ok.
- Preconditions are **optional and unenforced** — a host that declares none gets `ok: true,
  precondition: "none"` for its methods; the harness never blocks a call on a stale or
  missing declaration.
- The precondition fns must be `&World`-only (no `world.query`, which needs `&mut World`) —
  they use resource reads and `iter_entities()` + `entity.get::<T>()`. Documented by
  example in p19's preconditions.
- Open: if hosts routinely declare rich precondition graphs, a *compositional* check ("check
  this sequence as a whole, including interleavings") might earn its keep. Deferred until
  observed.
