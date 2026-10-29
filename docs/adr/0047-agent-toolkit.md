# ADR-0047: Agent toolkit: an API client and an MCP server, with answers checked by `onyx-check`

## Status
Accepted, 2026-10-26 (docs/NORTH-STAR.md NS8, pillar 8 "Agents first"). Implemented by
`crates/onyx-check`, `apps/agent` (client, check wrapper, MCP server, tests) and
`scripts/agent-smoke.sh` (run by `scripts/check-all.sh`).

## Context
The first users we plan for are apps and AI agents (a partner's Q9; ADR-0045). The API is described by
one OpenAPI file, but an agent still had to write its own HTTP calls, could not pay, and had no way
to check an answer: the client check (`onyx_api::client`, docs/spec/delivery.md §Verification)
exists only as a Rust library. The website's browser verifier (ADR-0020) does not cover API
answers, and `onyx-api` does not build for wasm32 (its server stack needs `mio`); splitting the
client out of `onyx-api` would move about 6,800 lines across `client`, `catalog`, `observations`
and `roots`.

## Decision
1. **`onyx-check`, a command, does every check.** It takes the answer (`--answer file|-`) and what
   was asked (`--asked`, JSON: `proof`, `export`, `settlement` or `metric` with the metric's
   definition) and calls `ClientNode::verify` unchanged: shard id and binding recomputed, rows
   checked against the root, the answer matched to the question, and `getFinalizedRoot` read on
   the caller's own node (`--rpc`, `--chain-id`, `--contract`). No check is written outside Rust.
2. **The chain read is the default; skipping it is explicit.** Without a node the command refuses
   to run unless `--unchecked` is given, and then reports `chainChecked: false` with the
   `NOT VERIFIED ON CHAIN` warning (as `verify_response_unchecked`). The question is parsed
   strictly: an unknown field is an error, so a typo cannot drop a condition.
3. **Stable results for machines.** One JSON report on stdout; exit 0 verified, 1 refused (with a
   stable `code` per `VerifyError`), 2 usage, 3 the caller's node could not be read (nothing
   decided). Only exit 0 with `ok: true` is a pass.
4. **The client and the MCP server are built from the shipped OpenAPI file**, never from the
   server's copy, so a server cannot rename a tool, change a description an agent reads, or add a
   parameter. Arguments are checked against the file's patterns; path values are encoded; only the
   payment, prepaid-voucher, contributor and cache headers can be set; the base URL is HTTPS (or
   http to this machine); a 308 is followed only on the same server; answers are capped at 64 MiB
   and 30 s.
5. **Payment stays with the agent's wallet, and only a pinned offer is payable.** A 402 is a
   normal tool result with the decoded x402 offer; the toolkit holds no key and signs nothing.
   `onyx-check --offer` checks the offer with the existing pin (`PayToPin`, REV-4): one option
   must pay the `OnyxRevenue` of the deployment manifest the agent trusts, in its token, on a
   testnet, within the agent's limit, and the agent's node must hold that contract's code.
   Without a manifest the result says not to pay. The agent then calls again with
   `PAYMENT-SIGNATURE` or a prepaid `ONYX-PREPAID-VOUCHER` (ADR-0046).
6. **Checked as sent.** Row answers are kept by the MCP server under an `answerId` (the last 32),
   so `onyx_check_answer` checks exactly what the server sent, not a copy the agent retyped; a
   body over 200,000 characters is kept but not shown. A check that skipped the chain read is
   `unverified_on_chain` and an error result.
7. **Writes are opt-in.** The MCP tool that records a data proposal exists only with
   `ONYX_MCP_ALLOW_SUBMIT=1`. Every tool result carries a note that server fields are data, not
   instructions.
8. **No dependencies.** The JavaScript is plain Node 20 (`fetch`, `node:test`); the MCP transport
   is newline-delimited JSON-RPC over stdio, written here (about 100 lines).

## Alternatives
- Split the client out of `onyx-api` into a crate that builds for wasm32, and ship it to Node as
  WebAssembly: one package with no binary, but a large move of tested code for the first
  version; kept as the follow-up if agents need a pure-JavaScript install.
- Re-implement the checks in TypeScript: two implementations of the protocol rules that drift;
  ADR-0020 rules it out for the website, and the same reason holds here.
- The official MCP SDK: a dependency tree for the 4 methods used.

## Consequences
- An agent can find (tool list), pay for (x402 offer) and check (`onyx_check_answer`) Onyx data
  today; the check needs the `onyx-check` binary next to the Node code.
- Level-2 observations, signed facts, track records and salted label rows are fetched by the
  toolkit but not yet checked by `onyx-check` (their Rust checks exist; adding each is a new
  `kind`).
- What an agent asks must come from its own sources: a settlement's round, shard and subject from
  the market contract, a metric's definition from the registry. No tool reads them yet on the
  agent's own node (ROADMAP S8, THREATS AGT-10).
- Review NS8-R1 (fresh adversarial reviewer, 2026-10-26) found 10 issues; THREATS AGT-1 to
  AGT-10 record each fix or the open part.
