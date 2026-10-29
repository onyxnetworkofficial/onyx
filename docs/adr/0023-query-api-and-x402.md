# ADR-0023: Query API and x402 payments

## Status
Accepted, 2026-10-10 (M7 loop 1, project loop 38). Implemented in `crates/onyx-api`; tests in
`crates/onyx-api/src/tests.rs` (router, mock data), `adapters/source-btc/tests/api.rs` (a round
of blocks 921,080-921,082 settled by the M4 code) and `contracts/test/OnyxApiVector.t.sol`
(the settlement payload verified by `OnyxMerkle`). Demo server:
`adapters/source-btc/examples/serve-api.rs`, run book `docs/ops/run-api.md`.
Supersedes the library choice in ADR-0006; keeps its trait-boundary idea.
Amended 2026-10-10 (M7 loop 39): review findings M7-F1..F10 (THREATS.md); see
"Amendment (loop 39)" below. Where it differs from the decision list, the amendment wins.
Amended 2026-10-13 (M7 loop 3): the client's chain check is on by default and an unknown
settle is checked against the token ("Amendment 2" at the end).

## Context
M7 delivers data: rows with Merkle proofs, paid while fresh, free later (docs/spec/delivery.md,
docs/spec/pricing.md). Research: RESEARCH.md "M7: delivery and x402". Facts that shape this:
- x402 v2 (x402-foundation/x402, `specs/transports-v2/http.md`) moved the payment headers to
  `PAYMENT-REQUIRED`, `PAYMENT-SIGNATURE` and `PAYMENT-RESPONSE` (v1 used `X-PAYMENT` and
  `X-PAYMENT-RESPONSE`). Each carries base64 JSON.
- The facilitator API is two calls, `POST /verify` and `POST /settle`, each with body
  `{x402Version, paymentPayload, paymentRequirements}`.
- `x402-axum` (ADR-0006) pulls in a full HTTP client, TLS and EVM signing stack. The dev box has
  about 8 GB of disk free, and the crate is unaudited either way.
- The canonical root is on chain (`OnyxSettlement.getFinalizedRoot`, ADR-0022). Rows live in the
  CAS keyed by their Merkle root (ADR-0018).

## Decision
1. **Endpoints** (axum 0.8, JSON, all under `/v1`):
   `health`, `proofs/{round}/{shard}/{index}`, `metrics/{metric}?subject=&as_of=`,
   `export/{round}/{shard}`, `settlement/{market}`. Row responses carry
   `{row, proof{index, leafCount, siblings}, root, finalizedAt, recipeId, labelSet, tx, license,
   attribution}`. Rows and hashes are `0x` hex.
2. **Roots come from a `RootSource`, never from a miner or the CAS.** `LedgerRoots` reads a
   settled `Ledger` (indexer or demo); `ChainRoots` does `eth_call getFinalizedRoot(uint64,bytes32)`
   through a `JsonRpc` trait. A zero root means not finalized (404 `not_finalized`).
3. **Rows come from the CAS by root.** The API rebuilds the tree and refuses to serve
   (500 `rows_mismatch`) if it does not hash to the finalized root.
4. **Rights at serve time.** `Restricted` and `Forbidden` sources answer 451
   (`restricted_data`, `forbidden_data`). Free and attributed data carries license and attribution.
5. **x402 v2, own wire types.** `onyx_api::x402` implements the v2 types and headers directly
   (serde, base64), with a `Facilitator` trait: `MockFacilitator` (local, tracks nonces, refuses
   replay, underpayment and unfunded payers) and `HttpFacilitator<T: HttpPost>` (the spec's
   `/verify` and `/settle` bodies over a transport trait). The flow is:
   price 0 → serve; no header → 402 + `PAYMENT-REQUIRED`; malformed → 400; `accepted` must equal
   our requirements exactly (scheme, network, amount, asset, payTo) before any facilitator call;
   verify → settle → only then serve, with `PAYMENT-RESPONSE`. A facilitator outage is 500 and
   serves nothing.
6. **Pricing** is the step decay of docs/spec/pricing.md: `base` micro-USDC while
   `now - published_at < fresh_window`, then free. Free rows are `Cache-Control: public,
   immutable`; paid rows `private`. ETag is `"{root}:{index}"` (or `:all`); `If-None-Match`
   answers 304.
7. **Stable error codes.** Every error is `{"error":{"code","message"}}` with a code from a fixed
   list (spec: delivery.md "Error codes").
8. **Boundaries.** `onyx-api` is a core crate and depends on no adapter. The demo server lives in
   `adapters/source-btc/examples/` because it settles the BTC fixture.

## Consequences
- Departures from ADR-0006: no `x402-axum`; v2 headers instead of `X-PAYMENT`; no concrete
  HTTP/TLS transport for `HttpFacilitator` or `ChainRoots` yet (M7 loop 2). Our x402 code is
  small and tested against the spec examples, but it is unaudited (THREATS.md M7).
- Base Sepolia (`eip155:84532`) and the USDC address are configuration taken from the spec
  example, not constants in code.
- A client verifies proofs itself; the same settlement payload verifies in Rust and in Solidity
  (`fixtures/api/settlement-vector.json`).
- A 304 is answered before payment. It carries no body, so it gives nothing the client did not
  already hold.

## Amendment (loop 39)
Review findings M7-F1..F10 (THREATS.md, "Review findings, M7 loop 38") change these points:
1. **Verification bundle.** Every row, export and settlement response carries `verify`: ledger
   kind, chain id, contract, `getFinalizedRoot(uint64,bytes32)`, round, shard, root, leaf
   count, the binding and every input that recomputes it and the shard id (ADR-0022), the
   finalization height and time, and `tx`. `onyx_api::client::verify_response` is the reference
   client check; delivery.md "Verification" is the procedure. The server refuses (500
   `binding_mismatch`) a root whose on-chain binding differs from its catalog inputs.
2. **Cache policy.** `immutable` only for URLs that pin `?root=` to the finalized root; a
   different pin is 409. Other successes are `no-cache` + ETag. Errors, 402 and health are
   `no-store`. This replaces point 6's "free rows immutable".
3. **Order of work.** Price and the 402 come before any row is read; rows are read after the
   facilitator verifies, from a bounded cache keyed by root; settle comes after the body is
   built. Roots are cached 30 s. Exports are capped (`export_max_rows`, 413).
4. **Payments.** A payment is bound to one URL (`resource.url`). The server checks the EIP-3009
   authorization locally (amount, recipient, window, EIP-712 signature recovered with
   secp256k1, low-s) before calling the facilitator; `MockFacilitator` makes the same check.
   Facilitator calls have a deadline (503 `facilitator_unavailable`, replacing point 5's 500).
   Payment attempts are rate limited per client address (429). An identical settled request is
   answered from a bounded 10-minute store, not charged twice.
5. **Server limits.** One global in-flight limit (503 `overloaded`) and one request deadline
   (504), as a middleware: `Router::layer` with tower's `ConcurrencyLimitLayer` would give each
   route its own semaphore.
6. **Roots on chain.** `ChainRoots::connect` checks `eth_chainId` and the contract's code (and
   its keccak when configured), failing closed; reads use block tag `finalized`; the
   finalization time is the block's timestamp; `tx` comes from the `Finalized` log.
7. **Canonical URLs and errors.** One spelling per resource (308 to it), JSON for every
   rejection, 404 and 405.
8. **Pricing.** Age runs from the root's finalization time (pricing.md), not a catalog time. An
   export is priced per row. A catalog read failure is 500, never free.
9. **Metrics** fall back to older shards when the newest lacks the subject.
10. **Test label sets** (`test.*`) are refused (403 `test_labels`) unless the server runs with
    `allow_test_labels`; the demo passes `--dev`.

Residual: a settle that times out after it landed on chain leaves the payer charged without data
until the server checks `authorizationState` on chain (THREATS.md M7-F5, M8).

## Amendment (loop 40)
Fixes for the loop 39 review (THREATS.md M7-L39-F1..F7).

1. **Settlement outlives the request.** Settle and storing the answer run in their own task.
   The handler waits for it within the request deadline. A retry of the same payment waits for
   a settle in flight, then gets the stored answer. Config requires `request_timeout >= 2.5 x
   facilitator_timeout`; a settle may take `4 x facilitator_timeout`. Defaults: 12 s and 4 s.
2. **Clients check the question, not only the proof.** `verify_response` takes what was asked
   (`Expect`) and checks index, row key, value, metric, `asOf` and market.
3. **Metrics** price from the newest finalized candidate shard, before any row is read, and
   never answer 304 before payment. At most 16 candidate shards. Each cached root has a key
   index. An unknown subject after payment is 404, not settled.
4. **Limits.** IPv6 clients are keyed by /64. The rate limiter is an LRU (65,536 clients). At
   most `max_facilitator_calls` (32) facilitator calls at once; others get 503
   `facilitator_busy`. Only 65-byte EOA signatures are accepted (smart wallets later, M8).
5. **Replays** are bound to the client and capped at 3 per payment.
6. **One URL per resource**, query included: unknown parameters are 400, other spellings 308,
   and the payment `resource` is the canonical URL.
7. **Chain id** is rechecked before every root read and before a block time is trusted.

## Amendment (loop 41)
Fixes for the loop 40 review (THREATS.md M7-L40-F1..F5).

1. **The client check is complete by construction.** Every response field is either checked
   (`client::BOUND_FIELDS`) or listed as informational (`client::INFORMATIONAL_FIELDS`); a test
   fails on a field in neither list. The bundle carries `sourceRef` and `rights` (Borsh bytes
   that hash to `sourceHash` and `rightsHash`). `Expect::Metric` carries the metric's
   definition (source id, recipe, schema, params hash, optional label set) and checks `asOf`
   against the end of the pinned snapshot. `Expect::Settlement` carries the market's round and
   shard. Echoed fields (`leafCount`, `finalizedAt`, `recipeId`, `labelSet`, `license`,
   `attribution`) must equal the bound values. `ShardMeta::from_spec` takes `as_of` from the
   snapshot, not from the caller.
2. **Wider rate scopes.** An IPv6 /64 also draws from its /56 (x4) and /48 (x16); all or none.
3. **Per-payer budget for unsettled payments.** A verified payment that does not settle (404
   after verify, refused settle, abandoned request) is charged to its `from` address. Default
   20 per hour (`unsettled_per_hour`); empty budget is 429 `payer_limited`, checked before the
   facilitator. We count instead of settling the 404, so a payer is never charged for no data.
4. **Atomic claim.** One request owns a payment key; identical concurrent requests wait and
   then get the stored answer. Verify and settle run once.
5. **Drop guard.** The claim releases itself on every exit, including a panic in the settle
   task. The settled store size is configurable (`settled_entries`, default 65,536). Replays
   stay bound to the client address; a client whose address changes before its retry gets a
   402 (documented in delivery.md).

## Amendment (loop 42)
Fixes for the loop 41 review (THREATS.md M7-L41-F2..F5). F1 (metric recency) waits on PLAN q14.

1. **The metric definition pins every input.** `MetricDef` adds chain, window rule, runtime
   hash and registry root; the label set is required (`ZERO` = no labels). A finalized shard
   with another label set (a `test.*` one included), chain, window, runtime or registry is
   refused (`NotRequested`).
2. **Settled store bounded by bytes.** Each stored response weighs its body bytes; cap
   `settled_bytes` (default 256 MiB) on top of `settled_entries`.
3. **Per-endpoint keys.** Each `Expect` has its own key allowlist (`client::allowed_keys`);
   extra keys are refused, not ignored. `ChainAnswer` carries `(round, shard)` and must match.
4. **Reserve, then refund.** The per-payer unit is taken atomically when a payment is
   accepted and refunded on settle or on a facilitator failure. Concurrent requests cannot
   overspend. Fresh `from` keys still bypass the per-payer budget; the per-client buckets are
   the real cap (documented in delivery.md).

## Amendment (loop 43)
Fixes the loop-42 review (M7-L42-F1..F3); 0 merge-blocking before.
1. **Refund on facilitator errors at settle.** A settle that returns a facilitator error gives
   the payer's unit back; a refused settle (the payer's: no funds, spent nonce) and a settle
   timeout (outcome unknown, M7-F5) keep it spent.
2. **Refund on refused verify.** A payment the facilitator refuses at verify is not charged, so
   a replayed settled header from other addresses cannot drain the real payer's budget.
3. **Full budgets are not stored.** A refund that brings a payer back to its cap removes the
   entry; fresh keys refused at verify never hold a slot, so they cannot evict drained payers.

## Amendment (loop 47)
Decisions PLAN q14 (a) and q-M7-1 (a), 2026-10-10.
1. **Metric freshness floor.** `Expect::Metric` requires `min_as_of`; the client refuses a shard
   whose snapshot ends before it, and (as before) after the asked `as_of`. Recency within the
   floor and absence of a subject are not provable from one response and are documented as
   such (delivery.md §Verification). Anything that settles money pins `(round, shard)` through
   the settlement endpoint. Closes M7-L41-F1. An on-chain "latest shard per metric" index
   (q14 b) stays possible later if a customer needs provable recency.
2. **Facilitator.** `x402::facilitator_url` defaults to the public x402.org facilitator
   (`https://x402.org/facilitator`) for Base Sepolia only; `ONYX_X402_FACILITATOR` overrides it;
   other networks have no default. No account is needed. Tests use the mock facilitator and
   never call it.

## Amendment (loop 48)
Fixes the loop-47 merge-gate review (THREATS.md M7-L47-F1..F5).
1. **Price and serve the same shard.** A metric lookup searches only the quoted shard and older
   ones (`&candidates[quote..]`); a newer shard that finalizes after the quote is never served
   at the quoted, possibly free, price.
2. **Exports.** A byte cap (`export_max_bytes`, 32 MiB) joins the row cap; free exports have a
   per-client budget (`free_exports_per_minute`, 6). A response holds its in-flight permit until
   its body is written (64 KiB frames) or `send_timeout` (60 s) passes; the next frame after the
   timeout fails and closes the connection. There is no socket write timeout in the server, so
   public deployments need a buffering proxy with a send timeout (run-api.md).
3. **Settle errors are unknown outcomes.** A facilitator error at settle is treated like a
   timeout: the unit stays spent, nothing is stored as a success, the answer is 503 and the
   retry re-verifies (M7-F5). This supersedes loop 43 item 1. `HttpFacilitator` accepts a
   non-2xx answer only as a refusal.
4. **Facilitator configuration.** The server reads `ONYX_X402_FACILITATOR` at startup
   (`x402::facilitator_url_from_env`; a set, non-empty value wins, else x402.org for Base
   Sepolia). Payment networks outside `x402::TESTNET_NETWORKS` (Base Sepolia only) are refused
   by `ApiState::new` and `facilitator_url` unless `allow_mainnet` (`--allow-mainnet`) is set;
   it is off by default and this project is testnet-only. The HTTPS transport is
   `x402::HyperPost` (hyper, rustls with webpki roots; feature `http-client`), used by the demo
   with `--live-facilitator` and never in tests. Reqwest was the first choice; it cannot be
   resolved offline against the workspace's pinned wasm-bindgen, and hyper is already a
   dependency.
5. **Freshness floor on the server.** `/v1/metrics` takes `min_as_of`; with no finalized shard
   at or above it the answer is 404 before any 402. It is part of the canonical URL (after
   `as_of`).

## Amendment (loop 50)
Fixes the second merge-gate review (THREATS.md M7-L48-F1..F4).
1. **The price is checked, not inferred.** Loop 48 item 1 bounded the metric lookup by `as_of`
   position, but price depends on finalization time: an older-`as_of` shard finalized later is
   dearer than the quoted one. `find_metric_row` now takes the quote and skips any shard whose
   charge at serve time is not exactly the quote, before reading its rows. A served row costs
   what was paid (0 when free). A row that exists only in a shard at another price is a 404
   (no charge when free; when paid, as any 404 after verify); the client reaches it by asking
   with that shard's `as_of`. Re-quoting to that shard was rejected: it would need the subject
   before the 402 (M7-L39-F3). A seeded random property test holds the invariant.
2. **Trusted proxies.** `ApiConfig::trusted_proxies` (CIDRs, default empty). Only when the peer
   is in one, the client key comes from the right-most `X-Forwarded-For` address outside the
   trusted ranges (an entry may carry a port, which is dropped: M7-L50-F4); a malformed entry
   first, or none, leaves the peer. Otherwise the header is ignored.
3. **Export order.** The 413 for size precedes the 402 only when the rows are in memory (and for
   free exports); otherwise it follows verify and refunds the unit (docs corrected).
4. **Facilitator over https only.** `facilitator_url` refuses a non-https URL at startup; plain
   http only to localhost, 127.0.0.1 or [::1] (dev). The URL is parsed as a URI (no user info,
   exact host); the transport re-checks each request URL and speaks plain http only to those
   hosts (M7-L50-F3). A base URL with a query or fragment is refused (M7-L51-F3).
5. **HEAD never pays.** A `HEAD` with `PAYMENT-SIGNATURE` gets 400 `invalid_payment` before any
   verify or settle; `HEAD` without one still shows the 402 terms (M7-L50-F2).

## Amendment (OPS-L2, settle intents)
1. **Every paid settle is recoverable.** Before a settle call the server writes an intent
   (payer, nonce, round, shard, amount, `validBefore`) to a journal; no durable intent, no
   settle (503, unit refunded). Success ends the intent once the receipt is on disk (or the
   shard was credited in a self-settle). Any other ending, a crash or a kill leaves it; the
   intent resolver makes it a receipt only when the token's logs show the exact amount reached
   OnyxRevenue (`AuthorizationUsed` and `Transfer` in one successful transaction), and drops
   it 10 minutes after `validBefore` otherwise. This closes the residual above (M7-F5): the
   payer may still get no data for a settle that ended unknown, but the fee is attributed.
2. **The facilitator client waits the whole settle patience** (4 x the verify timeout), and the
   facilitator slot taken at verify is held through the settle (OPS-L2-F1, F2).
3. **Arrival proofs fit public nodes**: `authorizationState` first, then only the blocks of the
   authorization's time window (found by timestamp, never pruned history), 200 blocks per
   `eth_getLogs` (OPS-L2-N1).
4. **Review 3 (OPS-L3).** An intent ends only when its receipt is durable (`ReceiptLog::record`
   reports durability; nothing is removed while the receipt log is failing). The arrival proof is
   the token's own pairing, `AuthorizationUsed` then its adjacent `Transfer`, so one transfer
   proves one authorization. Expiry is judged by chain time at a pinned block, never the local
   clock. `validBefore` is capped at `now + maxTimeoutSeconds` + 300 s before verify (x402 clients
   sign `now + maxTimeoutSeconds`; this cap is Onyx's own rule), one payer may hold 4 open intents,
   and resolver passes are bounded and resumable.
5. **Review 4 (OPS-L4): every intent ends.** The invariant behind the intent caps: an intent no
   settle owns becomes a receipt or is dropped within `validBefore` + grace + a bounded resolver
   delay, in chain time, whatever the payer does. A cancel (`AuthorizationCanceled`, searched up
   to the pinned block) ends it; a use that is not FiatToken's adjacent pair is not ours; a used
   authorization with no payment of ours in its whole span (bounded: `validAfter`/`validBefore`,
   the accepted life, and a 300 s clock-skew guard that refuses new paid settles beyond it) is
   dropped after two complete reads and one extended read. `validBefore` must also outlive the
   settle budget; one intent per `(payer, nonce)` (409); only unsettled intents count toward the
   per-payer cap. See THREATS OPS-L4-F1..F6.

## Amendment 2 (M7 loop 3, 2026-10-13): client chain check by default, settle check
Context: review IDX-R5-3 showed that the server's own root read (`ChainRoots`) goes through the
operator's node, so only the client's own read protects against a lying operator node, and that
read was opt-in (`verify_response(…, None)` passed with `chain_checked: false`). Separately, a
facilitator settle that timed out or failed in transport stayed "unknown" (M7-F5 residual): the
payer was charged when the transfer had landed but got 503, and a new authorization signed for
the retry could charge twice if the first one landed later.
1. **The client reads the root on its own node by default.** `client::ClientNode::verify(body,
   &Expect)` reads `getFinalizedRoot(round, shard)` at the `finalized` block of the chain and
   contract the client configured (after checking `eth_chainId`) and runs every check:
   `chain_checked: true`. `verify_response(body, &Expect, Option<&ChainAnswer>)` keeps its
   signature but now requires the answer: `None` (the client's node says not final, or no answer
   was given) is refused with `VerifyError::NotFinalOnChain`; the response's own errors are
   reported first. The opt-out is explicit, `verify_response_unchecked(body, &Expect)`, and its
   result still reads `chain_checked=false (NOT VERIFIED ON CHAIN …`.
   Backward compatibility: code that passed `None` to skip the chain read now fails closed and
   must call `verify_response_unchecked`; code that passed an answer is unchanged.
   Alternatives: a new default-checking function next to the old one (rejected: the old call
   would keep passing unverified answers silently); a required enum argument (rejected: every
   caller already has `Option<&ChainAnswer>` from `ChainAnswer::from_return`, whose `None`
   means "not final", so refusing `None` is the same rule with no new type).
2. **An unknown facilitator settle is checked against the token** (`settle_check`,
   `ApiState::with_settle_check`; `onyx-node` wires it in facilitator mode). After a timeout,
   transport error or error page at settle, the server reads `authorizationState(from, nonce)`
   at the latest block. Used: the token's logs decide (`arrival::use_of`, the same
   `AuthorizationUsed`-then-`Transfer` pairing as the intent resolver): a transfer of the exact
   value into `OnyxRevenue` is a settled payment (served, the receipt written, the intent ended,
   `PAYMENT-RESPONSE` names that transaction); a use paying someone else or a cancel is the
   payer's refusal (402 `payment_failed`); a use not found yet stays unknown. Unused: the same
   signed authorization is sent once more (an EIP-3009 nonce moves money at most once, so a
   re-send cannot make that authorization pay twice even if the first settle is still in a
   mempool), and a re-send that does not succeed is followed, after about one block
   (`ApiConfig::settle_recheck_wait`, default 2 s), by one more check. A re-send the token
   refused as used means the first settle landed: that check reads the token's logs even when
   the node still reads the authorization unused (a lagging backend; M7L3-R1-1). Each check
   reads again while a use is not found or the node fails, within its budget. Anything
   undecided keeps the M7-F5 semantics (503, unit spent, intent left to the resolver). The
   identical retry (same header bytes, resource and client) of a request whose intent is still
   open runs the check again, with no new verify or settle, instead of getting 409
   `payment_in_use`, and is served once the transfer is proven (M7L3-R1-1); it takes no new
   payer unit, and one authorization is re-sent at most three times in all, its retries
   included (`intents::MAX_RESENDS`, M7L3-R2-5). Once the resolver has recovered the intent
   (transfer proven, receipt written), the intent is replaced, in one journal line, by a proof
   (the paying transaction, under the request's key) and the identical retry is served from it
   for 10 minutes (`PROVEN_WINDOW_SECS`, the settled-response window), with no verify, settle
   or check (M7L3-R2-1). The request key is in the intent journal, so both survive a restart.
   A self-settling server (ADR-0026) runs no such check: there the identical retry of an open
   intent gets 503 `payment_pending` (`Retry-After: 30`, about one resolver pass), never a new
   settle, and is served from the resolver's proof once the transfer is proven; before the L6
   soak it got 409 `payment_in_use` "for another request ... no charge" though its payer may
   have been charged (SOAK-1).
   The proof serves once: it is deleted from the journal (one durable line) before the answer
   leaves, so a restart cannot serve it again, and a deletion that fails serves nothing
   (M7L3-R3-2). An intent a retry adopted while the resolver judged it gets no proof (that
   retry serves from its own check, M7L3-R3-3). A request served though its receipt could not
   be written keeps its intent for the resolver, without its request key, so after a restart
   its retry is not served again (M7L3-R3-4). The proof and retry paths compare the payment
   with the requirements offered now, as a fresh payment is: a price changed since the first
   request gets 402 with the new terms, never the data at the old price (M7L3-R3-5).
   A settle task hands its intent back to the resolver before it releases the request's claim,
   so a retry it wakes finds the intent (M7L3-R2-2). Each check gets half the settle patience;
   a settle task runs at most two settles, two checks and the wait (`settle_task_budget`, 50 s
   with the defaults; in the stop budget: 62 s with the 12 s request timeout, plus 4 s slack,
   under the 75 s supervisor stop). A re-send needs `validBefore` to outlive one more settle.
   Self-settle keeps its own handling (ADR-0026).
   What "never charged twice" means, exactly: one signed authorization moves the payer's money
   at most once, and nothing is served without a facilitator success or a transfer of its exact
   value into `OnyxRevenue` in the token's logs. It does not mean every payment is served at
   once: with a lagging node the first request can end 503 although the transfer landed. The
   identical retry gets the data once the transfer is proven: by its own check while the intent
   is open, then from the resolver's proof for 10 minutes. It is not served when it comes later
   than that, after the proof was used once and its 3 cached replays are spent or lost (the
   settled-response cache is memory only), with another header, or from another client: a real
   facilitator then answers 402 on the used nonce, and the payment is still attributed (the
   receipt is written). After a price change it gets 402 with the new terms (M7L3-R3-5). A
   payer who signs a new authorization instead of retrying pays again
   (that is a second payment, not a re-use).
   Alternatives: never re-send (rejected: the payer's only other way forward is a new
   authorization, which is the double-charge risk); re-send without reading the token first
   (rejected: a used authorization is decided by its logs, and a re-send would only add a
   failed settle).
