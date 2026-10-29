# Spec: parametric insurance demo (draft, north-star loop NL18)

Status: **Built 2026-10-27** (lane C: `contracts/src/OnyxParametric.sol`, ADR-0042 amendment 3,
`scripts/insurance-sepolia.sh`; deployed on Base Sepolia, docs/ops/deployment.md). The demo
products use the two live level-2 metrics (Denver high, world M5+ count), since the PRCP and
regional templates (NL17) do not exist yet. Drafted first by lane B. Testnet only: tONYX (no
value), Base Sepolia and local forks. Prior art: RESEARCH.md "More datasets and parametric
insurance (NL17, NL18)". Builds on the futures pattern (ADR-0042 amendment 2, `OnyxObservedFacts`).

## Scope

Parametric insurance pays a fixed amount when a measured value crosses a line, with no claim and
no loss assessment ("pre-specified payouts based upon a trigger event"; the gap between payout and
real damage is basis risk). Onyx can settle such a policy from the same registry facts the futures
use: a level-2 value observers recorded and anyone can prove. The demo: one policy that pays and
one that does not, on a fork, then on Base Sepolia.

Out of scope: real money, pricing models, regulation (a real product is insurance law, not code).

## Interfaces

- **`OnyxParametric` (new test contract).**
  - `underwrite(amount)`: an underwriter deposits tONYX into the pool that backs payouts.
  - `buy(fact, comparator, threshold, payout, premium, buyBy, observableAt, resolveBy)`: the insured
    pays `premium`; the pool reserves `payout`. `comparator` is `>=` or `<=`; `fact` is listed in
    `OnyxObservedFacts` (the same listing rules as a market).
  - `settle(policy)`: anyone, after `observableAt`: reads `factValue(fact)`; level 2 or stronger and
    accepted at or after `observableAt` pays `payout` to the insured if the condition holds, else
    releases the reserve to the pool (the premium stays with the pool). No value by `resolveBy`:
    the premium goes back to the insured and the reserve is released.
- **Demo policies** (NL17 sources): "Denver precipitation on day D is at least X mm" (`PRCP`,
  `USW00003017`) and "at least one M4+ earthquake within 300 km of Los Angeles on day D" (a regional
  USGS count, value >= 1). X, D and amounts are set when the demo runs.
- **Tool.** `scripts/insurance-sepolia.sh deploy|buy|settle|status` on the markets driver's pattern
  (keys in process, Base Sepolia only, every bettor-like caller its own key file).

## Rules (carrying the NL6 review)

1. **R-PI-1 Bought before anyone knows.** `buy` closes (`buyBy`) before the source publishes the
   value (THREATS NL6-10: the source's earliest publication, not Onyx's observation rule).
2. **R-PI-2 Listed against `observableAt`.** The fact's `notBeforeBlock` is estimated at
   `observableAt`, so an early recording cannot make the policy unsettleable (NL6-11).
3. **R-PI-3 No vote.** Only a level-1 or level-2 value settles a policy; a stake vote or panel
   answer never does (a policy that cannot settle refunds the premium).
4. **R-PI-4 No insider policies.** The registry coordinator, the facts and contract owner, and the
   fact's observers may not buy or underwrite (NL6-12, NL6-4).
5. **R-PI-5 Solvent by construction.** `buy` reverts unless the free pool covers `payout`; the
   reserve is held until settlement.
6. **R-PI-6 Plain words.** The page and docs say: test token, no value, a demo of a trigger, basis
   risk (a payout can differ from any real loss).

## Assumptions

- `OnyxObservedFacts` lists facts of any dated level-2 metric, so no second facts contract is needed.
- The regional earthquake count (NL17 row 3) and the precipitation element (NL17 row 1) exist as
  templates with live rounds first.

## Open questions

1. One contract for futures and policies (a policy is a two-bucket market with a fixed payout), or
   a separate one? Separate is clearer to read; shared reuses audited code.
2. Who underwrites on testnet: the deployer alone, or test accounts as in the futures bets?
3. Should the payout scale with how far the value crosses the line (a step function, like CCRIF's
   payouts that rise with modelled loss up to a limit), or stay one fixed amount for the demo?
