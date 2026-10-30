# Spec: the contributor loop, CL2-CL8 together (overview)

Status: **Proposed 2026-10-29**, design only. Tracker: docs/contributor-loop.md. Decisions:
ADR-0053 (CL3), ADR-0054 (CL4), ADR-0055 (CL5, a short note; the CL5 implementation ADR
supersedes it), ADR-0056 (CL6), ADR-0057 (CL7), ADR-0058 (CL8). CL2 is designed with its build.

## Scope

One place, one API, one format for data users, with no provider keys to juggle; contributors keep
their own keys and send only data. The loop's items fit together as one path for a value:

```text
contributor (CL2 client, own keys)
  ├─ public JSON API ─► manifest + rights review (CL3) ─► level-2 round ─┐
  ├─ keyed API ──────► keyed pool round (CL4) ───────────► level-2 round ┤
  └─ bulk own data ──► manifest, stored on first paid use (CL5) ─ level 4 ┤
                                                                          ▼
buyer / agent ─► one API (/v1), one balance (CL6 credits) ─► value + proof level + ladder
                                                                          │
                     correction market on the value (CL7) ◄───────────────┤
                     aligned multi-venue feed over values (CL8) ◄─────────┘
```

What stays true in every item: a contributor's key never leaves its machine; data naming a person
is refused; a source whose terms forbid passing data on stays refused; entities only; testnet
only; no token, emission or price logic (hooks only: the `asset` field, placeholder parameters).

## Interfaces

| Item | Builds on | New code (planned) | New on chain | New routes |
|---|---|---|---|---|
| CL2 contributor client | ADR-0038, ADR-0047 (MCP), ADR-0053's `onyx-fetch` | `onyx-contrib`: key vault, generic HTTP/JSON connector, MCP tools | none | none |
| CL3 generic fetcher | ADR-0027, 0030, 0038 | `onyx_core::feed_json`, `crates/onyx-json`, `crates/onyx-fetch`, `onyx_level2::manifest`, `sources/` | `registerMetric` per manifest (exists) | `/v1/sources`, `/v1/sources/{metric}` |
| CL4 keyed agreement | CL2, CL3, ADR-0027 draws | `Access::Keyed`, keyed seats and pools, private fetch slots, `draw_v2` operator rule | none in v1 | none |
| CL5 storage at scale | ADR-0040, 0049, 0052 | per the implementation ADR | a deposit contract | upload and wants routes |
| CL6 credits | ADR-0046, 0025, 0026, 0049 | credits journal, relayer | `OnyxPrepaid` v2, `OnyxRevenue` v2 | `/v1/credits/...`, `/v1/relay` |
| CL7 correction markets | ADR-0042, 0048, CL3 | `recheck_of`, `SubjectRule::Recheck`, recheck runner | `OnyxCorrections` | value answers gain `recheck` |
| CL8 time alignment | ADR-0027, 0017, 0049, CL3 | `SubjectRule::Bucketed`, `aligned()` | none | `/v1/aligned/{metric}` |

Every new route goes into `docs/api/openapi.json` with its schema and errors (ADR-0045); every
threat named in an ADR goes into THREATS.md with its test when the item is built.

## Order of work and dependencies

1. **CL1** (tester kit) first: nothing below depends on it, but testers need it now.
2. **CL3's `onyx-fetch`** before CL2 ships: the client and the observers must use the same
   SSRF-safe transport (about half a day: a move of `HttpsFetch` plus caps and tests).
3. **CL2** (client v0), then **CL3** (strict JSON reader, descriptor, manifests, the review
   tool, the browser check): CL3 is what makes a contributed endpoint reach level 2.
4. **CL4** after CL2 and CL3: it reuses the vault, the descriptor and the whole round.
5. **CL6** next: the relayer removes the ETH-for-gas step for CL4 observers and pays CL5 and CL7
   refunds into one balance. Its contracts are redeployments, so batch them in one release.
6. **CL5** in parallel (already being implemented): independent of CL3 and CL4; it uses ADR-0049
   holds, which exist.
7. **CL7** after CL3 (recheck templates are manifests) and ideally after CL6 (payouts into
   credits).
8. **CL8** last: it needs CL3, a new subject rule and at least three venues whose terms are
   cleared, which is review work before any code is useful. 2026-10-30: the subject rule, the
   alignment rule, composites and the client check are built and tested on fixtures and loopback
   venues (ADR-0058 amendment 1); no venue is cleared (docs/ops/market-venues.md), so the route
   and every live venue wait (ROADMAP S14).

Shared pieces, built once: the strict JSON reader (CL3, used by CL2, CL4, CL7, CL8), the hold
model (ADR-0049, used by CL5, CL6, CL7), appended `SubjectRule` variants (CL7 `Recheck`, CL8
`Bucketed`; appended last so no live template id changes).

## Assumptions

- Base Sepolia only; tONYX and testnet USDC have no value, so every economic defence (bonds,
  deposits, fees) is a demonstration until the partners decide the economics (NS12).
- The coordinator stays trusted for what ADR-0027 and ADR-0042 already state (draw grinding within
  the subject bound, recorded results as attestations); nothing here removes that.
- One hosted node for now; node tiers (CL5) and replicas matter once operators join (NS4).
- Each source's terms are reviewed by a person before a template is registered; no automated step
  replaces that review.

## Open questions

1. Who besides the owner may register templates and pair rechecks on chain (a reviewer key)?
2. Observer pay (THREATS L2API-3) and any emission: NS12; CL6 only carries the result.
3. Keyed level 2: the same badge as public level 2 with evidence text, or a level of its own?
4. Should CL6's two redeployments and CL7's new contract ship in one Base Sepolia release with the
   current markets kept for their open positions?
