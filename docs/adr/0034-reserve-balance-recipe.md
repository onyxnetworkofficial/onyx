# ADR-0034: Reserve balances of published address sets (indexer Phase 2 rows 2 and 6)

## Status
Accepted, 2026-10-13 (branch `balance-recipe`; the owner: "add the entity indexer to everything").
Amended 2026-10-13 (Amendment 1, branch `reserve-api`: served through the paid API behind a
serving check). Amended 2026-10-17 (Amendment 2, branch `reserve-client`, ROADMAP D3: the client
re-runs the serving check). Amended 2026-10-18 (Amendment 3, branch `reserve-dated-client`,
ROADMAP D22: dated sets, every link on the client's headers, a span from the landing).
Implemented by `recipes/btc-entity-reserve`, `adapters/reserve-schema`,
`adapters/source-btc/src/reserve.rs`, `examples/export-reserves.rs`, `fixtures/reserves/`,
`scripts/stream-reserve-flows.py`, `scripts/recount-entity-reserve.py`,
`scripts/reserve-live-check.py` and the website's `#/reserves` page.

## Context
`docs/ops/indexer-integration.md` rows 2 (ETF issuer address lists, Bitwise BITB) and 6 (OKX's
signed proof-of-reserves addresses) need the on-chain balance of a published address set, not
its flows. Both sets are already in label set v2 (with Binance's and BitMEX's published lists).
`btc-entity-balance` can add a base to a range's flows (`BalanceParams`), but nothing produced a
base anyone could check, and its params do not say which height the base is for, so a base for
the wrong height passes silently. A balance is cumulative from genesis: rerunning all history
per shard is impossible within the fuel limit (a shard holds about 380 k inputs plus outputs).

## Decision
1. **Incremental state.** A reserve history is a chain of `btc-entity-reserve` shards. Params
   (`ReserveParams`, Borsh) carry `base_height`, `base_ref` and one balance per labelled entity.
   The recipe refuses a source that does not start at `base_height + 1` (`STALE_BASE`, code 9),
   a base that does not name exactly the labelled entities (`BAD_PARAMS`), and any transaction
   that would take a balance below zero (spends applied before outputs; `NEGATIVE_BALANCE`). It
   emits one `EntityReserveRow` per entity (also untouched ones), so the rows are the next
   shard's base. `base_ref` is the snapshot hash for the first shard and the previous shard's
   Merkle root after that; the recipe cannot check it, so `reserve::check_chain` (exporter,
   website, tests) checks every link (R-RES-CHAIN-1). A new recipe, not a change to
   `btc-entity-balance`, so the pinned history's recipe ids stay; R-RES-2 ties the two (same
   base, same rows). The schema lives in a new crate (`onyx-reserve-schema`): adding types to
   `onyx-btc-schema` changed the existing recipes' bytes.
2. **Base from a UTXO scan.** `bitcoin-cli scantxoutset` over every label of the set reads the
   UTXO set at the node's tip only. The scan is committed per address
   (`fixtures/reserves/scan-<height>.json`, label set name, version and content hash, block
   hash; `ScanSnapshot::hash`). The base at an earlier height is the scan minus each entity's
   net flow over the blocks in between (`reserve::derive_base`, Python `derive`, which also
   refuses any address whose scan minus its own flows is negative, R-RES-NEG-2), and the recipe
   chain from that base must land on the scan exactly (R-RES-SCAN-1). Landing on the scan is an
   identity (base = scan minus flows): it checks the arithmetic and the flows, not the scan. The
   scan's block must be the header chain's block at that height (RES-4). **The base is one
   node's scan.** Precondition for serving `btc.entity-reserve`: an independent re-scan on a
   second node (or an independently published source) must reconcile with the base, per
   address, before a reserve shard is served (THREATS RES-1, TODO M7 delivery). Today the base
   is cross-checked independently only for OKX (its proof-of-reserves file) and Bitwise BITB
   (the issuer's wallet list); Binance and BitMEX are not.
3. **How anyone checks the base.** A later scan reconciles with an earlier one through the flows
   between them, per address (`reserve::reconcile`, Python `reconcile`). This needs any full
   node at any later tip, not the operator's. The committed second scan (971,202) is the same
   node 3 blocks later: it shows the scans are consistent, not that the node saw every output; a
   node that omits an address in every scan reconciles with itself (review RES-R1 F1). The UTXO set's MuHash at the scan height
   (`gettxoutsetinfo muhash <height>`, coinstatsindex) is recorded in the snapshot's `method`
   note; it commits to the whole set and cannot prove a subset, so it is context, not a proof.
4. **Trust levels.** The balance of the address set given the base is level 1 (rerun over the
   blocks); the base is one node's scan, cross-checked independently only for OKX and BITB. Attribution keeps the label set's level: OKX's addresses are signed by OKX (level 3
   as a source), Binance's, BitMEX's and Bitwise's lists are published claims (level 4); a v2
   shard is level 4 overall (`label_set_proof_level`). Dust and spam inflows count: they are on
   chain at the address and cost the sender exactly what they add.
5. **Live checks, not tuned.** The method is compared with the companies' own figures
   (`scripts/reserve-live-check.py`); differences are reported as found.

6. **Params bound to the chain, supply cap (review RES-R1 F2, F3).** The recipe is a pure
   function of its params, so a rerun cannot see a forged base. `reserve::check_params_chain`
   requires each spec's params to be the committed start (first shard) or the previous
   finalized shard's end rows, height and root (R-RES-PARAMS-1); the exporter runs it, and the
   API/indexer must run it from the committed snapshot before serving (THREATS RES-7, TODO M7
   delivery, with the API metric). The recipe refuses a base or end balances summing above 21
   million BTC (`ABOVE_SUPPLY`, code 10, R-RES-CAP-1); that changed the recipe id.

## Alternatives
- Rerun from genesis per entity: exact but impossible within a shard's fuel.
- `dumptxoutset ... rollback` to an old height: rewinds the node (network off, hours); not for a
  shared node.
- Trust the PoR files' own balances: those are the claims being checked.
- Extend `btc-entity-balance`: changes its bytes and every pin of the history.

## Consequences
- Website `#/reserves`: four companies over the one-week history, checked in the browser
  against `pinned::RESERVES_*` (no label rows: the export holds entity totals only).
- The base is as good as one node's scan until a scan on another node reconciles it; a reconcile on two committed
  scans is part of the tests.
- Fuel per shard is the series' (label decoding dominates); the week reuses the history's shard
  plan and the preflight confirms each shard.
- Address-list changes need a new label set and a new scan; a scan of another list is refused.

## Amendment 1 (2026-10-13, branch `reserve-api`): serving `btc.entity-reserve`
Closes THREATS RES-7 and the RES-1 serving precondition; review RES-R2 noted the exporter's
`check_params_chain` is circular (it checks params against the base they were built from).

1. **Serving checks in the API, fail closed.** `onyx_api::gate`: a sector gate judges all finalized
   shards of its metric at once from what the server holds (binding inputs, source heights, the
   params from the row store by the params hash the shard id commits to, the leaves that hash to
   the chain's root). A shard naming a check (`servingCheck`, set on every reserve catalog entry),
   or of a metric with a gate, is served only with an answer for its current root; a server
   without that gate refuses it (403 `serving_check_unavailable`); no answer yet is 503
   `serving_check_pending`; a refused shard is 403 `serving_check_failed` on every endpoint and
   skipped by `/v1/metrics`. All refusals come before any 402. `onyx-node` takes gates through
   `onyx_node::app::main_with`; the plain binary has none, `onyx-node-btc`
   (`adapters/reserve-serve`) adds `btc-entity-reserve-v1` from `ONYX_RESERVE_GATE`. A separate
   crate because `onyx-source-btc` also builds for the browser.
2. **The check is non-circular** (`reserve::serving_check`). Inputs: the committed scan, its label
   set, a pinned start height and recipe id. The finalized ranges from the start must tile the
   blocks up to the scan's height; the start base is the scan's entity totals minus the finalized
   rows' net flows (received and sent do not depend on the base, and all shards of one range must
   agree on them); then each shard's params must be exactly the expected ones: the derived start
   base with `base_ref` = the scan's hash, then the previous served shard's end rows, last height
   and root. `check_params_chain` runs over the selected shards from the derived start, and the
   chain must land on the scan's totals. The spec under test and any base file are never inputs.
   A chain that does not reach the scan serves nothing; a shard on another base is refused while
   the honest shard of the same blocks is served (R-RES-SERVE-1..4).
3. **RES-1 policy: refuse, not flag.** A row is served only if its entity's base is cross-checked
   independently: a committed record for this scan and label set (`fixtures/reserves/
   cross-checks.json`, every compared address exact: OKX against its proof-of-reserves file,
   Bitwise BITB against the issuer's wallet list), or an operator-configured re-scan from a second
   node that reconciles at every address of the entity. Binance and BitMEX rows are refused (403
   `not_cross_checked`, before any 402). Serving them flagged `cross_checked: false` was the
   alternative; a flag a consumer can ignore is weaker than a refusal, so the safe option wins.
   Served rows carry `servingCheck {gate, crossChecked: true, basis, source}`; the client exposes
   it as `Verified.serving` and `Verified::require_cross_checked`. It is the operator's claim, not
   bound to the root, and the server cannot tell two nodes apart: a second scan is the operator's
   statement. OKX's record covers 3,558 of its 9,443 label-set addresses (by value 140,974.44 of
   140,998.90 BTC at block 965,949); the record says so.
4. **Level 4, floored on chain.** The base is an input the rerun does not check: `input_level`
   gives a reserve spec `RESERVE_BASE_LEVEL` (4) and `METRIC_MIN_LEVELS` registers the reserve
   recipe's floor at 4 (`fixtures/registry/metric-min-levels.json`, read by `OnyxRegistry.t.sol`).
   With label set v2 (level 4) the shard is level 4 either way; the floor keeps it there under a
   stronger set. No market settles on a reserve row (money policy: levels 1 and 3). The local demo
   registers `btc.entity-reserve` with the recipe and floor in one owner call and checks
   `verifyRowLeveled` returns level 4.
5. **RES-9.** The base derivation is checked per address at every height of the window
   (`reserve::check_running_balances`, Python `derive`), and the Python recount refuses a flow at
   an address outside the set, as Rust does.

Consequences: a reserve shard is served only by a gate-equipped server with the committed scan;
the API test (`adapters/reserve-serve/tests/api_reserve.rs`) serves the 89 committed shards, and
`scripts/demo-local.sh` settles one on anvil (blocks 971,192-971,199), serves it paid and refuses
a forged base. Open: the check is operator-side (clients that do not trust the operator rerun it);
no second-node scan exists yet, so Binance and BitMEX are not served; RES-4's confirmation depth
for serving stays a TODO.

Review RESAPI-R1 (2026-10-13) changed four points of this amendment:
- **Claimed by binding (F1).** A gate also claims shards by their binding (`ServingGate::claims`:
  the reserve gate takes `schema_id = keccak(SCHEMA_ENTITY_RESERVE)` or its pinned recipe id), and
  `onyx_api::gate::KNOWN_GATES` lists the reserve schema and every committed reserve recipe id in
  every server. A shard needs a gate if its catalog entry names one, its metric has one, a
  registered gate claims it, or `KNOWN_GATES` lists its binding; two different gates refuse it.
  So a misnamed catalog entry (another metric, no `servingCheck`) no longer fails open, and plain
  `onyx-node` refuses reserve shards however they are listed.
- **Landing bound to the block (F2).** A shard ending at the scan's height must have the scan's
  block hash as its snapshot's tip hash, or it is refused and is no link (THREATS RES-4: only the
  landing is bound to a block; the earlier ranges are bound by heights and the `base_ref` chain).
- **Liveness (F3).** Checks run on a timer (`ONYX_GATE_REFRESH_SECS`, default 60), with or without
  the indexer. A failed root read judges the shard at its last judged root; if it was never
  judged, the run stores nothing new, keeps every unchanged answer and leaves the rest pending.
- **Packaging (F4).** `docker build --target onyx-node-btc` ships the binary with the committed
  scan, label set, cross-checks (and only their `SHA256SUMS` lines) and `deploy/reserve-gate.json`;
  `deploy/systemd/onyx-node.service.d/btc.conf` switches the unit to `onyx-node-btc`.

Review RESAPI-R2 items, fixed in M7 loop 3 (2026-10-13):
- **Metric names in `KNOWN_GATES` (R2-1).** `KnownGate` also lists the metric names it covers
  (`metrics: &["btc.entity-reserve"]`), so a shard listed under `btc.entity-reserve` needs the
  reserve gate on every server whatever its binding: a later reserve build not yet listed (new
  recipe id and schema id) fails closed on plain `onyx-node` (403 `serving_check_unavailable`).
  Residual: such a build listed under another metric name on a server without the gate is not
  recognised (only an operator listing the shard can do that; RES-10 stands).
- **Refresh on removals (R2-2).** The gates run again after every indexer poll that admits or
  removes a shard (`gate::after_poll`), so the later links of a chain judged with a refuted link
  lose their pass at once instead of at the next timed run.

## Amendment 2 (2026-10-17, branch `reserve-client`, ROADMAP D3): the client re-runs the check
Closes the client side of THREATS RES-10: `servingCheck` is the operator's claim, nothing on chain
binds a reserve shard's params to the scan, so an operator who skips the check (or fakes a gate)
serves a forged-base row that verifies against its finalized root.

1. **The check needs no operator input.** A reserve row copies its params into itself
   (`base_height`, `base_ref`, `base_sats`), and the row is under the finalized root. So a client
   that holds the entity's row in every shard of the chain (each proven under a root final on its
   own node) and the committed scan can derive the base and check every row's params, for one
   entity, without the params blobs or the other entities' rows. `reserve::derive_entity_chain`:
   the links must tile from one base height to past the scan; the start base is the scan's total
   minus the links' net flows up to the scan; each row's `base_ref` is the scan's hash (first) or
   the previous link's root, its `base_sats` the derived base (first) or the previous end, its
   balance adds up, and the landing's balance is the scan's total. `reserve::client_check` adds
   the binding (pinned recipe, label set, reserve schema), the entity's leaf index, and the
   landing on the scan's block (RESAPI-R1-2). The browser runs the same function on the week
   export (`reserves_check`: the base is derived, never read from the export).
2. **The client pins the scan itself** (`ReservePin::from_scan`: the scan file and label set the
   client holds; it recomputes the hash and totals, and compares the hash with one it trusts: the
   repository's, the website's `RESERVES_SNAPSHOT`). A pin taken from the server would anchor a
   chain built on a forged scan (R-RES-CLIENT-3 shows the operator's own scan file passes its own
   chain). The pin names the label set, so a chain over another label set version is refused.
3. **Discovery: `/v1/served/{metric}`**, free, lists the shards a gated metric's check cleared
   (round, shard id, range, root; no row). It is a hint: a shard left out, added or reordered fails
   the client's check. Withheld like the rows (403 `label_rights_withheld` under
   `ONYX_RIGHTS=fallback`, before anything is read; every listed shard passes the serve-time
   checks). The client fetches each link through `/v1/proofs/{round}/{shard}/{index}` (paid like
   any row; the index comes from the pin), verifies each with `ClientNode::verify_row` (new in
   `onyx_api::client`: `verify_response` plus the binding inputs, source reference and proven row,
   chain read required), and `onyx_reserve_serve::client::check_reserve_answer` runs the check and
   requires the answer it was given (`/v1/metrics` or `/v1/proofs`) to hold one link's row (same
   root, index and row bytes) ending at or after the client's `min_as_of`. A `/v1/metrics` reserve answer cannot be checked with
   `Expect::Metric` (each shard's params hash carries its base), so equality with a proven link is
   how the answer is bound.
4. **Alternatives.** Ship the chain inside every answer: 89 links per row, and a new key would make
   older clients refuse the answer (unexpected field). Put the link list inside `servingCheck`:
   the same break. Discover links from `RootFinalized` events on the client's node: operator-free
   but a log scan per query; the hint plus the check gives the same guarantee. Bind params on
   chain (a registry of reserve anchors): the real fix for every client, a contract change, D21.
5. **What the client still cannot check** (a full node's job): that the scan saw every output at
   the listed addresses (RES-1; a second node's scan or a published source), and that the ranges
   before the landing are on the best chain (RES-4: only the landing's block is bound; the client
   can check the scan's block against its own headers with `ScanSnapshot::check_block`). Cost: one
   paid row per link (89 today, at the operator's price and decay).
6. **D1 compatibility (corrected by review D3-R2-3).** Date-ranged membership (D1, ADR-0037,
   not on main yet) binds each shard to its window's label set and moves an entity's base at a
   membership change by the balances of the addresses that join or leave. This check refuses
   both (one pinned label set hash; every base from the previous end or the flows), so it fails
   closed on a dated chain and needs work after D1 merges (ROADMAP D22: pin over the dated set,
   the window hash per link, the change deltas, every link's tip on the client's header chain).
   A format-1 set read as dated has one window, so the committed chain checks as before.

Review D3-R1 (fresh, 2026-10-17) changed four points of this amendment: the same work finalized
in two rounds (same range and root; the shard id has no round) is one link, not a broken chain
(R1-1); the client checks the list's shape before its first paid fetch (`MAX_LINKS`, consecutive
ranges, one landing; R1-2); the answer is bound to a link by root, index and row and must end at or
after the client's `min_as_of` (R1-3); `ReservePin::from_scan` takes the hash the client trusts and
refuses any other scan, and `ProvenRow`'s fields are private (R1-4). `/v1/served` skips a shard that
fails the serve-time checks and has a per-client budget (R1-5).

Review D3-R2 (fresh, 2026-10-18): the answer is placed in the list before the first paid fetch,
and the list is cut after the later of the answer's range and the landing (`link_requests` takes
the answer; R2-1). Cost bound stated: at most `MAX_LINKS` rows (40.96 USDC at 2,500 micro-USDC),
about 4,700 a year after the scan, and a cliff at `MAX_LINKS` because the chain must start on the
pinned scan (R2-2, TODO D22: a span check from the landing). Item 6 corrected (R2-3).

## Amendment 3 (2026-10-18, branch `reserve-dated-client`, ROADMAP D22): dated sets, headers, spans
Closes THREATS D3-R2-2 (cost grew with the chain and had a cliff at `MAX_LINKS`) and D3-R2-3 (the
client check failed closed on D1's dated chains, ADR-0037), and the header half of RES-C6.

1. **A span from an anchor, not a chain from the start.** A row's flows are reruns (level 1); only
   its base is a param. So one known balance at one link fixes every base of a run of consecutive
   links, and the first link's `base_ref` adds nothing to the values. `reserve::derive_entity_span`
   (shared by the API client's `client_check` and the browser's `reserves_check`) takes one
   entity's rows over consecutive links, finds the anchor, derives the first base from it and the
   flows, then walks forward: each `base_sats` is the derived base (first) or the previous end
   plus the entity's membership-change delta, each `base_ref` the previous link's root, each
   balance adds up. Anchors, in order: the **landing** (the link ending at the pinned scan's
   height, on its block: its balance is the scan's total over the members at that height); with a
   dated set, the entity's **exit** (a span before the scan whose entity has no address right
   after its last link: that link's end is the leaving addresses' balances) or **entry** (a span
   after the scan whose entity has no address right before its first link: that base is the
   joining addresses' balances); exit and entry come from the scan and the trusted flows file, as
   the serving check's rebase does (RES-13 residual, unchanged). D3's rule that the first
   `base_ref` be the pinned scan's hash is dropped: it bound the chain to its start, so the cost
   grew with the chain and a newer scan could not be pinned. A swapped or forged scan is still
   refused, by its totals at the landing (`a_chain_on_a_swapped_scan_is_refused`).
2. **Any later scan the client trusts can be pinned.** The landing is wherever a link ends at the
   pinned scan's height, so a scan reconciled with the chain's (`reserve::reconcile`) anchors the
   links between it and an answer (`d22_a_newer_reconciled_scan_can_be_pinned`). Which scans the
   client trusts stays its own pin (repository or website hash; on chain with D21).
3. **Cost.** `link_requests` walks from the answer toward the landing and stops there, or before a
   range in whose window the entity has no address; the span is what is fetched. No cap on the
   list (`MAX_LINKS` removed; the free list is checked for shape as before); a span longer than
   the client's budget is refused before the first fetch (`DEFAULT_SPAN_BUDGET` 4,096 rows, 10.24
   USDC at 2,500 micro-USDC). Cost is `|answer - landing| + 1` links: on the committed week 1 row
   (0.0025 USDC) for the landing's answer and 89 (0.2225 USDC) for the oldest; a fresh answer `w`
   weeks after the pinned scan at 89 shards a week is `1 + 89w` rows, whatever the chain's age
   (the default budget covers `w` up to 46). D3 cost 89 rows for every week answer and refused
   honest chains past 16,384 links.
4. **Dated sets (D1).** `ReservePin::from_dated_scan` takes the scan, the dated set, its earlier
   versions (`check_history`, as the gate: a version above 1 needs its history), the flows file
   and the hashes the client trusts for the scan and the flows file (`FlowsFile::hash`, Borsh,
   domain `onyx-reserve-flows-v1`; a flows file taken from the operator would let it move a
   change's delta). Each link's binding must name the pinned set's window for its blocks
   (`Epochs`, shared with the serving check), reach no further than the set's publication, and
   carry the entity's row at its index in that window (indices move when an entity that sorts
   first joins). The delta at a change is the serving check's `change_at` for that entity.
   `from_scan` reads an undated set as one window (`DatedLabelSet::from_undated`). A pin checks
   one set version: windows carry the version in their hash, so a span over shards bound to an
   older version's windows is checked with a pin on that version (the serving check has the same
   limit; review D22-R1-4).
5. **Headers (RES-4).** `client_check` takes the client's own `HeaderChain` (proof of work and the
   retarget rule checked): the pin's scan block and every flows-file block must be on it with six
   confirmations (`ReservePin::check_on`: a stale pin is refused), and so must every link's tip,
   not only the landing's.
6. **Review D22-R1 (fresh, 2026-10-18).** No wrong balance accepted. Fixed: what the client can
   refuse for free comes before the first paid fetch (`link_requests` takes the client's headers
   and `Limits`: a stale pin, an answer older than `min_as_of` or under six confirmations; each
   `LinkRequest` carries the listed range and root, and `matches` lets a client stop at the first
   misdescribed link, R1-1); the free list has a memory bound (`DEFAULT_MAX_LISTED`, 1,048,576
   entries, about 225 years of the committed pace; not a cost cliff, R1-2); a flows file's summary
   fields must agree with its blocks, which the hash commits (R1-3); item 4's one-version limit
   stated (R1-4).
7. **Review D22-R2 (fresh, 2026-10-18).** A flows file under a trusted hash that disagreed with
   the finalized flows moved a change's delta, and a chain whose bases followed it was accepted
   (a wrong balance; the serving check already refused such a file). Fixed: `derive_entity_span`
   checks the file's per-entity flows against every link it covers, as the serving check does
   (R2-1); `FlowsFile::check_consistent` refuses a repeated or missing block (R2-2); the free
   list is bounded before it is parsed (`client::parse_served`, R2-3).
8. **Not done.** The browser's check runs on the committed week export without headers (its tips
   against a header file are D12). The flows file stays the client's word under a trusted hash
   for the split of one entity's flows between its addresses at a membership change (RES-13,
   D11).
