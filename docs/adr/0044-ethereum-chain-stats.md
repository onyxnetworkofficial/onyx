# ADR-0044: Ethereum adapter and chain statistics from hashed blocks (ROADMAP P4a)

## Status
Accepted, 2026-10-19 (the loop decides; ROADMAP P4a, the project owner 2026-10-18: "Ethereum next to
Bitcoin"). Implemented by `adapters/eth-schema` (`onyx-eth-schema`), `adapters/source-eth`
(`onyx-source-eth`), `recipes/eth-chain-stats`, `examples/export-eth-chain-stats.rs`,
`scripts/refresh-eth-onchain.py`, `scripts/recount-eth-chain-stats.py`, the browser checks in
`apps/web/verify/src/eth_chain.rs` and the Data page's "Ethereum on chain" section. Threats
ETH-1..ETH-12. Absorbs ROADMAP X1's adapter half. Built beside ADR-0043 (Bitcoin), sharing no code
with it, so either merges without the other.

## Context
- Ethereum has no proof of work since the merge (September 2022): a block's validity rests on the
  beacon chain's attestations, which an execution-layer JSON-RPC does not return. A client that
  reads blocks over JSON-RPC can still check everything the execution header commits to: the
  block hash is Keccak-256 of the header's RLP, the header names its parent's hash, and
  `transactionsRoot` and `receiptsRoot` are Merkle-Patricia trie roots over the transactions and
  receipts (Yellow Paper appendices B-D; EIP-2718 typed envelopes).
- No Ethereum node listens on this machine (`ss -ltn`, `eth_chainId` probes, 2026-10-19), and no
  SSH or tunnel was allowed. Keyless public endpoints answer read calls (publicnode, Flashbots
  Protect, dRPC, 1RPC, MEV Blocker, Tenderly's public gateway); none returns raw RLP
  (`debug_getRawBlock` and `debug_getRawReceipts` are not offered on publicnode).
- A vendor's Ethereum on-chain catalogue starts with block, gas and fee series that need no
  label and no price: functions of the blocks. Onyx serves those at level 1 in every build
  (ADR-0035 holds back only label-derived totals).

## Decision
1. **Rebuild the chain's bytes from the JSON and check them against the chain's hashes.** The
   adapter re-encodes each header (the London-to-Prague field list; an unknown header field is
   refused, never dropped), each transaction (types 0-4) and each receipt in the chain's own
   encoding and requires: Keccak-256 of the header = the block hash; Keccak-256 of each envelope =
   the transaction hash; the ordered trie roots = the header's `transactionsRoot` and
   `receiptsRoot`; each block's parent hash = the block before (the window's parent for the
   first). Then the JSON's derived fields (`gasUsed`, `effectiveGasPrice`, `blobGasPrice`,
   `blobGasUsed`, `contractAddress`) must equal what the hashed bytes give. The RPC's word is
   taken for nothing the statistics use. RLP and the trie are written in `onyx-eth-schema`
   (about 250 lines, no new crate): the only hash is Keccak-256 from `sha3`, already in the graph.
2. **The recipe checks the same things itself.** `eth-chain-stats` (metric
   `crypto.eth.chain_stats`, schema `onyx.eth.chain-stats-row.v1`, no label set, empty params)
   takes `EthChainSource` (the parent header's RLP, then per block its number, hash, header RLP,
   transaction envelopes and receipt envelopes) and, as params, `EthStatsParams` (from, to, tip:
   the snapshot's range and last block hash, `params_for`); it fails with a code unless the
   source is exactly those blocks (review P4A-R1-1: otherwise a self-consistent invented chain
   could be labelled as the pinned blocks), every header hashes
   to its block and links to its parent, numbers and times ascend, the base fee is EIP-1559's
   from the parent, the excess blob gas is EIP-7918's from the parent under BPO2 (target 14,
   maximum 21 blobs), the gas limit moves by less than 1/1024, gas used is within the limit, both
   trie roots rebuild, every transaction decodes and pays at least the base fee (fee cap at least
   the priority cap), receipt types match, cumulative gas rises and ends at the header's gas used,
   and blob hashes add up to the header's blob gas (at most 21 blobs). Given the pinned tip, the
   hash chain fixes every byte of the range. A validator rebuilding the source therefore
   cannot be fed rows for other bytes: a source that does not hash is a recipe error.
3. **Trusted, stated, not hidden.** Which chain is Ethereum's: the fetch stays at or below the
   endpoint's `finalized` block and a second operator must return the same hash for the window's
   parent, first, every 50th and last block (`scripts/refresh-eth-onchain.py`); a beacon light
   client (sync-committee signatures, BLS12-381) is the stronger check and is planned (ETH-2,
   ROADMAP P4a follow-up). Execution: a receipt's status and gas are the node's result of running
   the transaction; only a full node re-executes. Signatures are not recovered (no statistic
   needs the sender).
4. **Rows.** One `EthChainStatsRow` per block, key the number (8 bytes big endian): number, hash,
   time, interval, transactions, reverted transactions, successful top-level contract creations, gas used and
   limit, fullness (ppm, rounded down), base fee, burned (base fee x gas used), priority fees
   (sum of `(effective gas price - base fee) x gas used`), blob transactions, blob gas used,
   excess blob gas, blob base fee (EIP-4844 `fake_exponential` with BPO2's update fraction
   11,684,671), blob fees (blob gas x blob base fee), value sent (top-level `value` fields of successful transactions: a reverted one sends nothing,
   review P4A-R2-1). Wei
   as `u128`, integers only (ADR-0009). Definitions: docs/spec/recipes.md R-ETH-2.
5. **Fork rules are pinned, not guessed.** The recipe accepts blocks timed from 2026-02-01 (after
   BPO2) to the end of 2026 with the blob fields (review P4A-R1-2: blocks from 2027 need the fork
   schedule reviewed and a new recipe id); a later fork that changes the blob parameters, the base-fee rule,
   the header field list or the transaction types makes the adapter or the recipe refuse, and
   needs a new recipe version (a new id). The adapter compares the recipe's blob base fee with
   every blob receipt's `blobGasPrice`.
6. **One shard per block**, like ADR-0043: a row never depends on the range, the browser reruns
   one block. The site pins `keccak("onyx-eth-chain-stats-v1" ‖ recipe id ‖ tip ‖ per block:
   from ‖ to ‖ root)` (`eth_chain_stats_commitment`). Measured fuel per block on the window: mean
   48,647,979, max 96,632,769 (2.4% of the 4 B limit). The exporter refuses a window that no
   second operator on another host confirmed (`cross_checked`, review P4A-R1-3) unless
   `ONYX_ETH_UNCHECKED=1`, and the file then says so.
7. **Window and data.** Blocks 26,225,261-26,225,560 (300 blocks, 2026-10-19 07:54-08:54 UTC,
   finalized at fetch), first fetched from `ethereum-rpc.publicnode.com`, then re-fetched from Onyx's own
   reth node (identical export; THREATS ETH-10) and cross-checked on
   `rpc.flashbots.net` (DATA-LICENSE.md records both sites' terms). The raw answers are 52 MB
   gzip, too big for git: the repository holds every answer's SHA-256
   (`fixtures/eth/window-26225261-26225560.manifest.json`), three consecutive blocks in full
   (`fixtures/eth/blocks-26225268-26225270`, 355 KB, every transaction type 0-4, blobs, a
   contract creation, reverted transactions) and the fetch script; `golden_eth_chain` reruns the
   whole sample when the window is present (`ONYX_ETH_DIR`, default the gitignored
   `fixtures-live/eth`).
8. **Website.** `static/onchain/eth-chain-stats.json` (one leaf per block, grouped by UTC day, the
   range, tip, fetch record and commitment) and `static/onchain/eth-rerun-input.b64.txt` (the full
   input of the smallest block with at least 100 transactions and a blob transaction: 26,225,368,
   102 transactions, 72,446 source bytes); `scripts/build-web.sh` copies the recipe to
   `data/eth-chain-stats.wasm`. The verifier checks the leaves against its pins and computes every
   figure the page draws (window sums, 25-block bars, per-block lines); the page reruns the
   shipped block on request. Shipped in all builds. The shipped input carries the block's
   calldata and logs verbatim, since the trie roots cover every byte (ETH-6).
9. **Independent recount.** `scripts/recount-eth-chain-stats.py` recomputes every field of every
   block from the RPC's JSON fields (receipts' `effectiveGasPrice`, `gasUsed`, `status`,
   `blobGasPrice`; transactions' `type`, `to`, `value`), never the RLP; the exporter fails on any
   difference. On the window: all 19 fields of all 300 blocks match. For a block with no blob
   receipt the recount computes the blob base fee with the same BPO2 fraction (not independent
   there); the recipe's excess-blob-gas rule is what catches a changed schedule.

## Consequences
- The Data page shows real Ethereum data computed by an Onyx recipe in every build, each chart
  naming the recipe, its blocks and the rerun command.
- A validator needs only an execution-layer JSON-RPC (its own node, or any endpoint: the recipe
  refuses bytes that do not hash) plus an independent view of the finalized chain.
- Exchange and issuer balances on Ethereum (P4b) and USDT/USDC supply and flows (P4c) need token
  transfer logs, state reads at a block (`eth_getBalance`, `eth_call` with state proofs from
  `eth_getProof` against the header's `stateRoot`) and the issuers' address lists; this recipe
  reads none of them.
- `EthChainStatsRow` is fixed by schema tag v1; a new field is a new schema and recipe id.
