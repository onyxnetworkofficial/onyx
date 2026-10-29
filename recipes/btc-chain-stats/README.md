# btc-chain-stats

Input: a pinned block range (`ChainSource` from `adapters/source-btc` `chain::load_chain_range`,
built from raw blocks and the node's prevouts with every block check of a balance source); no
label set; empty `params`. Output: one `ChainStatsRow` per block, keyed by the height (8 bytes
big endian): transactions, inputs, outputs, size, weight, interval, `nBits`, subsidy, fees,
coinbase, new supply, value moved, median and mean fee rate (milli-sat per vB), SegWit
transactions, Taproot inputs and outputs, distinct active scripts, coin-blocks destroyed.
Integers only. Spec: docs/spec/recipes.md (R-CHAIN-1..4), ADR-0043.
