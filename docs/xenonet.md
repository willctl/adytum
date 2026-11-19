# A cell as a sandbox kind on a start

Designed, not built. Nothing in Adytum or in XenoNet implements this yet.

XenoNet is a separate program that maps coding-agent sessions across a person's machines and decides who may do what. Adytum depends on none of it, and this note says where the two could meet. It changes none of XenoNet's authorization rules.

## The idea

XenoNet's daily-use plan adds a `sandbox` field to a start: the request, the signed act and what the adapter is given all carry it, and a start from anyone but the person with it missing or off is refused. The field there is on or off with allowed domains, mapped to each harness's own sandbox. A cell could be a second kind of value in that field: the start names an Adytum cell as the sandbox, and the session runs inside it.

The cell would hold the project folder and the identity's eidolon. The start names an identity. The receiving machine opens that identity's eidolon in a new cell on the identity's wall, with the project folder as a folder facet, and starts the harness inside the cell through Adytum's exec channel. What the session can reach is then what the cell holds: a machine boundary, not a harness setting. The browser facet, if the eidolon has one, is in the same wall and so reachable.

## Costs

A cell is a stronger sandbox than a harness's, and it costs more.

- The image needs the harness installed.
- A cell takes seconds to a minute to start.
- The session's hooks and MCP servers inside it must reach XenoNet's local door without a published port. Adytum publishes ports on 127.0.0.1 only, and never the automation port. That piece has no answer yet and may need a socket carried over the exec channel's duplex stream, as the browser's is in Eidolon.
- Which harnesses can run inside a cell at all is an adapter question.

## What it waits on

The `sandbox` field on a start. Until a start can carry a sandbox, the signed act has nowhere to name a cell. When the field exists, the cell kind adds a variant to it, a refusal for an adapter that can't run in a cell, and a doctor row for Adytum being present.

## What XenoNet would use

XenoNet would link Adytum's library crates rather than run the `adytum` binary:

- `adytum-model`, `adytum-policy` and `adytum-exec` as base crates any core crate may use.
- The drivers, `adytum-ledger` and `adytum-doctor` only from the program that composes XenoNet's pieces.

Adytum's side of this is unchanged: it enforces the wall it is given and never learns who the wall belongs to.

See [the design](design.md) for the rules Adytum enforces.
