# Sector: prediction-market settlement (stub)

## Scope

Onyx as a resolution source: a market contract or operator reads a finalized
Onyx value to settle.

## Claim model fit

- Any lane can feed a market, but settlement should use objective-lane claims
  (`Recompute`, `OfficialSourceMatch`, `CrossSourceQuorum`).
- Outcomes are `Value::Bool` (did X happen) or numeric values compared to a threshold by the market.
- The settlement endpoint (M7) returns: the row, its Merkle proof, and the finalized
  root's round and shard. The consumer checks the proof against the root on Base
  with `OnyxMerkle.verify` (`contracts/src/OnyxMerkle.sol`) and reads the recipe id and label set hash from the `Finalized` event.
- A value is settlement-grade only after its challenge window closes.

## Rights issues

Same as the source sector. Settlement data must be redistributable.

## Open questions

- Does a market need an onchain pull (contract reads a stored value) or is proof-on-submit enough?
- Subjective-lane values (label-dependent) for markets: publish with the label-set spread, or forbid?
