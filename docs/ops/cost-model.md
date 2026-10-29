# Cost model

## Scope

Cost per query and per validation, as formulas. Numbers get filled in only from measurements in this repo or cited sources. No guesses.

## Current state (M0)

No measurements yet. One cited external price:

- x402 via the Coinbase CDP facilitator: first 1,000 settlements per month free, then $0.001 per settlement; verification free (source: https://docs.cdp.coinbase.com/x402/network-support, read 2026-10-06; see [../../RESEARCH.md](../../RESEARCH.md)).

## Plan

Per paid query:

```
cost_query = c_compute_api + c_proof_build + c_storage_read + c_facilitator
c_facilitator = 0 for the first 1,000 settlements per month, else $0.001 (CDP, above)
```

Per validated shard:

```
cost_validation = s * (c_recompute(recipe, fuel) + c_fetch_snapshot) + k * c_canary
                  + c_commit_gas + c_reveal_gas
s = rows sampled, k = canaries mixed in
```

Per finalized round:

```
cost_round = c_finalize_gas + sum(cost_validation over shards) + c_storage_write
```

Measurement sources:

- Fuel per recipe run: wasmtime fuel counter (M1).
- Gas: `forge test --gas-report` (M6), Base Sepolia receipts.
- API cost: load test on the build machine 7920 (M7).

## Open questions

- Pricing floor: price per call must exceed `cost_query`; set in M7 from measured values.
- Who pays validation cost: query revenue, miner fees, or both?
