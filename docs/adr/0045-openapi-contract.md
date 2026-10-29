# ADR-0045: The query API is described by one OpenAPI 3.1 file, checked against the code

## Status
Accepted, 2026-10-25 (docs/pre-public-loop.md PP6; the project owner 2026-10-25: "yes do open api standard").
Implemented by `docs/api/openapi.json`, `scripts/check-openapi.py` (run by `scripts/check-docs.sh`),
`crates/onyx-api/src/openapi_check.rs` (tests only), the node route `GET /v1/openapi.json` and the
website's API Reference (`apps/web/src/lib/components/ApiReference.svelte`, `apps/web/tests/pp6.mjs`).

## Context
The API was described in prose (docs/spec/delivery.md) and on a hand-written website list of five
routes. The node serves 17 routes; the list showed five. The prose error table missed 11 of the 67
codes the code returns. A POST-only route answered a wrong method with "only GET is supported" and
`Allow: GET, HEAD`. Nothing compared any description with the code, so each change could drift.
API clients and AI agents, the first users we plan for, read OpenAPI.

## Decision
1. **One file.** `docs/api/openapi.json` (OpenAPI 3.1.0, JSON so every part of the repository reads
   it without a new dependency) describes every route: parameters, request bodies, every status
   each route can answer, response schemas, headers and the x402 offer. `x-onyx-error-codes` lists
   every stable error code with its status; `x-onyx-paid` marks the priced routes. It is edited by
   hand; it is the source of truth for the description, the code is the source of truth for
   behaviour.
2. **Checked both ways, on every check-all** (`scripts/check-openapi.py`): the spec's paths and
   methods are exactly the code's `.route(...)` calls; every error code the code returns is listed
   at its status and every listed code exists (proposal validation codes from `onyx_intake`); the
   delivery spec's error table names every code; Caddy proxies every path with its method; the
   website's copy is byte-identical. A self-test plants one fault per rule.
3. **Answers checked against the schemas.** Every response the API's tests receive (the library
   tests and the Bitcoin, pools and reserve suites: all 18 operations at their success status, and
   most error statuses) passes through `openapi_check::check`: the status must be one the
   operation lists, the body must match its schema (types, required keys, no unlisted key where the
   schema is closed, enums, patterns), and an error code must be listed at that status. A schema
   keyword or pattern the checker does not know fails the test, so the file cannot outgrow it.
4. **Served.** Every node serves the file it was built with at `GET /v1/openapi.json` (free,
   ETag); the website renders its Reference from the same file and offers it for download.
5. **Nested transcripts stay prose.** The level-2 transcript, sealed reveals, track records and
   signed reports are described at their top level, with their inner shapes in
   docs/spec/delivery.md: clients check them with the Rust client functions, never with a schema.

## Alternatives
- Generate the spec from the code (utoipa, aide): a new dependency on every handler, and the
  handlers build JSON with `json!`, so the generator would know no shapes without rewriting them.
- YAML: friendlier to edit, but needs a parser in the website build and in the Python check.
- Keep prose only: what we had; it drifted.

## Consequences
- A new route, error code or response key fails check-all until the file says it.
- The checker validates the answers the tests produce, not every possible answer; untested paths
  of a handler can still differ from the file.
- Clients and agents can fetch the description of the exact build they talk to.
