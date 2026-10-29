# ADR-0043: Bitcoin chain statistics from blocks alone (ROADMAP P3)

## Status
Accepted, 2026-10-18 (the loop decides; ROADMAP P3, the project owner 2026-10-18: "there's no on-chain data
anywhere on the website on the data section"). Implemented by `recipes/btc-chain-stats`,
`adapters/chain-schema` (`onyx-chain-schema`), `onyx_source_btc::chain`,
`examples/export-chain-stats.rs`, `scripts/recount-chain-stats.py`, `scripts/refresh-onchain.py`,
the browser checks in `apps/web/verify/src/chain.rs` and the Data page's "Bitcoin on chain"
section. Threats CHAIN-1..CHAIN-10.

## Context
The Data page showed one 24-block demo range and label-derived company flows. A vendor's
on-chain catalogue (480 metrics) starts with block, fee, supply and activity series that need no
label and no price: they are functions of the blocks. Onyx can serve those at level 1 (anyone with
the blocks reruns them) and in every build, since public chain data carries no label rights
(ADR-0035 holds back only label-derived totals). The data on hand (P3 brief): the fetched block
folders `fixtures-live/week` (969,839-970,846) and `fixtures-live/tail` (970,847-971,199), each
block as its raw bytes plus the node's verbosity-3 JSON with prevouts; no Bitcoin Core REST
endpoint listens on this machine. No licensed price source exists in the repository.

## Decision
1. **One recipe, one row per block.** `btc-chain-stats` (metric `crypto.btc.chain_stats`, schema
   tag `onyx.btc.chain-stats-row.v1`, no label set, empty params): per block a `ChainStatsRow`
   keyed by the height (8 bytes big endian) with height, hash, header time, interval to the
   parent, `nBits`, transactions, non-coinbase inputs, outputs, block size and weight (from the
   transactions' base and total sizes: `80 + CompactSize(n) + sum(total)` and
   `4 x (80 + CompactSize(n)) + sum(3 x base + total)`), the subsidy the schedule allows, fees
   (inputs minus outputs), coinbase outputs, new supply (coinbase minus fees, signed), value moved
   (non-coinbase outputs, change included), the lower median and the mean fee rate, SegWit
   transactions (total size above base size), Taproot inputs and outputs (`OP_1 <32 bytes>`),
   distinct spendable scripts spent from or paid to (Bitcoin Core's `IsUnspendable` excluded),
   and coin-blocks destroyed (sum of value x blocks since creation). Integers only (ADR-0009):
   satoshis, fee rates in milli-satoshis per virtual byte rounded down, vsize = weight / 4
   rounded up. Definitions: docs/spec/recipes.md R-CHAIN-1..4.
2. **The recipe checks what it can itself.** Each block's hash is the double SHA-256 of its
   header, each header names the block before it (the parent header for the first), the coinbase
   and only it is first and spends nothing, no fee is negative, no input was created after its
   block, the coinbase claims at most subsidy plus fees, sums stay within 21 M BTC. A failure is a
   recipe error, never a row.
3. **Source.** A new canonical `ChainSource` in its own crate (the existing recipes' bytes and ids
   do not change): the parent header, then per block its height, hash, header and per
   transaction its base and total sizes, spent prevouts (value, script, creation height) and
   outputs. `onyx_source_btc::chain::load_chain_range` builds it from the raw block and the node's
   JSON with every check `node.rs` makes for a balance source, plus the parent link and the
   creation height of in-range prevouts. Prevouts created before the range come from the node, as
   for every Bitcoin source (L16): a validator uses its own node.
4. **No free text in the source (CHAIN-4).** An output script starting with `OP_RETURN` is carried
   as that byte alone; coinbase `scriptSig`s and witnesses are never carried. The rows are the
   same (the recipe reads only that such an output is unspendable), and a block input the website
   ships holds no message written into the chain.
5. **One shard per block.** Each block is its own shard, so a block's row is final and the
   browser can rerun any one block's input (hundreds of KB) without the window. Measured fuel per
   block on the window is at most 81,900,478, 2.05% of the 4 B limit (docs/ops/benchmarks.md); the preflight still
   confirms every shard. The site pins
   `keccak("onyx-chain-stats-v1" ‖ recipe id ‖ tip ‖ per block: from ‖ to ‖ root)`
   (`chain::chain_stats_commitment`).
6. **The window.** All blocks of the fetched folders: 969,840-971,199 (1,360 blocks, 4-13 Oct
   2026; 969,839 is the first block's parent). The page says so and shows nothing outside it.
7. **Website.** `static/onchain/btc-chain-stats.json` holds one leaf per block grouped by UTC day,
   the range, tip and commitment; `static/onchain/rerun-input.b64.txt` holds the full recipe
   input of the window's smallest block with at least 1,000 transactions (970,835: 1,144
   transactions, 190,062 source bytes; an empty block would prove little), written by
   `export-chain-stats rerun`; `scripts/build-web.sh` copies the recipe to
   `data/chain-stats.wasm`. The verifier (Rust) checks the leaves against the pins and computes
   every figure the page draws (per-day sums, shares in ppm, means) from the checked rows; the
   page reruns the shipped block on the canonical engine on request. Shipped in all builds,
   public and fallback included, under every existing guard (AUD-2-F1, L8-F1, L9A-8, L14).
8. **Refresh.** `scripts/refresh-onchain.py` fetches new blocks from a Bitcoin Core REST endpoint
   (`/rest/chaininfo.json`, `/rest/headers`, `/rest/block/<hash>.bin`, and
   `/rest/block/<hash>.json` for the prevouts, which `.bin` lacks; loopback only by default), or
   takes block folders, runs the exporter over the last 2,016 blocks and with `--write-pins`
   rewrites the verifier's `CHAIN_*` pins.
9. **Independent recount.** `scripts/recount-chain-stats.py` recomputes every field of every block
   from the node's own JSON (its `size`, `weight`, per-transaction `fee`, `vsize`, `weight`,
   prevout `height` and script `type`), never the raw bytes; the exporter fails on any difference.
10. **Not in this recipe (planned, the open ROADMAP P3):** daily active addresses (a distinct count over a
    UTC day needs one shard holding the day: about 105 MB of source per day at today's density,
    past the 256 MiB memory limit once decoded, or a two-stage distinct count); realized cap, MVRV
    and SOPR (a price per block from a licensed source, none in the repository); exchange and ETF
    netflows (P2a, P2c). Their inputs are named on the page.

## Consequences
- The Data page shows real Bitcoin data computed by an Onyx recipe in every build, each chart
  naming the recipe, its blocks and the rerun command; a reader's browser reruns one block.
- A full history needs every block from genesis with its prevouts: an unpruned node (verbosity-3
  JSON reads the undo data; `-txindex` is not needed). Export plus recount took about 0.75 s per
  block of today's density on this machine under load; early blocks are far smaller, so 971,200
  blocks are at most about 8.4 days of one core, and shards run in parallel (P5 measures it).
- Per-block shards make 1,360 Merkle roots per window; a month is about 4,400. The commitment is
  a flat list, so the pin grows with the window, not the page's checks.
- `ChainStatsRow` is fixed by schema tag v1; a new field is a new schema and a new recipe id.
