# Onyx on Base Sepolia: run summary (template)

Fill from `.onyx-testnet/runs/run-<time>.log` and the driver output. Public data only: addresses
and transaction hashes; never a key.

| Item | Value |
|---|---|
| Date (UTC) | |
| Commit | |
| Settlement / Disputes / Revenue | `0x…` / `0x…` / `0x…` |
| MockUSDC (bonds) / USDC (x402) | `0x…` / `0x036CbD53842c5426634e7929541eC2318f3dCF7e` |
| Data | live blocks 970,543-970,566 or fixture |
| Round 1 shard / root | `0x…` / `0x…` |
| Open / release / settle tx | [BaseScan](https://sepolia.basescan.org/tx/0x…) ×3 |
| Root finalized (L2 finality wait) | … s |
| Paid request | 2,500 micro-USDC → 200, booked to the shard in the settle tx |
| Dispute round | panel of …, true root final, cheater bond taken |
| Spend | … txs, … L2 gas, … ETH (+ … ETH L1 data) |
| Checks | tag, seed, payouts, verifyRow, revenue booking: all passed |
