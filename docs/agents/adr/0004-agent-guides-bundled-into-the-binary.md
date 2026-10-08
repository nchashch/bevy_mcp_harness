# 4. Agent guides bundled into the binary and served via `read_guide`

Date: 2026-10-07

## Status

Accepted — implemented (`include_str!` of `docs/agents/skills/playtest.md`,
`docs/agents/skills/bugreport.md`, `AGENTS.md`, `README.md`; the `read_guide` MCP tool).

## Context

The guides are load-bearing: the playtest playbook (`skills/playtest.md`) is the distilled
recipe for launching an app headlessly, driving it through the tool API, and avoiding every
trap hit so far — and the workflow assumes the agent reads it *before* driving. But the agent
that most needs it is one connected to an app's MCP server over the network, which may:

- have no checkout of this repository at all (the app is a `cargo add` dependency),
- not know the playbook exists,
- or have only MCP as its transport (no shell access to the app's filesystem).

Serving the guides from the same MCP server removes all three: `cargo add
bevy_mcp_harness` + one plugin line, and the agent can `tools/list` → call `read_guide` —
zero setup. The guides are exactly the artifact an agent reads; making them a tool was the
natural completion of "the MCP server is the agent's only required interface."

## Decision

- `include_str!` (compile-time text embedding, not runtime paths — no CWD assumptions, ships
  inside the binary) of the two skills plus `AGENTS.md` and `README.md`.
- One **`read_guide` MCP tool** rather than one tool per guide:
  - no arguments → an index (guide names, byte sizes, `##` section headings);
  - `{"guide": "playtest"}` → the whole document;
  - `{"guide": "playtest", "section": "6"}` → one `##` section, matched by number or title
    prefix (`"4b"`, `"reporting"`), `###` subsections included in their parent.
- Section extraction matters: the playtest guide is ~50 KB (~12 K tokens); a section read is
  ~2 KB. Agents routinely need one section (failure modes, input semantics) mid-session.
- Unknown guide/section → a descriptive error listing the valid names, so a confused agent
  self-corrects in one turn.
- The tool description itself instructs: "Read the playtest guide BEFORE driving the app
  headlessly" — the bootstrap is self-contained even for an agent that has never heard of
  the guides.
- Guides carry a preface noting that file paths inside refer to the harness repository (the
  p19-specific sections cite that repo's `docs/agents/…` tree, which a host checkout won't
  have).

## Alternatives considered

- **MCP resources** (`resources/list`/`resources/read`) — the protocol's native
  "read-only documents" concept. Deferred: tool calling is the transport every agent in the
  observed sessions used and trusts; resources remain a plausible future addition, but
  dual-serving the same content doubles the surface to test for zero new capability.
- **One tool per guide** (`read_playtest`, `read_bugreport`, …): more schema rows, no
  benefit; the index/section semantics need a home either way.
- **Fetch from GitHub at runtime**: rejected — network dependency in a localhost QA tool,
  version skew between the running harness and the fetched docs, and an unnecessary
  egress requirement for air-gapped CI hosts.

## Consequences

- The guides are compiled into every host binary that embeds the harness (~60 KB of text —
  irrelevant next to a Bevy binary) and are version-locked to the crate: the playbook an
  agent reads always matches the tool API the binary serves. This is a feature — the
  playbook documents the tool API, so they must move together.
- Guide content is packaged with the crate on crates.io; the `docs/agents/skills/` files
  must stay in the package (they are not gitignored).
- The guides' p19-specific sections are clearly scoped ("prototype_19 specifics"), so a host
  agent reads them as history rather than as its own playbook.
- Maintenance rule: a tool-API change that invalidates guide text must update the guide in
  the same change (enforced by review, not mechanically — the guides are compiled in, so at
  least stale paths fail visibly when the docs move).
