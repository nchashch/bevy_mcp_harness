# Architecture Decision Records

Short, dated writeups of specific significant decisions made in this project — the
alternatives considered, why one was picked, and what it cost. These are deliberately
separate from [`AGENTS.md`](../../AGENTS.md): `AGENTS.md` describes the *current* state of
the code and gets rewritten as things change, which means the reasoning trail behind a past
decision (what was tried first, why it didn't work, what the tradeoff actually was) tends to
get compressed away or dropped entirely once it's no longer "current." ADRs are meant to stay
put — even once a decision is superseded, the old record stays as history rather than being
edited to match.

Each record uses the same shape: **Context** (the problem, forces, constraints), **Decision**
(what was actually done), **Alternatives considered**, **Consequences** (what it bought, what
it cost, what's still open).

Provenance note specific to this repository: the harness was **extracted** from
prototype_19's `crates/client/src/dev/tool_api.rs` (decision record
[0009](https://github.com/nchashch/prototype_19/blob/main/docs/agents/adr/0009-agent-tool-api-via-brp.md)
in that repository, plus 0010–0012 for the input-mocking/vision/no-render layers). The ADRs
here cover this crate's own decisions: the extraction boundary, the generalizations, and
everything learned from the first real host adoption (prototype_19 itself).

## Index

| # | Title | Status |
|---|-------|--------|
| [0001](./0001-extract-the-agent-tool-api-into-a-reusable-crate.md) | Extract the agent tool API into a reusable `bevy_mcp_harness` crate | Accepted |
| [0002](./0002-generalization-boundary-what-stays-generic.md) | Generalization boundary: what stays generic, what the host provides | Accepted |
| [0003](./0003-host-owned-vs-harness-owned-headless-rendering.md) | Host-owned vs harness-owned headless rendering (`OffscreenMode`) | Accepted |
| [0004](./0004-agent-guides-bundled-into-the-binary.md) | Agent guides bundled into the binary and served via `read_guide` | Accepted |
| [0005](./0005-extension-ergonomics-from-the-first-host-adoption.md) | Extension ergonomics from the first real host adoption (prototype_19) | Accepted |
| [0006](./0006-pre-flight-preconditions-and-no-pddl-planner.md) | Pre-flight preconditions (`plan_check`) — and no PDDL planner | Accepted |

New records should be added to this index in the same commit that adds the file.
