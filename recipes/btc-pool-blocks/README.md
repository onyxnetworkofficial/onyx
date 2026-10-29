# btc-pool-blocks

Input: a pinned block range's coinbases (`CoinbaseSource` from `adapters/source-btc`, built from
raw blocks whose header, height and coinbase Merkle inclusion were checked); no label set; `params`
= the Borsh `PoolTable` (the reviewed pool table derived from the MIT-licensed
`bitcoin-data/mining-pools` list, see `fixtures/pools/`). Output: one `PoolBlocksRow` per pool that
mined at least one block of the range, plus `unknown`, keyed by the pool name's UTF-8 bytes.
Coinbase text is never output: only table names and `unknown`. Spec: docs/spec/recipes.md
(R-POOL-1..4), ADR-0032.
