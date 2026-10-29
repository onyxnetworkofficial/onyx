# ADR-0039: Durable submission receipts and local review

## Status
Accepted for U2, 2026-10-21. Extends ADR-0038; no verification or publication authority is granted.

## Context
The contributor workflow needs a durable receipt and actual review decisions. The original intake
API is not mounted in the node. Capacity admission and live-index publication happen outside its
journal mutation, permitting over-capacity admission and missing live receipts after cancellation.

## Decision
1. One mutex owns journal and receipt index. Capacity admission, durable append and index update
   run in one blocking operation, surviving HTTP cancellation. Reads that can wait for disk also
   run outside async executor threads. A shared 16-permit worker cap remains held inside each
   worker after cancellation; admission is immediate busy rejection rather than an unbounded queue. A failed write disables further writes; fsync precedes ack.
2. New submissions add `kind=submission`, `version=1` to the original S1 record. Replay accepts
   original unversioned S1 records but revalidates complete content, derived hash, route and initial
   status. Unknown formats or invalid records fail closed. Existing hash and receipt-ID domains stay.
3. Append review events: `kind=review`, `version=1`, submission ID, content hash, revision,
   previous/next status, reason code, Unix seconds. Revision begins at zero. Decisions require the
   expected revision and hash. Time cannot precede receipt/previous review. At most two decisions
   per submission; rejected is terminal. No deletion or editing of existing records.
4. `pending_review -> approved` needs an accepted route and reason `reviewed`. Any initial state
   may become `rejected` for `rights_unresolved`, `unsupported_source`, `invalid_content` or
   `duplicate`. `approved -> rejected` requires `withdrawn`. Held sources cannot be approved here.
   Corrected information needs a new submission. Approval permits configuring supported checks;
   it is neither verification nor publication. Unsupported adapters still fail at the job gate.
5. Reviewer authority is local filesystem access. `submit-review list/show/decide` opens the same
   exclusively locked journal: stop the node, review, restart. No second writer or HTTP admin API.
   List is bounded/paged; show is local only. Fixed reason codes; no free-text notes.
6. Public responses remain compatible: POST `{id,status,hash,route}`; GET exactly `{id,status}`
   (plus `check` once fed: amendment 1), no-store. IDs are locators, not secrets. No public content, notes, history, duplicates or listing.
   HTTP persistence failures do not disclose filesystem paths.
7. Node intake is opt-in through `ONYX_SUBMISSIONS` and uses existing attribution, concurrency,
   deadlines and budgets. Caddy admits only the two exact POST paths (64 KiB) and receipt GET/HEAD,
   and restores signed-route parity. `VITE_ONYX_SUBMIT_API=1` enables same-origin web intake;
   offline builds retain JSON download. Receipts are downloadable and their status can be reopened.

## Consequences
Local review requires maintenance downtime; document it. Online authenticated review and storage
retention/migration remain later decisions. Rejecting a receipt does not free admission capacity:
its history remains durable, at most three records per receipt. Lost-response retries can create
another receipt, as in S1; the UI explains uncertain delivery and never automatically resubmits.
In-memory rate limits reset at restart. The journal is for a single host on local disk.

## Amendment 1 (2026-10-25, PRODUCT-PLAN U4, docs/pre-public-loop.md PP1): the fed receipt
A contributor could see "approved" but never what the observers then found. The U4 worker's feed
ledger moves into `onyx_api::submit::feed` and always lives at `JOURNAL.feed.json` (the worker's
`--ledger` flag is gone, so the node and the worker cannot read different files). The worker
writes it owner-only, atomically, while the node is stopped, like the journal. The node reads it
once at startup; a damaged or oversized (> 8 MiB) ledger refuses startup.

`GET /v1/submit/{id}` adds `check: {level, recordDate, roundId, result, registry}` only when the
receipt's current status is `approved` and the fed content hash is its hash. A withdrawn approval,
a failed or missing feed show `{id,status}` as before. Failure reasons and attempts never leave
the ledger. Every served field is shape-checked when the ledger is read (hashes `0x` + 64 hex, a
date, `simulated` or `eip155:<id>`). The record date is the only proposal field shown; the round
it names is public in the results store anyway. `registry: simulated` marks a local rehearsal and
the page says the round was not posted on a chain. The page states that the evidence shows the
source published the value, not that the value is true. Tests: `submit_tests::a_fed_receipt_shows_the_observers_evidence_and_nothing_private`,
`u4_intake_feed::the_feed_ledger_feeds_once_and_bounds_failures`, `apps/web/tests/pp1.mjs` and the
connected rehearsal (`ONYX_SUBMIT_BROWSER=1 scripts/level3-book.sh local`). THREATS U4-5, U4-6.

## Sources
[Tokio blocking tasks](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html) survive
cancellation once started. [Rust file locks](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock)
support the existing exclusive writer. Reviewed 2026-10-21; neither implies distributed durability.

## Amendment 2 (2026-10-25): the observers' value on the receipt (pre-public loop PP11)
A fed receipt's public `check` also carries `value`, the value the observers agreed on as the
feed's decimal text (`40305316210829.72`), read from the round itself (the recorded run's
settlement, or a live store's transcript), never from the submission. The page shows it grouped
for reading and says, as before, that it shows what the source published, not that the source is
correct. The feed ledger keeps it; a ledger entry without it is refused.

