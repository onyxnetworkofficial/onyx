# ADR-0032: Mining-pool block share by coinbase (indexer Phase 2, item 5)

## Status
Accepted, 2026-10-11 (the loop decides; plan row 5 of `docs/ops/indexer-integration.md`).
Implemented by `recipes/btc-pool-blocks`, `adapters/pool-schema` (`onyx-pool-schema`),
`onyx_source_btc::pools`, `examples/export-pools.rs` and the website's "Who mined the blocks"
chart. Review POOL-R1 (2 blocking: a real miner handle in a test and raw coinbases with miner
handles in a fixture) was fixed on a fresh branch so that main's history never held them.

## Context
The indexer's `mining_pool_payouts.csv` treats pool payout addresses as entity labels. That was
refused as a label set (LBL-V2-F1, ADR-0031): payouts often go to individual miners. What is on
chain and checkable is narrower: each block's coinbase transaction carries a free-text scriptSig
and pays some output scripts. Public lists map those to pools. One is bitcoin-data/mining-pools
(https://github.com/bitcoin-data/mining-pools, MIT); the indexer holds a copy of its `generated`
branch (`fixtures/groundtruth/pool-list.json`, 149 pools, committed in indexer commit 9520235 on
2026-08-10; SHA-256 `ddc505c8…ef0e`). That upstream file is not committed here: some of its entries
are people's names or handles. The coinbase text is chosen by whoever built the block: some
blocks carry a person's handle (e.g. solo miners' "Mined by ..." strings).

## Decision
1. **A block metric, not labels.** Metric `btc.pool-blocks` (registry name
   `crypto.btc.pool_blocks`, schema tag `onyx.btc.pool-blocks-row.v1`): per block range, one row
   `PoolBlocksRow { pool, from_height, to_height, blocks }` per pool that matched at least one
   block, plus `unknown` for the rest, keyed by the pool name's bytes. No label set is pinned, so
   a shard is level 1 (anyone with the blocks reruns it).
2. **Source.** A new canonical `CoinbaseSource` (`onyx-pool-schema`; a separate crate so the
   existing recipes' WASM bytes and ids do not change): per block, height, hash, coinbase
   scriptSig and output scripts. The adapter builds it from raw blocks or from committed proofs
   (header + coinbase + Merkle branch) and checks proof of work against the mainnet floor,
   parent links, the BIP-34 height and the coinbase's inclusion under the header's Merkle root.
3. **Reviewed allowlist (R-POOL-1, POOL-R1-F4).** `scripts/build-pool-allowlist.py` turns the
   pinned upstream file (checked against its SHA-256) into `fixtures/pools/pool-allowlist.json`,
   which is all that is committed. Every upstream id gets one recorded decision: a pool is named
   only with a company website (http(s); not a github.com issue, a forum post, a bare IP address
   or an explorer page), not run under an operator's personal handle, not a person's name, and
   with at least one non-empty tag or address. Excluded ids are recorded by id and reason class
   only, never by name (`EXCLUDED_POOL_IDS`; `fixtures/pools/PROVENANCE.md`). 127 of 149 pools
   pass (id 115 excluded in review POOL-R2: its site is on an operator-handle domain). Everything else is never named: its blocks count as `unknown`.
4. **Matching (R-POOL-2), per block, in this order:** (a) the first pool in table order (list
   id order) owning the script of the coinbase's largest-value output (first such output on ties;
   zero-value outputs never count). Only the largest output is the payout, so a coinbase paying
   many parties is not credited to a listed pool because one small output is that pool's address
   (POOL-R1-F3). Payout addresses are only the list's own `addresses` for that pool, decoded as
   mainnet addresses; (b) else the pool whose tag occurs earliest in the scriptSig (byte substring; ties at one
   offset: the longer tag, then table order). A pool writes its own tag first, ahead of any text
   it lets a miner add, so a miner on a template-sharing pool (Ocean: "OCEAN.XYZ" at byte 7 of
   all 26 Ocean coinbases of the week) who puts another pool's tag in their own text does not move
   the block (POOL-R2-F3); (c) else `unknown`. Payout beats tag. The week result is unchanged by
   this rule (no week block matches two tags).
5. **No free text (R-POOL-3).** The output carries only table names and `unknown`. The recipe
   refuses a table whose names are not printable ASCII, are duplicated or are `unknown`.
6. **Pinned table.** `ShardSpec.params` is the Borsh `PoolTable { list_hash, pools }`, so the
   table (and the keccak of the list file it came from) is in the shard id and the commitment.
   The site pins the table hash and a week commitment
   `keccak("onyx-pools-v1" ‖ recipe id ‖ keccak(table) ‖ tip ‖ per day: from ‖ to ‖ root)`.
7. **Checks.** Golden on 11 committed mainnet coinbases (970,019-970,029, chosen because each
   carries only a pool's own tag and pays at most two outputs: committed fixtures must never carry
   a person's coinbase text, POOL-R1-F2; `tests/pool_review.rs` scans every tracked file) and on the week
   (1,008 blocks, gated on the fetched blocks), an independent Python recount
   (`scripts/recount-pool-blocks.py`), the preflight (each day is one shard, at most 1.25% of the
   fuel limit, `docs/ops/benchmarks.md`), browser checks of every day's root against the pins, and `/v1/metrics` serving a
   row by company name with its proof.

## Consequences
- "Block N's coinbase names pool X" is level 1; the share rows are level 4 (Amendment 1).
  "Pool X mined block N" is not proven: a pool can mine for another, change its tag, or a miner can copy a tag (THREATS POOL-1). The page says the
  table is the list's claim, not proof of ownership.
- The list maintainers are trusted for the name-to-tag mapping; a list update is a new table hash,
  a new shard id and new pins, reviewed like any data change (POOL-2).
- Short tags in the table (`EMC`, `🐟`, 3-4 bytes) can match unrelated coinbase
  text (POOL-3).
- The one-week result on the site: Foundry USA 26.6%, AntPool 19.8%, F2Pool 17.6%, unknown 0.4%
  (4 of 1,008 blocks); `docs/ops/benchmarks.md` has the run.
- Attribution: the list's MIT notice ships in `fixtures/pools/LICENSE-mining-pools.txt`;
  provenance in `fixtures/pools/PROVENANCE.md`.

## Amendment 1 (2026-10-11, audit AUD-2-F6): pool shares are floored at level 4
"Block N's coinbase names pool X" is level 1, but the shard's rows are pool shares, and which pool
owns a payout address or a tag comes from the published list: a claim. A shard is as strong as its
weakest input (`ProofLevel::weakest`), so a pool-share shard is level 4.
- `onyx_source_btc::POOL_TABLE_LEVEL = BondedClaim`; a catalog applies it with
  `ShardMeta::with_input_level` (it only weakens), and an `onyx-node` catalog file declares it per
  shard with `"inputLevel": 4`.
- Under the default market policy (levels 1 and 3) a pool-share shard is refused for settlement
  (`adapters/source-btc/tests/api_pools.rs`).
- The site's data map shows pool shares at level 4, and the "Who mined the blocks" text says why.
- Amendment 2 (AUD-2R-F3): the floor is bound on chain. The registry's owner registers
  `crypto.btc.pool_blocks` with minimum level 4 (`OnyxRegistry.registerMetric` with its floor, keyed by recipe since REG-F;
  `onyx_source_btc::METRIC_MIN_LEVELS`, vectors `fixtures/registry/metric-min-levels.json`), so
  `attestShard` refuses a pool shard at a stronger level, and an `onyx-node` with a pinned registry
  floors every catalog shard at its on-chain `levelOf` (unattested or not final: level 5). The
  adapter derives the level from the spec's schema (`onyx_source_btc::input_level`), not from
  config. A node without a pinned registry (local demo only) still relies on `inputLevel`
  (THREATS AUD-2-F6, AUD-2R-F3).
