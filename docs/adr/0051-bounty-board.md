# ADR-0051: The bounty board: closed-list wants from prepaid accounts, shown in buckets weekly

## Status
Accepted, 2026-10-28 (docs/NORTH-STAR.md NS15; docs/north-star-loop.md NL9; ROADMAP S10).
Implemented by `onyx_intake::bounty`, `onyx_api::wants` (`GET` and `POST /v1/wants`), the node
mount in `crates/onyx-node/src/app.rs`, both Caddyfiles, and the Add data page's "Wanted data"
panel (`apps/web/src/lib/components/WantedData.svelte`). Design draft: docs/spec/bounty-board.md
(lane B).

## Context
Contributors want to know what data buyers want. Buyers must not reveal what exactly they look
for: an exact query can be trading intent or a person's interest. The board must say "demand
exists here" without saying who wants what, and must not become a channel for free text.

## Decision
1. **Closed lists only.** A want is `kind` (the submit flow's six kinds), `domain` (eight),
   `region` (a continent or global) and `cadence` (once, daily, monthly). `parse_want` refuses any
   other field and the route refuses anything beside `want`, `buyer` and `signature`
   (`bounty_too_specific`). There is no field for a subject, metric, URL, date, entity or person.
2. **Identity is an open prepaid deposit.** The buyer signs (EIP-191) the want text for the
   current week; the node reads the buyer's `OnyxPrepaid` account on chain (finalized) and takes
   the want only from an open deposit, never charging it. A deposit makes each extra account cost
   money (spec assumption 1). The signature names the week, so an old signature cannot renew a
   want later. The spec's "voucher header" is replaced by this signed text: a voucher signs a
   running total, which anyone who saw it could replay as the buyer's identity. Without a prepaid
   contract the board is shown and posting answers 503 `wants_closed`.
3. **k-anonymity and a weekly cut.** A category shows only with live wants from at least 5
   distinct accounts (spec open question 1: the larger candidate), as a bucket (`5-19`, `20+`),
   and only wants made before the current week's cut count, so one want's time cannot be read
   off the board. A want lives 4 weeks unless renewed (open question 2). An account holds at most
   16 live wants.
4. **Salted accounts.** The node keeps `keccak256(salt ‖ buyer)` with a random salt made once in
   its state directory (`wants.salt`), journals each accepted want (synced before the answer) and
   never returns the hash or the address.
5. **On request on the site.** The panel asks `/v1/wants` only when the reader presses its button
   and maps every value through the same closed lists, so nothing outside them is ever printed.

## Consequences
- The board is coarse by construction: a niche want stays invisible until four other accounts
  share its category. That is the price of not exposing one buyer.
- One funded actor with five deposits can make a category show (THREATS NL9-1); buckets and the
  weekly cut keep the signal coarse, and money pledges (spec "Optional money") wait for NL8's hold.
