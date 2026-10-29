# Spec: recipes

## Scope
Open, version-pinned, content-addressed metric code. Covers the ABI, the determinism profile,
the registry and versioning. Decision record: ADR-0004.

## Interfaces
- Identity: `RecipeId = keccak256(wasm_bytes)`. A changed byte is a new recipe.
- ABI v1 (`crates/onyx-recipe-sdk`): exports `onyx_alloc(len) -> ptr`, `onyx_run(ptr, len) -> u64`
  (packed out ptr and len), `onyx_abi_version() -> u32`. No imports.
- Input: Borsh-encoded `onyx_recipe_sdk::RecipeInput { abi_version, source, labels, params }`.
  `source` is the adapter's canonical bytes for the pinned snapshot; `labels` is the Borsh of the
  pinned `LabelSet` (decoded in WASM via `LabelSetView`, layout-tested); `params` is `ShardSpec.params`.
- Adapters only load and verify source data. Every transformation, including turning label
  addresses into scripts, happens inside the recipe so all logic runs in the checked sandbox
  (THREATS T-ADAPT-1).
- Output (ADR-0013): Borsh `RunReturn::Ok(RecipeOutput { rows: Vec<OutputRow { key, value }> })`
  or `RunReturn::Err(code)`; rows non-empty and strictly ascending by key. Merkle leaf = Borsh of
  each `OutputRow`. Every failure commits one canonical failure leaf.
- Reference recipe `btc-entity-balance` (recipe id in `recipes/out/btc_entity_balance.wasm.keccak`):
  seed-address entity flows over a block range; optional starting balances via `BalanceParams`;
  failure codes in `btc_entity_balance::code`.
- Series recipe `btc-entity-flow-series` (recipe id in
  `recipes/out/btc_entity_flow_series.wasm.keccak`; schema tag `onyx.btc.entity-block-flow-row.v1`,
  metric `crypto.btc.entity_flow_series` in `fixture_registry_series`): the same input as
  `btc-entity-balance`, `params` must be empty; one `EntityBlockFlowRow { entity, height,
  received_sats, sent_sats, net_sats, tx_count }` per (entity, block) touched, keyed
  `entity ‖ 0x00 ‖ height (u64 BE)`. Golden: `fixtures/golden/btc-entity-flow-series-*.json`,
  recounted by `scripts/recount-entity-flow-series.py`.
  - R-SERIES-1: only touched (entity, block) pairs get a row; a missing pair means zero flow and
    zero transactions. An entity id containing a zero byte is refused (`BAD_LABELS`), since it
    would break the key order. A range where no labelled entity is touched at all ends with
    `NO_ACTIVITY` (code 8, `onyx_btc_schema::SERIES_NO_ACTIVITY`): a shard needs at least one
    leaf, so it commits the canonical failure leaf of `RecipeError(8)`. That is a valid result,
    not an error in the data: every honest run reaches it, miners commit it and validators agree
    (`Miner::work` commits deterministic failure leaves), and readers (the website verifier,
    `export-demo`) show it as an all-zero series (SER-R1-F1).
  - R-SERIES-2: over the shard's range, each entity's rows add up (received, sent, net, tx count)
    to its `btc-entity-balance` row for the same blocks and label set; an entity with no rows has
    an all-zero range row. Tested on the fixture and the live range (`tests/golden_series.rs`).
  - R-SERIES-3: a series shard is checked like any shard (rerun + root). A consumer comparing it
    with a range shard needs both roots from the same source snapshot and label set. Per-entity
    sums cannot see flow moved between two blocks of one entity; only the series root (a rerun,
    or a pinned root) catches that (SER-R1-F3).
- Pool recipe `btc-pool-blocks` (ADR-0032; recipe id in `recipes/out/btc_pool_blocks.wasm.keccak`;
  schema tag `onyx.btc.pool-blocks-row.v1`, metric `crypto.btc.pool_blocks` in
  `fixture_registry_pools`, served as `btc.pool-blocks`): `source` is a `CoinbaseSource`
  (`onyx-pool-schema`: per block height, hash, coinbase scriptSig, output scripts), `labels` must
  be empty, `params` is the Borsh `PoolTable`. One `PoolBlocksRow { pool, from_height, to_height,
  blocks }` per pool with at least one block, plus `unknown`, keyed by the name's bytes. Golden:
  `fixtures/golden/btc-pool-blocks-970019-970029.json`, recounted by
  `scripts/recount-pool-blocks.py`.
  - R-POOL-1 (reviewed allowlist): the table is built from `fixtures/pools/pool-allowlist.json`
    (a reviewed subset of the pinned upstream list; the upstream file is not committed because it
    names people). A pool is named only with a company website (not a forum post, bare IP,
    explorer page or github.com issue), not run under an operator's personal handle, not a
    person's name, with a non-empty tag or address; excluded ids are recorded by id and reason
    class only (`EXCLUDED_POOL_IDS`). Anything else counts as `unknown`.
  - R-POOL-2 (matching, deterministic): per block, (a) the first pool in table order owning the
    script of the coinbase's largest-value output (first on ties; zero-value outputs never count;
    the list's own addresses, mainnet only),
    else (b) the pool whose tag occurs earliest in the scriptSig (byte substring; ties at one
    offset: the longer tag, then table order; POOL-R2-F3), else (c) `unknown`.
  - R-POOL-3 (no free text): rows carry only table names and `unknown`; never coinbase bytes. The
    recipe refuses a table with a name that is empty, over 64 bytes, not printable ASCII,
    duplicated or `unknown`, or a pool with an empty tag or script (`BAD_PARAMS`).
  - R-POOL-4 (source): a `CoinbaseSource` is canonical (schema version 1, chain `btc.mainnet`,
    non-empty, contiguous heights). The adapter builds it only from blocks whose proof of work
    meets the mainnet floor, whose parents link, whose coinbase gives the BIP-34 height and is
    included under the header's Merkle root; an optional pinned tip must be the last hash.
    Committed proofs (header + coinbase + branch) make the golden run offline.
- Chain statistics recipe `btc-chain-stats` (ADR-0043; recipe id in
  `recipes/out/btc_chain_stats.wasm.keccak`; schema tag `onyx.btc.chain-stats-row.v1`, metric
  `crypto.btc.chain_stats` in `fixture_registry_chain`): `source` is a `ChainSource`
  (`onyx-chain-schema`: the parent header, then per block height, hash, header and per
  transaction base and total size, spent prevouts with creation height, outputs), `labels` and
  `params` must be empty. One `ChainStatsRow` per block keyed by the height (8 bytes big endian).
  Golden: `fixtures/golden/btc-chain-stats-synthetic.json` (a synthetic two-block source) and the
  website window, recounted by `scripts/recount-chain-stats.py` from the node's own JSON.
  - R-CHAIN-1 (checks, failure codes): `BAD_SOURCE` (1) not canonical (schema version 1, chain
    `btc.mainnet`, first height above 0, contiguous); `BAD_LABELS` (2); `BAD_HEADER` (3) a hash
    that is not the double SHA-256 of its header, or a header whose previous-block field is not
    the block before (the parent header for the first); `BAD_TX` (4) no transactions, a coinbase
    that is not first or spends something, a non-coinbase spending nothing, a base size of 0 or a
    total size below it, a negative fee, an input created above the block's height, a coinbase
    over subsidy plus fees; `BAD_PARAMS` (5); `OVERFLOW` (7) a sum past 21 M BTC or the integer.
    The header checks are consistency checks: nothing in the source ties the transactions to the
    header, so the binding is the content hash of a source a validator rebuilds from its own node
    (P3A-R1-3).
  - R-CHAIN-2 (definitions, integers only): `txs` counts the coinbase; `inputs` are non-coinbase
    inputs; `outputs` all outputs; `size_bytes = 80 + CompactSize(txs) + sum(total_size)`;
    `weight_wu = 4 x (80 + CompactSize(txs)) + sum(3 x base_size + total_size)`; `subsidy_sats`
    from the height (halving every 210,000); `fees_sats = sum(inputs - outputs)` over
    non-coinbase transactions; `coinbase_sats` its outputs; `issued_sats = coinbase - fees`
    (signed); `transfer_sats` non-coinbase outputs; fee rate per transaction
    `fee x 1000 / ceil(weight / 4)` (milli-sat per vB, rounded down), `fee_rate_median_msat_vb`
    the lower median (index `(n - 1) / 2` of the sorted per-transaction rates, 0 with none;
    unweighted: each non-coinbase transaction counts once, whatever its size) and
    `fee_rate_avg_msat_vb = fees x 1000 / sum(vsize)`; `segwit_txs` non-coinbase transactions with
    `total_size > base_size`; `taproot_inputs`/`taproot_outputs` scripts `0x51 0x20 <32 bytes>`;
    `active_scripts` distinct scripts among spent prevouts and outputs, without those that start
    with `OP_RETURN` or exceed 10,000 bytes; `coin_blocks_destroyed = sum(value x (height -
    created_height))`; `interval_secs` the header time minus the parent's. Each input's value,
    script and `created_height` come from the source (the node's prevouts): the recipe checks only
    `created_height <= height`, so in a one-block shard they bind through the content hash of a
    source a validator rebuilds from its own node (CHAIN-2), not through the recipe.
  - R-CHAIN-5 (the website's figures, `apps/web/verify/src/chain.rs`, integers, rounded down):
    per UTC day and for the window, `fullness_ppm = sum(weight_wu) x 10^6 / (blocks x 4,000,000)`
    (the mean share of the consensus weight limit), `segwit_ppm = sum(segwit_txs) x 10^6 /
    (sum(txs) - blocks)` (non-coinbase transactions), `taproot_inputs_ppm` over `inputs`,
    `taproot_outputs_ppm` over `outputs`, `mean_interval_secs = sum(interval_secs) / blocks`
    (truncated toward zero), `mean_size_bytes = sum(size_bytes) / blocks`; sums of the other fields
    as they are. The page draws these from the checked rows; it reads no figure from the file.
  - R-CHAIN-3 (sharding): the website runs one shard per block, so a row never depends on the
    range it was computed in (any range gives the same rows for its blocks); a window is pinned by
    `chain::chain_stats_commitment` over every shard's range and root.
  - R-CHAIN-4 (no free text): the adapter carries an output script that starts with `OP_RETURN`
    as that byte alone and never carries coinbase `scriptSig`s or witnesses; the rows are the same.
- Reserve recipe `btc-entity-reserve` (ADR-0034; recipe id in
  `recipes/out/btc_entity_reserve.wasm.keccak`; schema tag `onyx.btc.entity-reserve-row.v1`,
  metric `crypto.btc.entity_reserve` in `fixture_registry_reserve`): the same source and labels as
  `btc-entity-balance`; `params` is the Borsh `ReserveParams { base_height, base_ref, base }`
  (`onyx-reserve-schema`). One `EntityReserveRow { entity, base_height, height, base_ref, base_sats,
  received_sats, sent_sats, tx_count, balance_sats }` per labelled entity, keyed by the entity's
  bytes. Golden: `fixtures/golden/btc-entity-reserve-921080-921082.labels-v2.json` (a fixture
  base of 1,000 BTC per entity), recounted by `scripts/recount-entity-reserve.py shard`.
  - R-RES-BASE-1: the source starts at `base_height + 1`, else `STALE_BASE` (code 9).
  - R-RCP-BASE-1: the base names exactly the labelled entities, strictly ascending, else `BAD_PARAMS`.
  - R-RES-NEG-1: per transaction, spends are applied before outputs; a balance below zero at any
    transaction is `NEGATIVE_BALANCE` (code 6): a base too low for the chain is refused even when
    later inflows would hide it by the end of the range. The check is per entity (the recipe holds
    no per-address state): one address short and another of the same entity long by the same
    amount pass.
  - R-RES-NEG-2: the base derivation is checked per address at the base height: each scanned
    address's balance minus its own net flow must not be negative (`reserve::derive_base_by_address`,
    Python `recount-entity-reserve.py derive`; at every height too, R-RES-NEG-3).
  - R-RES-CAP-1: the base, and the end balances, sum to at most 21 million BTC
    (2,100,000,000,000,000 sats), else `ABOVE_SUPPLY` (code 10).
  - R-RES-2: with the same base, the rows equal `btc-entity-balance`'s (`end_balance_sats`,
    received, sent, tx count) for the same range and label set (`tests/golden_reserve.rs`; the
    week export checks each shard's flows against the history's balance leaves).
  - R-RES-CHAIN-1: a reserve history is a chain: shard k starts after shard k-1 on its end
    balances, and its `base_ref` is shard k-1's Merkle root; the first shard's `base_ref` is the
    base snapshot's hash. The recipe cannot see other shards; `onyx_source_btc::reserve::check_chain`
    checks the links (exporter, website, tests).
  - R-RES-PARAMS-1: a rerun cannot see a forged base (miner and validators agree on any base), so
    each shard spec's `params` must equal the committed start (first shard) or the previous
    finalized shard's end rows, height and root (`reserve::check_params_chain`; the exporter runs
    it). Serving `btc.entity-reserve` runs it from a start derived from the committed scan and the
    finalized rows, never from the spec under test (R-RES-SERVE-1..4, `reserve::serving_check`,
    docs/spec/delivery.md "Serving checks"; ADR-0034 Amendment 1).
  - R-RES-SCAN-1: the base comes from a committed UTXO scan (`ScanSnapshot`, per address, of every
    label in the set, at the node's tip). The base at an earlier height is the scan minus the
    flows in between, and the chain from it must land on the scan's entity totals exactly (an
    identity: it checks the arithmetic, not the scan). Two scans reconcile through the flows
    between them, per address (`reserve::reconcile`); the committed second scan is the same node
    3 blocks later, so it does not catch a node that omits an address in both. An entity's
    reserve is served only when its base is cross-checked independently (a committed check
    against a published source, or an operator-configured second-node re-scan that reconciles);
    others are refused (R-RES-XCHK-1, THREATS RES-1).
  - R-RES-NEG-3: walking back from the scan block by block, every address's balance after every
    block must not be negative, and every flow must be at a label-set address
    (`reserve::check_running_balances`, Python `derive`; THREATS RES-9).
  - R-RES-DEPTH-1 (ADR-0037, THREATS RES-4): a scan, a flows file and a shard are used only when
    their last block has `reserve::MIN_CONFIRMATIONS` (6, counted with the block itself) in a
    header chain checked for proof of work, the mainnet retarget rule (bits unchanged inside a
    2,016-block period, at most 4x at its first block: `node::permitted_bits_transition`, RES-20)
    and parent links (`reserve::HeaderChain`); every
    shard's last block (its source snapshot's tip hash) must be the chain's block at that height,
    before the landing too, and a flows file must be on the chain block by block. A second-node
    re-scan counts as a cross-check only with 6 confirmations.
  - R-RES-WINDOW-1 (ADR-0037, THREATS RES-2): with a dated label set, a shard over `f..=t` is bound
    to `window(f, t)` (its `LabelSetRef`); a shard across a membership change is refused.
  - R-RES-CHANGE-1 (ADR-0037): at a membership change after block `h`, the next shard's base is
    the previous end minus the balances after `h` of the addresses that leave each entity, plus
    those that join (an address moving between entities does both; an entity whose last address
    leaves must end at zero). Those balances are the scan rolled back with a flows file on the
    header chain; wherever the file holds a whole shard, its sum over that shard's window must
    equal the finalized rows. The start base is the scan's totals over the members at the scan's
    height, minus the finalized net flows, minus what every change moved.
  - Level: a reserve shard is level 4 overall. Its base is an input the rerun does not check
    (`input_level`, `RESERVE_BASE_LEVEL`), so the registry floors the recipe at 4
    (`METRIC_MIN_LEVELS`, `fixtures/registry/metric-min-levels.json`), whatever the label set's
    level (v2 is 4 itself).
- Ethereum chain statistics recipe `eth-chain-stats` (ADR-0044; recipe id in
  `recipes/out/eth_chain_stats.wasm.keccak`; schema tag `onyx.eth.chain-stats-row.v1`, metric
  `crypto.eth.chain_stats` in `onyx_source_eth::fixture_registry_eth`): `source` is an
  `EthChainSource` (`onyx-eth-schema`: the parent header's RLP, then per block its number, hash,
  header RLP, transaction envelopes and receipt envelopes, as the tries store them), `labels`
  must be empty, `params` is Borsh `EthStatsParams` (from, to, tip) as the snapshot pins them
  (`onyx_source_eth::params_for`). One `EthChainStatsRow` per block keyed by the number (8 bytes big
  endian). Golden: `fixtures/golden/eth-chain-stats-synthetic.json` (two synthetic blocks over
  every transaction type), the committed mainnet blocks `fixtures/eth/blocks-26225268-26225270`
  and the website window, recounted by `scripts/recount-eth-chain-stats.py` from the RPC's JSON.
  - R-ETH-1 (checks, failure codes): `BAD_SOURCE` (1) not canonical (schema version 1, chain
    `eth.mainnet`, first number above 0, contiguous, one receipt per transaction); `BAD_LABELS`
    (2); `BAD_HEADER` (3) a header that does not decode (London's 16 fields at least), whose
    Keccak-256 is not the block hash, whose parent hash is not the block before (the parent
    header for the first), whose number is not the block's and its parent's plus one, whose time
    is not after the parent's, whose gas used exceeds its limit, whose gas limit moved by 1/1024
    of the parent's or more or fell below 5,000, or whose base fee is not EIP-1559's from the
    parent (elasticity 2, change denominator 8), or whose excess blob gas is not EIP-7918's from
    the parent under BPO2 (target 14, maximum 21 blobs, blob base cost 2^13); `BAD_TX` (4) a transaction that does not decode
    (types 0-4, exact field counts, 20-byte recipients, 32-byte hashes, canonical integers), whose
    gas price or fee cap is below the base fee, or whose fee cap is below its priority cap;
    `BAD_PARAMS` (5) params that do not decode or name another first or last number or last
    hash than the source's; `BAD_ROOT` (6) the transactions or receipts do not rebuild the header's
    trie root (key `RLP(index)`, value the envelope); `OVERFLOW` (7); `BAD_RECEIPT` (8) a receipt
    that does not decode (status 0 or 1, 256-byte bloom, logs of 20-byte addresses and 32-byte
    topics), whose type is not its transaction's, whose cumulative gas does not rise, whose gas
    exceeds its transaction's gas limit, a last cumulative gas that is not the header's gas used,
    or blob hashes x 131,072 that are not the header's blob gas used, or more than 21 blobs;
    `UNSUPPORTED` (9) a block timed before 2026-02-01 00:00 UTC or from 2027-01-01 00:00 UTC on,
    or without the blob fields. Not checked by the recipe: that
    the chain is Ethereum's finalized chain (no proof of work; the fetch's finality and
    second-operator checks, ETH-2) and that each receipt is the result of executing its
    transaction (only a full node re-executes, ETH-3).
  - R-ETH-2 (definitions, integers only, wei as `u128`): `txs` all transactions; `failed_txs`
    receipts with status 0; `contract_creations` successful (status 1) transactions with an
    empty `to` (top level only; a reverted creation makes no contract); `gas_used`, `gas_limit` the header's; `fullness_ppm = gas_used x 10^6 / gas_limit`
    rounded down; `base_fee_wei` the header's; `burned_wei = base_fee x gas_used`; per
    transaction `gas = cumulative - previous cumulative`, `tip = gas_price - base_fee` (types 0,
    1) or `min(max_priority, max_fee - base_fee)` (types 2-4), `priority_fees_wei = sum(tip x
    gas)`; `blob_txs` type-3 transactions; `blob_gas_used`, `excess_blob_gas` the header's;
    `blob_base_fee_wei = fake_exponential(1, excess_blob_gas, 11,684,671)` (EIP-4844 with BPO2's
    update fraction); `blob_fee_wei = blob_base_fee x blob_gas_used`; `value_wei` the sum of the
    `value` of successful transactions (a reverted one sends nothing, P4A-R2-1); `interval_secs` the header time minus the parent's.
  - R-ETH-3 (sharding): one shard per block, so a row never depends on the range it was computed
    in; a window is pinned by `eth_chain_stats_commitment` over every shard's range and root.
  - R-ETH-4 (adapter): the RPC's JSON is re-encoded and checked against the block hash, every
    transaction hash and both trie roots before the source is built; the JSON's `gasUsed`,
    `effectiveGasPrice`, `blobGasPrice`, `blobGasUsed` and `contractAddress` must equal what the
    hashed bytes give; an unknown header field or transaction type is refused.
- Host (`crates/onyx-recipe-host`): `Host::run(id, wasm, input, profile) -> RunResult | HostError`.
  Canonical engine: wasmi 2.0.0 (ADR-0014); `fast::FastHost` (wasmtime, optimizer off) is advisory.
- Registry (M6): onchain `registerRecipe(recipeId, uri)`; the WASM is fetched from the URI and hash-checked.

## Determinism profile
1. No float types or opcodes (rejected at load).
2. No imports: no clock, randomness, network, filesystem, host calls.
3. Fuel metering; out-of-fuel is a deterministic failure.
4. Fixed memory limit.
5. Output must be canonical (strictly ascending by primary key, no duplicate keys, normalized);
   the host re-decodes and re-encodes to check (R-DUPKEY-1).
6. Call depth: `max_call_depth` = the number of nested wasm calls allowed below the entry export
   (`onyx_run`). Recursion `n` levels below the entry uses `n + 1` nested calls. The canonical engine
   (wasmi) enforces it with its heap call stack; the fast path with an instrumented counter. Both
   are tested at the boundary (`host::depth_boundary_identical_on_both_engines`).
6b. Machine failures (out of system memory, engine panic) abstain: no verdict, never a failure leaf.
7. A trap, out-of-fuel or depth overflow yields a canonical failure output, so failure is also
   byte-identical across hosts.
8. The runtime profile (ABI, host build, limits) is pinned in each commitment.

## Shards
A recipe run covers one shard: a source snapshot range (for BTC, a block range) times a subject
set. Shard ids: `keccak256(recipe_id || label_set_hash || borsh(shard_params))` (ADR-0011).

Fuel headroom (R-DET-4, SER-1, SER-R1-F2): `Round::open_preflighted` is the only public way to
open a round (SER-R2-F1). It runs every shard once on the canonical engine and refuses one whose
run uses more than half its fuel limit (`recompute::PREFLIGHT_MAX_FUEL_PPM`) or runs out of fuel.

Valid failures (R-FAIL-1, SER-R2-F2): a schema lists the recipe failure codes that are valid
results (`FailureAllowList`, supplied by the adapter: for BTC `valid_failure_codes()`, the series
`NO_ACTIVITY` only). The preflight refuses a shard whose canonical run ends with any other
failure (another recipe code, a trap, bad output, output too large), and a miner commits a
failure leaf only for a listed code (`Miner::with_allowed_failures`); otherwise it abstains. The
on-chain `openRound` trusts the coordinator to have run the preflight (TODO, SER-R2-F1).

Sizing BTC shards: cost grows with label decoding and with inputs plus outputs, not with block
count. Measured on wasmi for `btc-entity-flow-series` and `btc-entity-balance` with label set v2
(9,850 labels): about 0.56 B fuel to decode the labels plus about 3.8 k fuel per input or output
(`tests/series_props.rs`, `tests/golden_series.rs`). To stay at or under 2 B (half of the 4 B
limit), a shard holds at most about 380 k inputs plus outputs. On the live range that is 12
blocks (1.39 B fuel measured); the 24-block range the website shows (2.39 B) is a display rerun,
not a round shard, and the preflight refuses it as one shard. Size by counting inputs and
outputs, then confirm with the preflight.

## Assumptions
- Ethereum chain statistics (ADR-0044): the finalized chain an endpoint reports, confirmed by a
  second operator's block hashes, is Ethereum's; receipts are the result of executing their
  transactions (a validator reruns from its own node; THREATS ETH-2, ETH-3).
- Reserve bases (ADR-0034): a node's `scantxoutset` reports the UTXO set at its tip; the scan is
  as good as the node until a second scan, on any node, reconciles with it. The snapshot pins
  the scan's block hash, which must be on the header chain the shards read, with 6
  confirmations (ADR-0037).
- A reorg deeper than 5 blocks on top of a used block is not expected (Bitcoin whitepaper
  section 11: under 0.1% for an attacker with 10% of the hash power); one deeper than that makes
  the served chain wrong until the operator re-anchors it (THREATS RES-4).
- Recipes are small enough to rerun whole within a challenge window (no bisection needed in v1).
- Chain statistics (ADR-0043): the node's prevouts for outputs created before a shard (value,
  script, creation height) are the chain's; a validator reruns from its own node, and a lying
  node shows as a content-hash mismatch (as for every Bitcoin source, THREATS L16, CHAIN-2).
- Reproducible builds are a convenience, not a trust root: the id is the WASM hash, and anyone can
  diff the WASM against source.

## Open questions
- Long histories need many short shards (see Shards); decoding labels once per range set would
  cut the fixed cost (THREATS SER-1).
- Should the registry require a reproducible-build attestation (source commit -> same WASM)?
- Max fuel per recipe class, and who sets it.
- Do we allow non-Rust recipe languages (AssemblyScript, TinyGo) if they emit no floats?
- Daily distinct counts (active addresses per UTC day, ADR-0043 item 10): one day-sized shard
  (memory) or a two-stage recipe whose first stage commits sorted script hashes per shard?
