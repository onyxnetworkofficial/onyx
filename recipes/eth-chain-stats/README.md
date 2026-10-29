# eth-chain-stats

Per-block Ethereum statistics from the blocks alone (ADR-0044, ROADMAP P4a): transactions,
failed transactions, contract creations, gas used and limit (fullness in ppm), base fee, ETH
burned, priority fees, blob gas, blob base fee and blob fees, value sent (creations and value
count successful transactions only). Integers only, amounts in wei.

The recipe checks what it reads: each header hashes to its block hash and names the block
before it, numbers and times ascend, the base fee is the EIP-1559 one and the gas limit moves
within its bound, the transactions and receipts hash to the header's trie roots, every
transaction pays at least the base fee, receipts match their transactions and add up to the
header's gas, blobs add up to the header's blob gas. What it cannot check (which chain is
Ethereum's, and the execution itself) is in docs/spec/recipes.md R-ETH-1.

Source: `onyx_eth_schema::EthChainSource`; no label set (level 1); params `EthStatsParams` pin the range and tip. Definitions:
docs/spec/recipes.md R-ETH-1..4. The website runs one shard per block.
