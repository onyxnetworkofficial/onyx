# Recipes

Each recipe is its own crate compiled to `wasm32-unknown-unknown`, kept out of the main workspace
build so host tests do not depend on wasm builds. Recipe id = Keccak-256 of the built `.wasm`.

- `btc-entity-balance/` (M1): per-entity BTC balance at a block height, ported from the
  the entity indexer logic, run against a pinned label set.
- `btc-entity-flow-series/`: per-entity flows for every block of the range (a rerunnable time
  series); its rows sum to the `btc-entity-balance` row for the same range (R-SERIES-2). A range
  with no labelled activity ends with `NO_ACTIVITY` (code 8), read as an all-zero series.
- `btc-pool-blocks/` (ADR-0032): blocks per mining pool over a range, matched by coinbase payout
  script, else coinbase tag, against a reviewed allowlist of pool companies; everything else is
  `unknown`. No label set: level 1.
- `eth-chain-stats/` (ADR-0044): per-block Ethereum statistics (transactions, gas, base fee, ETH
  burned, priority fees, blobs and blob fees, contract creations, value sent) from blocks whose
  header hashes, parent links, trie roots and base fee the recipe checks itself. No label set:
  level 1. The website runs one shard per block.
- `btc-entity-reserve/` (ADR-0034): per-entity balance at the end of a range, from a base
  (`ReserveParams`: base height, base reference, one balance per labelled entity) plus the
  range's flows. A reserve history is a chain of these shards; the first base comes from a
  committed UTXO scan (`fixtures/reserves/`). Refuses a base for another height and any balance
  that would go below zero.
- `btc-chain-stats/` (ADR-0043): per-block chain statistics from the blocks alone (transactions,
  fees and fee rates, size and weight, supply, SegWit and Taproot use, active scripts,
  coin-blocks destroyed). No label set, no params: level 1. The website runs one shard per block.
