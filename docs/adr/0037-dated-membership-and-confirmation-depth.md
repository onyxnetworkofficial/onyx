# ADR-0037: Dated label-set membership and confirmation depth for reserves

## Status
Accepted, 2026-10-17 (ROADMAP D1, branch `reserve-ranges`). Closes the TODOs of THREATS RES-2
(date-ranged membership) and RES-4 (confirmation depth). Builds on ADR-0017 (label-set versions),
ADR-0034 (reserve balances) and its Amendment 1 (the serving check).

## Context
- RES-2: an issuer rotates wallets (Bitwise's list has 52 rotated-out BITB addresses in label set
  v2, `fixtures/labels/PROVENANCE.md`), so an undated set keeps counting coins at addresses the
  entity no longer names. The residual was over-inclusion: a reserve at height H could include
  addresses that were not the entity's at H.
- RES-4: a UTXO scan is taken at the node's tip (0 confirmations); only the landing shard was bound
  to the scan's block (RESAPI-R1-2). Earlier shards were bound by heights and the `base_ref` chain,
  so a shard over a stale branch before the landing was not detected, and flow files were not
  checked against proof of work.
- Recipes read a plain `LabelSet` (Borsh `LabelSetView`); changing `Label`'s bytes would change
  every recipe input, golden and pinned history.

## Decision
1. **Dated sets are a new format, not a new `Label`.** `onyx_labels::dated::DatedLabelSet { name,
   version, published, members }` with `Membership { label, from, to: Option<u64> }`: the address
   counts for the entity at every block height `from..=to` (`None`: still listed at `published`).
   Members are sorted strictly by `(address, from)`; two ranges of one address never overlap
   (whatever the entities: an address is never in two entities at once); adjacent ranges with the
   same label must be one range; no range starts after `published` and no closed range ends at or
   after it. Heights come from the source's own dated publications: `from` is the first block at or
   after the publication that first lists the address; `to` is the block before the first
   publication that no longer lists it. Format 1 (`LabelSet`) keeps its bytes, hashes, goldens and
   pins; `DatedLabelSet::from_undated` reads it as one membership per label over all heights.
2. **Ranges are committed in Merkle leaves.** One leaf per membership (Borsh `Membership`), the
   members root over them (onyx-merkle), and `content_hash = keccak(Borsh{domain
   "onyx-dated-label-set-v1", format 2, name, version, published, member count, root})`. A client
   checks one address's range with a leaf proof (`member_proof`, `verify_membership`). Shared
   vector: `fixtures/dated/vectors.json` (`ONYX_BLESS=1 cargo test -p onyx-labels`, only
   with a change to this ADR).
3. **Recipes are unchanged; shards run windows.** A shard over blocks `f..=t` runs
   `window(f, t)`: the labels valid at every height of the range, as a plain `LabelSet` (same name
   and version, its own content hash). A membership change inside the range is refused: shards
   start at every change. The `btc-entity-reserve` recipe id and every pin stay.
4. **Versions may not back-date.** `check_successor`: version n+1 has the same name, the next
   number, a later `published`, other content, and exactly the previous version's memberships at
   every height up to the previous `published`. It may close an open range at or after that height
   and add ranges that start after it; it may not add, end, move or re-attribute one earlier. So a
   reserve already served for a height cannot be rewritten by a later list.
5. **Membership changes in a reserve chain.** At a change between two shards (end of A at `h`,
   start of B at `h + 1`), B's base is A's end rows minus the balances after `h` of the addresses
   that leave each entity, plus those of the addresses that join (an address that moves between
   entities does both). An entity whose last address leaves must end at zero. The changing
   addresses' balances come from the committed scan rolled back (or forward) with a per-address
   flows file (`scripts/stream-reserve-flows.py`). The serving check derives the start base as the
   scan totals over the members at the scan's height, minus the finalized net flows, minus what
   every change moved; the walk then checks every shard's params against that, and
   `check_params_chain` runs over each run of shards between changes. Wherever the flows file
   holds a whole shard, its per-entity sum over that shard's window must equal the finalized rows
   (level 1); it may hold only addresses of the set, and no address may go below zero after any
   of its blocks.
6. **Confirmation depth: 6.** `reserve::MIN_CONFIRMATIONS = 6`, counted as Bitcoin Core counts
   (the block itself plus five on top). Bitcoin whitepaper section 11
   (https://bitcoin.org/bitcoin.pdf): an attacker with 10% of the hash power catches up from 5
   blocks behind with probability 0.0009137, under 0.1%. A scan, a flows file (its last block) or
   a shard (its last block) is used only with 6 confirmations; a second-node re-scan counts as a
   cross-check only with 6.
7. **Proof-of-work evidence, before the landing too.** The gate takes a header chain
   (`HeaderChain`, `fixtures/reserves/headers-969839-971207.json`, built by
   `scripts/build-reserve-headers.py`): every header meets the pinned floor
   (`node::MAINNET_MIN_BITS`) and its own target, and names the one before. Every finalized shard's
   last block (its source snapshot's tip hash) must be the chain's block at that height: a shard
   over a stale branch is refused wherever it is, and the chain stops there unless an honest shard
   of the same blocks exists. A flows file must be on the chain block by block, its first block's
   parent included. The committed chain is our own node's raw blocks to 971,199 plus 971,200-971,207
   from mempool.space, with hashes cross-checked on blockstream.info and three of them against our
   own node's flows file (the S2 precedent: consensus data, checked here by proof of work).

## Alternatives
- Add `from`/`to` to `Label`: changes every recipe input and every pinned history; refused.
- Per-shard dated rows in the recipe (the recipe drops addresses inside a range): needs a new
  recipe id and per-address state in WASM; the window rule gets the same result with no recipe
  change.
- Count a joining address from zero: its coins received before the publication would appear as
  an inflow at the change; counting its balance at the change is what "valid at H" means.
- Depth 1 or 3: weaker than common practice; 100 (coinbase maturity) would hold every new scan for
  about 17 hours with no matching threat.
- Trust the node's own `confirmations` field: one node's word; headers are checkable by anyone.

## Consequences
- The committed reserve chain (label set v2, scan 971,199) is served as before: the scan has 9
  confirmations in the committed header chain, every one of the 89 shards ends on it. Without the
  header file the gate does not start.
- No dated label-set fixture ships: the per-row publication dates live in the indexer's snapshot
  archives, which this branch did not read (the sources' rights are pending L8-5, and no source is
  added). Version 3 of the Bitcoin set as a dated set is ROADMAP D10.
- Residual (THREATS RES-13): the changing addresses' balances at a change are the flows file's
  word per address: flows outside their ranges are rerun by no shard, and inside them the file is
  checked against the finalized rows only as per-entity sums. A file that misstates them moves the
  reserve before a change, consistently. The file is on the header chain and anyone can recompute
  it from the blocks (TODO D11: rerun every flow of each changing address per address).

Review D1-R1 (fresh, read-only, 2026-10-17) changed four points:
- **History checked where used (F2).** The gate takes every earlier version (`datedHistory`) and
  runs `check_history` (version 1, then `check_successor` each step); without it a dated version
  above 1 is refused, so item 4 holds at serving time, not only for publishers.
- **Nothing after the publication (F3).** Only heights up to `published` are frozen, so a scan
  after the set's publication and a shard reaching past it are refused.
- **Floor pinned (F4).** The gate config has no `minBits`; headers meet `MAINNET_MIN_BITS`.
- **Canonical spellings (F6).** A dated set file must spell every address canonically, so the
  overlap rule cannot be passed with an upper-case copy of the same script.
- `VersionLog` still records format-1 manifests only; a manifest format field and
  `append_dated` come with the first published dated set (D10).

Review D1-R2 (fresh, adversarial, 2026-10-17) changed one point:
- **Retarget rule (RES-20).** Item 7's headers also keep the bits of the header before them
  unless a 2,016-block period starts at their height, where the target moves at most 4x
  (`node::permitted_bits_transition`). The floor alone let six confirmations be mined at a quarter
  of the real difficulty. Timestamps are not read and one header file is compared with nothing
  (D12).

## Amendment 1 (2026-10-18, ROADMAP P2a, absorbs D10): issuer rosters as label set v3

Context: the first dated fixture. Thirteen issuers were asked for (P1 survey); eleven publish or
sign their own Bitcoin rosters in a form Onyx may hold under the rights rule (L8-5, L14).
wBTC/BitGo stays out (wbtc.network's terms forbid redistribution, IDX-R1-F1) and Coinbase cbBTC
stays out (terms unread, rows via a Wayback capture and a person's relay; IDX-R1-F2, D4).

1. **Rosters become memberships by one rule** (`onyx_labels::roster::derive`). Each dated
   snapshot of an issuer lists addresses at a block height. A listing supports membership for
   `MAX_LISTING_AGE = 8,928` blocks (62 days at the 144-blocks-a-day target: one missed monthly
   publication). A range opens at the height of the snapshot that lists the address; it is
   extended by each later listing while still alive; it closes the block before the first later
   snapshot that omits the address, or when its last listing is 8,928 blocks old, whichever is
   first. A range alive at the set's `published` height stays open. A grade change (published to
   signed or back) starts a new range. This bounds item 1's "until a publication drops it": a
   one-off disclosure (Crypto.com, 2022-11-11) supports 62 days, not four years.
2. **Evidence is the snapshot record.** Each membership cites the first and the last snapshot of
   its range as `EvidenceRef { kind: "roster.signed" | "roster.published", content_hash }`, the
   hash being `keccak(Borsh{"onyx-roster-snapshot-v1", SnapshotRecord})` with the source id,
   entity, issuer URL, issuer date, fetch date, height, how the height was found and the sha256 and
   keccak-256 of the raw snapshot bytes. The evidence sits in the Merkle leaves, so a member proof
   also proves which publication justifies it. Confidence: 990,000 ppm when signed, 900,000 when
   published (a fixture convention, not a calibrated score).
3. **Signed means verified here.** A row is `signed` only when the issuer's published Bitcoin
   message signature verifies in Onyx (`onyx_source_btc::signed_roster`, a port of the indexer's
   `verify-signed-addresses.py` rule: script hashes to the address, enough distinct recovered keys
   in it; for a key-hash address the published key must hash to it, a check the Python lacked,
   review P2a-R1-1), over a message that names the issuer (OKX, CoinEx; HTX signs as "huobi"; any
   other message leaves the row `published`, P2a-R1-2). A failed or uncheckable signature leaves
   the row `published`; a published script that does not hash to its address drops the row,
   signed or not. A signature shows the key signed that message at some time; the snapshot dates
   the listing.
4. **Collisions and filings.** An address two entities claim at overlapping heights is dropped
   for both; an address a government filing names (`fixtures/rosters/deny.txt`, addresses only)
   is dropped (LBL-V2R-F1). The list holds only roster addresses a filing names, so it is
   current only for the rosters it was computed against: `deny.basis.json` records their digest
   and the filings file's sha256, and `build-rosters.py listings` refuses to run once the roster
   addresses change until `deny` is re-run (review P2a-R2-1).
5. **Heights.** Taken in this order: the height the snapshot states (OKX, BitMEX, Binance, HTX,
   CoinEx: basis `issuer`); else the first block after the snapshot time by the indexer's
   header-timestamp mapping (MEXC, Bybit, Bitfinex 2022, Crypto.com: `indexer-mapped`, with the
   block times kept in `fixtures/rosters/<source>/snapshots.json`); else the indexer's tip at the
   fetch plus one (`fetch-tip`) or a later observed block that bounds it (`bound`). Onyx has not
   yet checked the mapped heights against headers (THREATS ROS-3, TODO D12).
6. **Format-1 versions may precede the first dated version** (`check_history`): v3 follows the
   undated v1 and v2 of the same name. An undated set claims every height, so no dated set can be
   its successor; data served under v1 or v2 stays bound to their content hashes (every reserve
   pin names its set). Undated versions only as a prefix, numbered from 1 with no gap, each with
   other members (an undated version's header carries no publication height, so "other
   content" means other labels there; a dated successor may still republish the same members
   with a later header, as before).
7. **Reproducible from committed files.** `fixtures/rosters/` holds each source's raw snapshot
   files where they fit (re-gzipped with mtime 0) or a deterministic extract (OKX's Bitcoin rows,
   address lists from PDFs and pages), with the raw hashes; `scripts/build-rosters.py listings`
   and `build-label-set-v3` rebuild the set byte for byte (`labels_v3`). Snapshots before
   2026-01-01 of OKX, BitMEX and MEXC are recorded by hash only (93k, 336k and 28k addresses per
   snapshot). Because of item 4 of the decision, v4 cannot add them under this name. CoinEx and
   BitMEX are committed as extracts too (worksheet cells; height and address section) so no
   workbook metadata or customer deposit custody ships (review P2a-R1-4).
8. **Proof level of a shard over v3:** level 4 when its label set is exactly v3's window of the
   shard's own heights and holds a label (`dated_window_proof_level`); anything else level 5.
   The loader also binds each member's range to its cited snapshots (closed by expiry, open only
   when alive at the publication) and refuses a dated file published at `u64::MAX`.

## Amendment 2 (salted rows for restricted sets, format 3; partner tracker T1, 2026-10-24)

A partner (partner, 2026-10-22, Q7): a Merkle proof exposes sibling hashes, and an unsalted
low-entropy row can be guessed from one. True for format 2: the buyer of one membership can
confirm a guessed sibling membership exactly (`salted::tests::a_guessed_sibling_can_be_checked_in_format_2_but_not_in_format_3`).
It matters only while a set's rows are not public.

1. Format 3 (`onyx_labels::salted`): each row is `borsh(membership) || salt_i`, `salt_i =
   keccak256("onyx-row-salt:v1" || set_secret || i)`; content hash domain
   `onyx-salted-label-set-v1`, format 3. The Merkle library and `OnyxMerkle.sol` are unchanged.
2. A buyer gets its row's salt with the row; validators get all rows and salts.
3. Format 2 keeps its bytes, hashes and goldens. Use format 3 for any set whose rows are not
   public (rights not cleared, private tiers); public sets gain nothing from it.
4. Not done: no published set uses format 3 yet; the serving path, the browser verifier and the
   on-chain registry do not read it. Wiring is a follow-up when the first restricted set is sold
   row by row (TODO D30).

## Amendment 3 (serving salted rows; ROADMAP D30, pre-public loop PP3, 2026-10-25)
1. **The wire form.** `onyx_labels::salted::SaltedAnswer` (feature `json` for its JSON): the set
   header (name, version, publication height, member count, root), the row index, the row
   (`borsh(membership) || salt`) and its proof. `contentHash` travels with it for reading only.
2. **The client check** (`verify_salted_answer`): the header hashes to the content hash the client
   pinned itself; the proof is for this index in a tree of `members` leaves; the row is exactly a
   Borsh membership and a 32-byte salt (re-encoded byte for byte); the Merkle path leads to the
   root. The browser runs the same function (`check_salted_answer`).
3. **The route.** `GET /v1/labels/{set}/{address}` returns every membership of the address as an
   answer, never another row's salt and never a format-2 row. A node loads sets from
   `ONYX_SALTED_LABELS` and refuses any set whose name does not start with `test.`: real sets whose
   rights are not cleared are never served from here. Free, 30 a minute per client, `no-store`.
4. **Shared vectors.** `fixtures/dated/salted-vectors.json` (synthetic: example entities, `test:`
   addresses): four rows and five refusals, run by the library, the node and the browser.
5. **Not done (ROADMAP D31):** the registry entry of a format-3 content hash, per-row payment, an
   operator tool for set files, and any real set.

