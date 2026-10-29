# ADR-0033: Chain indexer fills the catalog from `Finalized` events

## Status
Accepted, 2026-10-12. Implemented by `crates/onyx-api/src/indexer.rs` (tests in
`indexer_tests.rs`), wired into `onyx-node` (`ONYX_INDEXER=on`) and the local rehearsal
(`scripts/demo-local.sh`). Spec: docs/spec/delivery.md "Indexer" (I-1..I-5).

## Context
Until now the API served the shards a deploy driver wrote into its catalog file, with a round
each. The settlement contracts ran on Base Sepolia on 2026-10-12, so the chain itself says which
roots are final. PLAN.md asked for an indexer that fills the catalog from `OnyxSettlement`
events, and M7 loop 3 for the evidence half of `LabelSet::check_servable` where label sets are
loaded. Public nodes cap `eth_getLogs` spans (sepolia.base.org: 200 blocks, HTTP 413) and are
load-balanced backends that lag each other (RESEARCH.md, 2026-10-12).

## Decision
1. **Where.** A module in `onyx-api` (`indexer`), next to `ChainRoots` and the catalog it fills;
   it uses the same `JsonRpc` transport. No new crate: it has no sector knowledge, and `onyx-api`
   already depends (through `onyx-coordinator`) on `onyx-labels`, now directly.
2. **Finalized blocks only.** The scan never reads above the `finalized` block of the answer in
   hand, and judges a shard with `getFinalizedRoot` pinned to the highest finalized block seen.
   No rollback logic is needed; a node reporting an older head scans less, never backwards.
3. **Bounded ranges, split on any error.** `span` blocks per call (default 200, configurable to
   10,000); on an error the range halves, down to one block, and stays that size for the rest of
   the poll. Any error is treated as "too big" because public nodes signal caps differently
   (status 413, -32614, or a body limit), and splitting is always safe.
4. **Overlap re-read.** Each poll reads the last `overlap` blocks (default 100) again; logs are
   keyed by (block, log index), so a repeat is free and a log a lagging backend left out is found
   later. Two different logs at one position stop the indexer.
5. **Cursor file.** Next block plus all events, bound to (chain id, contract, deploy block),
   written atomically; statuses are recomputed after a restart from the saved events. A write
   failure is counted on `/v1/health` and the scan goes on in memory (a restart then re-reads).
6. **The event is a hint.** A shard is listed only when the operator holds its spec, the
   contract's view agrees with an event, the held rows hash to the root with the chain's binding,
   and a pinned label set passes `check_servable` against the operator's store (I-3). A `test.*`
   set stays dev-only and, there, unchecked (the existing 403 keeps it out of production; the
   fixtures have no evidence).
7. **Holdings without rounds.** The catalog file's `held` entries carry no round: a shard id
   finalizes at most once (`everFinalized`), so the event supplies it. With the indexer on, a
   file that lists served `shards` is refused, and without it a file with `held` is refused.
8. **Serving unchanged.** `ChainRoots` stays the root source for every request (defense in
   depth); the indexer decides only what the catalog lists. `LedgerRoots` is not used for chain
   roots, since it reports an off-chain ledger in the verification bundle.

## Amendment 1 (2026-10-12, review IDX-R1)
1. **An event is a hint, also at one position (replaces the stop in item 4).** Two different
   logs at one (block, log index) are both kept and counted on `/v1/health`; `getFinalizedRoot`,
   the held rows and the binding decide. One forged or buggy answer used to stop the indexer for
   good, across restarts. Judging runs even when a scan fails. *Corrected by Amendment 2:* as
   first built, a position kept the first 4 logs and dropped the rest, and a shard was served
   only if one kept log named the contract's root, so 4 forged logs read first at the honest
   log's position kept that shard out for good, also after a restart (IDX-R2-1).
2. **Oversized blocks (item 3).** The indexer's transport reads answers up to 16 MiB (10,000
   logs fit), and a block the node still will not answer whole is read for the held shards only:
   their ids as a topic-2 filter, after the block's header proves the node is up. Splitting by
   the round (topic 1, also indexed) was rejected: a block's rounds are unknown before it is
   read, and one round's many shards are the likely crowd. Base Sepolia's blocks hold 1.2 billion
   gas (2026-10-12) and the run's `finalizeShard` used 508,215, so a block can carry thousands of
   `Finalized` logs; the shards other operators hold in such a block are missed until the overlap
   reads it whole.
3. **Ended shards.** A held shard that is not final is checked with `shardState`; one no root can
   come out of any more (`NoValidRoot`, `Unresolved`, `Voided`, `Empty`, or never opened) is
   marked and not polled.
4. **Deploy block** is checked against `eth_getCode` at start; the node refuses to start on a
   mismatch or a node without old state.
5. **Cost and exposure.** The state file is written once per poll (and every 50 ranges) on a
   blocking thread; `/v1/health` is a snapshot rebuilt per poll; `ONYX_INDEX_HEALTH=summary`
   hides shard ids and error text.

## Amendment 2 (2026-10-12, review IDX-R2)
1. **The contract's view alone decides the root (replaces "agrees with an event" in item 6).**
   A kept log says which (round, shard) to judge; `getFinalizedRoot` at the indexed finalized
   block names the root and binding, and the held rows and binding are checked against those.
   No event has to name that root: the node that answers the view also answers the logs, so
   the match added no assurance, and it let forged logs keep a final shard out.
2. **A full position (4 logs) evicts only what is already refuted or redundant.** A newcomer is
   refused when its (round, shard) is already named by a kept log, is not held, or has ended.
   Otherwise it takes the slot of, in order: a log whose shard is not held or whose (round,
   shard) ended or never opened (`shardState`); a log whose (round, shard) another kept log
   also names (a root the contract contradicts first). The last log naming a held (round,
   shard) the contract has not ended is never evicted. Logs not judged yet (after a restart)
   are judged before the scan. **Guarantee:** once any log naming a held (round, shard) is
   kept, that shard is judged until the contract ends it; a final one whose held data checks
   out is served. *Corrected by Amendment 3:* as built here, "until the contract ends it" meant
   until one `shardState` answer said so, and one wrong `getFinalizedRoot` answer was cached, so
   a single lying answer kept a final shard out until a restart (IDX-R3-2); a newcomer refused
   at a full position also reopened its refuted (round, shard) first, which let a lying backend
   keep the honest log out of a full position (IDX-R3-1). Forged logs naming shards not held, rounds
   never opened, or the honest log's own (round, shard) cannot keep it out. **Not guaranteed:**
   a lying backend that answers every read of a block inside the overlap window can still
   withhold the honest log (the completeness residual below), or fill its position first with
   logs for four other held (round, shard)s the contract has opened and no other kept log
   names (only genuine shards, which are then judged on their own merits).
3. **Deploy-block check (Amendment 1 item 4)** retries node errors (3 waits, about 6 s), and
   says which cause it was: the node failing, a node head below `deployBlock` (another network
   or still syncing), a node without the old state (not retried), or a real mismatch.
4. **Bounded JSON.** A JSON-RPC answer is parsed with a budget of 262,144 values (a full
   10,000-log answer is about 150,000), refused while read; a hostile 16 MiB `[0,0,...]` would
   otherwise build about 8 million values.

## Amendment 3 (2026-10-13, review IDX-R3)
1. **No single answer is final.** An end read with `shardState` counts (not polled, evictable
   first) only when a read on a later poll also says ended, with no live read between; the
   latest end is shown. A final `getFinalizedRoot` view is cached only when a read on a later
   poll names the same one, or once it admitted the shard. A read that disagrees starts over
   from the newer read: a live read makes the (round, shard) live again. (The review proposed
   "reset to unchecked"; the newer read is kept instead, since an unchecked key can be evicted
   and a live one cannot.) A judging `eth_call` that fails is sent once more at once. A
   positive answer still admits at once: `ChainRoots` reads the root again per request.
2. **Eviction order at a full position (replaces Amendment 2 item 2's order).** (0) a log
   whose shard is not held, or whose (round, shard) ended, confirmed on two polls; (1) a log
   whose (round, shard) another kept log also names, contradicted by the confirmed view first,
   then (2) any such; (3) the last log naming a (round, shard) never judged because its
   judging failed on two polls or more (each call sent twice), more failed polls first.
   Never: the last log naming a held (round, shard) whose latest read was live, that one
   unconfirmed read said ended, or whose judging failed on one poll or was not tried yet. The
   review proposed evicting unconfirmed refutations before unchecked keys; the property test
   below found that one lying end and then one lying log answer then evicted the honest log
   for good, so one read never makes a slot evictable. *Replaced by Amendment 4 item 1:* an
   end, confirmed or not, and a failed judging no longer make a held key's last log evictable.
3. **A refused newcomer changes nothing (IDX-R3-1).** It used to reopen a (round, shard) judged
   never opened before the cap refused it. A kept log naming such a (round, shard) now marks
   it for one more read and keeps its end and its eviction rank until a read says otherwise.
4. **Guarantee.** If no question (one `eth_call` about one (round, shard), or the log reads)
   gets two lying or failing answers in a row, a held (round, shard) once kept is never
   evicted or ended, and is served at the first true view read after its root is final.
   Tested by `idx_r3_property_no_single_answer_keeps_a_held_final_shard_out` (400 seeds per
   run, up to a third of the answers lying, a restart in each; checked once at 20,000 seeds
   and at a 50% liar). *Corrected by Amendment 4:* the "not guaranteed" list here (two agreeing
   lies in a row, lies timed across calls, four genuine held keys at one position, a backend
   withholding a log from every read of its block) was a list of ways a lying node delays
   serving; review IDX-R4 found that two agreeing lying ends also made the honest log
   evictable, lost for good once its block left the overlap. Amendment 4 states the threat
   model instead of extending this list: a lying operator node can only delay serving, and the
   held-shard rescan undoes the delay once the node answers honestly.
5. **Cost.** An end and a final view cost two reads on two polls instead of one; a failing
   judging call costs two calls.

## Amendment 4 (2026-10-13, review IDX-R4; the lead's decision at strike 3)
Reviews IDX-R2, R3 and R4 each found a way a lying RPC backend keeps a held, final shard
unserved. The lead's decision, in substance: **threat-model correction.** The operator's RPC
node is trusted for the operator's own liveness. Correctness never depends on it: every served
root is read on chain again per request (`ChainRoots`), and clients verify roots on their own
nodes, so a lying operator node can only make the operator serve less, which the operator fixes
by using a node it trusts. *Corrected by Amendment 5:* `ChainRoots` reads through the same
operator node, so the per-request read adds nothing against a lying node; correctness against it
rests on clients that check roots on their own node, which is opt-in in the client. The indexer
stops hardening its liveness against a persistently lying node beyond two cheap guarantees:
1. **A chain-status end never makes the last kept log naming a held (round, shard) evictable**
   (replaces Amendment 3 item 2's order). At a full position, on a scan's read, only a log of a
   shard not held and a log repeating a (round, shard) another kept log names may be evicted
   (contradicted by the confirmed view first). A failed judging no longer makes a slot
   evictable either.
2. **Held-shard rescan.** After every start (from its third poll, once each saved (round,
   shard) has been read on two polls) and every `ONYX_INDEX_RESCAN_POLLS` polls (default 240,
   one an hour at 15 s; 0: after a start only), the indexer reads `eth_getLogs` again from
   `deployBlock` to the cursor, filtered to the held shard ids (topic 2, at most 64 per call,
   as for an oversized block), in the same capped ranges with the same split and per-poll call
   budget; it continues on the next poll when the budget runs out or a range fails. The logs
   found are merged as candidates. When a periodic rescan starts, every confirmed end is read
   again and every cached view of a (round, shard) not served is dropped, so a lie confirmed
   by two answers holds only until the next rescan. **One addition the decision did not
   spell out:** a log the rescan found may take the slot of a held log at its position that the
   same rescan answer did not return and whose (round, shard) two reads on two polls say
   ended. Without it, four forged logs naming a held shard in rounds never opened, read before
   the honest log, would keep it out for good even with an honest node (guarantee 1 makes
   them unevictable). It needs a rescan answer leaving the log out *and* two agreeing ends, so
   no single answer evicts a live key (the IDX-R3 property test still holds); the next honest
   rescan puts a log back.
3. **Admission (IDX-R4-2, pre-existing).** A (round, shard) enters the catalog only when
   `shardState` reads Finalized or two reads on two polls name the same final view: one
   lying view naming the held rows' true root for a round never opened listed that round, and
   such rounds could crowd a metric's 16 candidates (`MAX_METRIC_CANDIDATES`).
4. **What is promised** (reworded by Amendment 5). A lying operator node can delay serving a
   held shard until it answers honestly and a rescan or restart runs. It can also make the
   server list and serve a wrong or unfinal root: the per-request root read (`ChainRoots`) uses
   the same node, so correctness against a lying operator node rests on clients that check
   roots on their own node (`client::verify_response` with a chain answer; without one the
   result says `chain_checked: false`). Tested:
   `idx_r4_1_two_lying_ends_cost_no_log_and_the_rescan_reads_them_again` (the review's PoC),
   `idx_r4_1_a_log_withheld_in_the_overlap_is_found_by_the_rescan_after_a_restart`,
   `idx_r4_1_one_of_three_lying_recovers_after_a_rescan` (the review's run shape: one backend
   of three always lying, 120 polls, a restart at 60, then honest answers: served after the
   periodic rescan and after a restart, every seed), `idx_r4_relaxed_liars_recover_after_a_rescan`
   (the review's relaxed models), `idx_r4_2_one_lying_view_never_lists_a_forged_round`; the
   IDX-R3 property test is unchanged. Operators: use a node you trust (docs/ops/deployment.md
   "Chain indexer").
5. **Cost.** A rescan costs about (blocks since `deployBlock`) / span `eth_getLogs` calls per
   64 held shards, spread over polls by the per-poll budget; an admission without a confirmed
   view costs one `shardState` call. *Amendment 5 item 4* gives the rescan its own budget.

## Amendment 5 (2026-10-13, review IDX-R5: correctness, and recovery once the RPC is honest)
1. **Judging pin (IDX-R5-1).** Judging read the contract at the highest `finalized` head ever
   reported; one lying answer far ahead made every later judging call fail on an honest node
   ("header not found") until a restart. Judging now reads at this poll's answer; a (round,
   shard) all of whose kept logs lie above it (a lagging backend) waits for a later poll. The
   head shown on `/v1/health` is the last one read.
2. **Served shards are judged again (IDX-R5-2).** A shard admitted on lies (16 forged rounds
   from one lying poll filled a metric's 16 candidates) was never judged again. Every rescan
   now reads each served shard's final view again; two reads on two polls naming the same
   other view (or none) take it out of the catalog and judge it again from the start, as two
   agreeing reads admit. One refuting read changes nothing.
3. **Threat model wording (IDX-R5-3).** `ChainRoots` reads roots through the same
   `ONYX_RPC_URL` as the indexer (onyx-node `rpc()`), so "every request re-reads the root on
   chain" adds nothing against a lying operator node. Correctness against it rests on clients
   that check roots on their own node. `verify_response(…, chain: None)` makes that opt-in and
   returns `chain_checked: false`; its text now leads with it, and the docs say so plainly. The
   client's default is not changed here (THREATS IDX-R5-3, TODO M7 loop 3).
4. **Rescan cost (IDX-R5-4).** A rescan grows by about 216 `eth_getLogs` calls per day of
   deployment age per 64 held shards (Base's 43,200 blocks a day at span 200), about 79,000
   per rescan after a year, and it shared the scan's 2,000-call budget. It now has its own
   per-poll budget (`ONYX_INDEX_RESCAN_CALLS`, default 100), continues on the next poll, and the
   next periodic rescan is counted from the end of the previous one, in polls
   (`ONYX_INDEX_RESCAN_POLLS`) or seconds (`ONYX_INDEX_RESCAN_SECS`). docs/ops/deployment.md
   "Rescan cost" tabulates calls per day at 1 month and 1 year and recommends a wider span.
5. **Cursor jump (IDX-R5-5).** One lying head far ahead could move the saved cursor up to the
   scan's budget (400,000 blocks) past blocks the node had finalized. A poll now reads at most
   `max_jump` blocks (`ONYX_INDEX_MAX_JUMP`, default 100, the overlap) past the head this poll
   and the previous one both reported, or past the cursor; with `max_jump` at most the overlap,
   those blocks are read again. Cost: a start reaches the head on its second poll.
Tested: `idx_r5_1_one_lying_finalized_head_fails_one_poll_only` and
`idx_r5_1_a_lie_while_pending_never_pins_later_judging` (the review's PoC 1),
`idx_r5_2_lying_admissions_are_removed_after_a_rescan` (PoC 2),
`idx_r5_2_one_refuting_read_never_removes_a_served_shard`,
`idx_r5_4_the_rescan_has_its_own_budget_and_resumes`,
`idx_r5_5_one_lying_head_moves_the_cursor_one_overlap_at_most`; each fails with its fix
reverted.

## Amendment 6 (2026-10-13, M7 loop 3: review IDX-R6 items, client default)
1. **Admission pin (IDX-R6-1).** Each served (round, shard) keeps the judging pin (that poll's
   `finalized` head) it was admitted at. A recheck at a lower pin (a lagging backend that may
   not know the root is final yet) is not read and refutes nothing; the recheck stays due for a
   later poll. A refutation at or above the pin still counts (two on two polls remove).
2. **Rescan cursor persisted (IDX-R6-2; corrected by M7L3-R1-2).** The state file keeps a
   rescan in progress with the held set it reads for (`rescan: {next, to, held}`, `held` the
   Keccak-256 of the held shard ids in order; checked on load: `deployBlock <= next <= to <=
   cursor`). The rescan after a restart starts from that `next` instead of `deployBlock` only
   when the restarted server holds the same set (same hash): the blocks before `next` were read
   for exactly those shards by the interrupted rescan, so the union still covers `deployBlock`
   to the cursor for them. A server holding any other set (a shard added, or a state file
   written before the hash) starts the rescan again from `deployBlock`: a newly held shard was
   never read below `next`. A finished rescan clears it; a restart still runs one.
3. **`max_jump <= overlap` enforced (IDX-R6-3)** in `onyx-node`'s config (a start refuses
   otherwise), so item 5 of Amendment 5 always holds for an operator's settings. Library
   callers (`IndexerConfig`) keep the bound unchecked; the tests use an unbounded jump to read a
   fresh chain in one poll.
4. **Client default (IDX-R5-3 TODO).** The client now reads the root on its own node by
   default (`client::ClientNode::verify`; `verify_response` refuses a missing answer; the
   opt-out is `verify_response_unchecked`): ADR-0023 Amendment 2. Item 3 of Amendment 5 is
   superseded on that point; `ChainRoots` still shares the operator's node.
Tested: `idx_r6_1_a_read_below_the_admission_pin_never_removes_a_served_shard`,
`idx_r6_2_a_restart_resumes_the_rescan_where_it_stopped`,
`m7l3_r1_2_a_new_held_set_rescans_from_the_deploy_block` (each fails with its fix reverted),
onyx-node `idx_r6_3_the_jump_is_at_most_the_overlap`, onyx-api
`the_client_checks_roots_on_its_own_node_by_default`.

## Consequences
- The local rehearsal's API is no longer handed its shard: it finds round 1 by its event, and
  with `--dispute` the resumed indexer lists round 2 as known but not held.
- `--export-node` writes `held`, the manifest's `deployBlock` and `ONYX_INDEXER=on`.
- Each poll costs `eth_chainId`, one `eth_getBlockByNumber`, about (new blocks + overlap) / span
  `eth_getLogs` calls, and one `eth_call` per held shard not yet final (an end or a final view
  is read on two polls before it is kept; Amendment 3).
- Residuals (THREATS.md IDX-*): completeness relies on the overlap, not on proof that one backend
  answered the whole range; the holdings are fixed at start; a non-test label set's level still
  comes from the registry floor or the weakest level, not from its evidence.
