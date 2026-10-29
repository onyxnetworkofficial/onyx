# ADR-0042: Staked submissions and data futures (testnet prototype): observation first, then a stake vote, then a fallback oracle; futures resolve only from re-observed or recomputed facts

## Status
Proposed, 2026-10-24 (branch `stake-markets`, ROADMAP S3, S4, S6; partner tracker T10). Built and
tested in a local EVM only: no deployment, no Rust client, no website page yet. Every economic
number is a placeholder until the partners agree the token economics (tracker T12).

## Context
On the 2026-10-20 call the project owner and a partner agreed to try staking as the trust mechanism (tracker
C2-C5): a submitter stakes that a value is right ("they pay like $10 ... if it's not accurate,
they lose the $10, if it is then they get the $10 back"); stake-weighted betting is the oracle
("let's try it, if it doesn't work out then we can always use an oracle"); validators still catch
honest mistakes; duplicates earn nothing; a rich attacker's cost should work like proof of stake.
They also named futures on values not yet known (tomorrow's close, the next print) as later work.

A partner (2026-10-22) asked what pays validators once fraud is rare, and warned about wash and Sybil
behaviour. `PRODUCT-PLAN.md` on branch `product-onboarding` recommends that a market never settles
from a stake majority. This ADR keeps both views: votes decide only what no observation can, and
a vote-decided value never settles a market.

Prior art (RESEARCH.md "Bonded claims and data markets"): UMA's optimistic oracle (a bonded
assertion, disputes escalate to a token vote), Polymarket's resolution rules and UMA adapter,
Augur's reporting and forks, Kleros juries, parimutuel betting.

## Decision
1. **Testnet only.** Contracts: `OnyxStakes`, `OnyxReobserve`, `OnyxFutures`, `OnyxPayout`
   (library), `OnyxTestnetOwnerOracle`, `IOnyxMarkets`. No real money moves.
2. **A test token with no value**, `OnyxTestToken` ("tONYX test token"), deploys and mints only on
   a local anvil (chain 31337) or Base Sepolia; any other chain reverts.
3. **Two kinds of claim.** `Observed`: past data that observers can re-read (the auto-pull route of
   ADR-0038). `Voted`: data nobody can re-read (own data, closed sources). `resolve` decides, in
   order: an `Observed` claim by its re-observation; otherwise the stake vote; otherwise the
   fallback oracle.
4. **Fact keys** are the registry's dated-fact keys (`OnyxRegistry.factKeyOf`), so a claim and a
   level-2 round name the same fact.
5. **Re-observation (S4).** `OnyxReobserve` is a minimal on-chain port of the level-2 round rules
   (ADR-0027) for exact values: every registered observer is assigned, commit-reveal with the
   observer's address in the commit, an explicit "unavailable" answer, and a strict majority of
   all assigned observers. A majority overrides any vote (`test_ObservationOverridesAWhaleVote`).
   Not ported (TODO S4b): random draws from a pool, observer bonds and slashes, numeric tolerance,
   sealed reveals.
6. **The vote.** Anyone except the submitter, the observers and the arbiter commits a hidden stake
   for "correct" or "wrong", then reveals it. The vote decides only with at least `quorum`
   revealed and one side holding at least `decisivePpm` of it (default 2/3). The submitter's bond
   never counts as a vote.
7. **Fallback oracle.** A contested or low-quorum claim goes to `IOnyxFallbackOracle`. On testnet
   this is one owner key (`OnyxTestnetOwnerOracle`); no answer within `fallbackWindow` voids the
   claim and refunds everyone. Production must use the drawn panel of ADR-0021/0024 (TODO S3b).
8. **Payouts.** Losers' stakes (and a wrong submitter's bond) go to the winners pro rata, less
   `feePpm` to the treasury. A right submitter gets its bond back plus `submitReward` from a funded
   reward pool. Unrevealed stakes are forfeited to the treasury. Rounding dust goes to the
   treasury after the last winner withdraws. A void refunds every stake and the bond.
9. **Attack cost.** To win a vote alone against `H` honest revealed stake, an attacker needs
   `A >= H × d / (1 - d)` (with `d = decisivePpm`: `A >= 2H` at 2/3), at least `quorum`, and within
   `maxStakePerClaim` (`minAttackStake` computes it; 0 means impossible under the cap). For an
   `Observed` claim a vote cannot beat the observers at all, so the attack applies only to `Voted`
   claims, which stay level 4 and never settle a market (item 12).
10. **Every economic and timing number is a `Params` field**, snapshotted per claim when it opens
    (docs/spec/stakes-and-markets.md has the table). The defaults are placeholders for a partner's
    token-economics split.
11. **Futures (S6).** `OnyxFutures` runs parimutuel markets on a future value of one fact: bucket
    edges (one edge = over/under), `tradingClose`, `observableAt`, `resolveBy`. Bets go into one
    pool per bucket. Winners split the losing pools pro rata after the fee.
12. **A market resolves only from a finalized fact at level 1 or 2** (`maxLevel`, default 2) that
    was accepted at or after `observableAt`. A value decided by a vote or the fallback oracle is
    level 4 and never settles a market (`test_VoteDecidedFactNeverResolvesAMarket`).
13. **Trading closes at least `minCloseLead` before the value can be observed**, so nobody bets
    after seeing it. Observers and the arbiter may not bet.
14. **Void.** A winning bucket with no bets, or no answer by `resolveBy`, voids the market and
    refunds every bet.
15. **Conservation is tested**: shared vectors (`fixtures/markets/vectors.json`), fuzz tests and an
    invariant run check that every token the contracts hold is in their books.
16. **Duplicates earn nothing.** A claim equal to the fact's accepted or open value takes no bond
    and earns nothing; a different value while one is open, or after one is accepted, is refused.
    One exception (review finding STK-14): an `Observed` claim may replace a value accepted by a
    vote or the fallback oracle (level 4), so a cheap vote cannot lock a fact that observers can
    still read and void every market on it. A value accepted by observation is final.

## Consequences
- The demo (`scripts/demo-markets.sh`) plays the whole story in one run and prints a ledger. It
  runs in a local EVM inside `forge test`, so it is part of check-all.
- A validator who backs a correct claim that nobody disputes earns nothing (there are no losers to
  pay it). That is a partner's question Q1 in numbers: steady-state attention needs another source of
  pay (tracker T4/T6).
- The owner key is the observer registry and the fallback arbiter on testnet: one party can decide
  contested claims. Acceptable for a testnet prototype only.
- Not built: a Rust client (`onyx-markets`), the website's stake step and markets page (the
  concept UI on `product-onboarding`, U6A, is the place), an anvil or Base Sepolia deployment, and
  the drawn-panel fallback.
- Before anything of real value is staked or traded: a legal review (event contracts and
  prediction markets are regulated in many places).

## Amendment 1 (2026-10-25, ROADMAP S3b, docs/pre-public-loop.md PP2): the drawn-panel fallback
`OnyxPanelOracle` replaces the one owner key as the fallback oracle (item 7). `OnyxPanelOracle`
is the arbiter (`arbiter()` is the contract itself), so no single key answers. It does not reuse
`OnyxDisputes`: that contract judges settlement shards by Merkle root, and changing an audited
contract for a value claim is not worth it. It reuses its draw: `blockhash` of the request's block,
the seats with the smallest keys.

1. **Seats.** Anyone bonds exactly `seatBond` tokens for one seat per address (at most 256). A
   seat on an open panel cannot leave.
2. **Draw.** From the block after the request, within 256 blocks: the `panelSize` eligible seats
   with the smallest `keccak256("onyx-fallback-panel-v1", claimId, beacon, seat)`. Eligible: seats
   that joined before the request's block, never the claim's submitter or anyone with a stake on
   the claim. A lapsed beacon, or too few eligible seats, answers void: a draw is never re-rolled.
3. **Votes.** Commit, then reveal, one answer (correct, wrong, void) in `commitWindow` and
   `revealWindow`. `majority` (a strict majority of the panel) matching reveals decide; otherwise
   void. A drawn seat that does not reveal loses `noShowPpm` of its bond to the treasury and its
   seat. A minority vote is not slashed, so members are not pushed to guess the crowd.
4. **Timing.** The stake contract's `fallbackWindow` must exceed the draw plus both windows;
   otherwise the claim times out to void, which refunds everyone.
5. **Level.** A panel answer is still level 4: it never settles a futures market (item 12).

Open: panel members are not paid (STK-10, partner economics); one person can hold many seats
from many addresses (STK-16); the beacon is a block hash the Base sequencer could bias (STK-17,
as ADR-0021). The demo and the shared test fixture keep the owner-key oracle as a simple stand-in.
Tests: `contracts/test/OnyxPanelOracle.t.sol` (14 cases and a fuzz test of the tally and books).

## Amendment 2 (2026-10-27, NORTH-STAR NS11, docs/north-star-loop.md NL6): markets on Base Sepolia settle from registry facts
The futures read their values through `IOnyxFacts`. `OnyxStakes` answers only for claims its own
re-observation or vote decided; the level-2 rounds that run live on Base Sepolia (ROADMAP B6)
record their results in `OnyxRegistry` instead. `OnyxObservedFacts` connects the two, so a market
settles from a value independent observers recorded, without a second round.

1. **Only a hash is on chain.** The registry keeps a fact's result hash,
   `keccak256("onyx-observe-result-v1" ‖ borsh(spec id, round id, outcome))`. Anyone calls
   `prove(fact, round, value)`: the contract rebuilds the hash of
   `ObservationOutcome::Value(Answer::Number(value))` with the recorded spec and accepts the value
   only when it equals the recorded result. Then the value is final at level 2, accepted at the
   proving block's time. Vectors: fixtures/markets/observed-facts.json (Rust and Solidity).
2. **A market is on a future value.** The owner `list`s the fact before its value exists, with
   the first block a recording may come from (`notBeforeBlock`, near the trading close). A fact
   the registry already recorded cannot be listed, and an earlier recording never proves. The
   market tool (`onyx_level2::markets::market_fact`) also refuses a date the source's own rule
   lets a round observe in a window that starts at the close, and one that no round may observe
   by `observableAt`.
3. **The key is the registry's.** The listed key is `OnyxRegistry.factKeyOf` under the template
   the market names. `prove` requires the registry's key for that (metric, subject, window) to be
   the same, so a market on another template never settles: it voids at `resolveBy`.
4. **Deployment.** `script/DeployMarkets.s.sol` (`scripts/markets-sepolia.sh deploy`) deploys
   tONYX, `OnyxReobserve`, `OnyxPanelOracle`, `OnyxStakes`, `OnyxObservedFacts` and `OnyxFutures`
   next to a deployment's registry. The futures read `OnyxObservedFacts`, and the drawn panel is
   the stakes' fallback and the futures' arbiter. The deployer key mints tONYX, owns the futures
   and lists facts. The economic numbers stay the placeholders of item 10.
5. **Trust.** The recorded spec and result are the coordinator's attestation (ADR-0030), as for
   every level-2 fact: a client checks them off chain (`level2-check`), but a market pays on chain
   without that check. That is acceptable for a test token only. Before anything of value is
   traded, a challenge window between `prove` and `resolve` is needed (THREATS NL6-2, TODO S6).

Tests: `contracts/test/OnyxObservedFacts.t.sol` (a market settled end to end from a recorded
value, a wrong value or round, a recording before the bound, an already recorded fact, another
template, a fuzz test and the vectors), `contracts/test/DeployMarkets.t.sol`, and
`adapters/source-fiscaldata/tests/market.rs` (the market's fact is the round's registry fact, and
its value proves).

## Amendment 3 (2026-10-27, docs/north-star-loop.md NL14 and NL18): guarded listings, and parametric policies
The NL6 review (THREATS NL6-10..12) found that a market could close after the source publishes,
that the deployer (the registry's coordinator and the futures owner) could bet and then void a
losing market by never recording its round, and that an early recording could leave a market
unable to settle. Parametric insurance (docs/spec/parametric-insurance.md) settles from the same
facts and needs the same guards.

1. **A listing states its latest close.** `OnyxObservedFacts.list` takes `closeBy`: the source's
   earliest publication for the fact, less a margin (`Source::earliest_public`, from the source's
   terms review). It implements `IOnyxFactGuard`: `closeBy(fact)` and `isInsider(who)` (the
   listing owner and the registry's coordinator, read from the registry's ledger).
2. **Guarded futures.** `OnyxFutures` takes a `guard` (zero, or the facts source itself). With a
   guard, a market's trading closes no later than `closeBy`, an unlisted fact opens no market, and
   the guard's insiders may not bet. Without one, the owner and the treasury still may not bet.
3. **The earliest recording block is at `observableAt`.** The market and policy tools list a fact
   with `notBeforeBlock` estimated at `observableAt`, not at the close, so an early recording
   never proves and a product voids at its deadline instead (NL6-11; the futures' Rust tool is
   lane A's, `scripts/insurance-sepolia.sh offer` does it for policies).
4. **`OnyxParametric`.** The owner offers a product: a listed fact, a comparator (at least / at
   most), a threshold, a fixed payout and premium per policy, `buyBy` (no later than `closeBy`
   and at least an hour before `observableAt`), `resolveBy` and a policy cap. Underwriters deposit
   tONYX for shares; a policy sells only when the free pool covers its payout, which is reserved.
   After `observableAt` anyone settles from the fact's value at level 1 or 2 accepted at or after
   `observableAt` (a vote or panel answer, level 4, never settles a policy): triggered, holders
   claim the payout; not triggered, the reserve is released; no value by `resolveBy`, holders
   claim their premiums back. The premiums of a settled product join the pool. The owner, the
   guard's insiders and re-observation observers may neither buy nor underwrite. An underwriter
   leaving takes its share of the free money only, never of reserved money. Shares are priced with
   one virtual share and token, and the pool is counted internally, so a donation moves no price.
5. **Deployment.** `script/DeployInsurance.s.sol` (`scripts/insurance-sepolia.sh deploy`) deploys
   a new `OnyxObservedFacts`, a guarded `OnyxFutures` and `OnyxParametric` next to the first
   deployment's tONYX, `OnyxReobserve` and `OnyxPanelOracle`. The first facts and futures stay for
   market 1 (NL15).
6. **Trust and limits.** As in amendment 2 item 5: the recorded result is the coordinator's
   attestation, acceptable for a test token only. Pricing is the owner's choice per product; a
   payout can differ from any real loss (basis risk). Every number is a placeholder for the
   partners (NS12).

7. **Delayed observer changes** (THREATS LC16-1). `OnyxReobserve(changeDelay)`: an added observer
   is assigned only to tasks opened after the delay, a removed one stays assigned until the delay
   passes and is then `prune`d by anyone; both count as insiders (`isObserver`) meanwhile.
   `DeployMarkets` uses 3 days, above the stakes' commit and reveal windows.
8. **Validator-only panel seats** (THREATS LC16-2). `OnyxPanelOracle` takes a `gate`, the
   settlement's staked validator set (`IOnyxValidatorSet`, `OnyxSettlement.validators`): only an
   active, staked validator that is not unbonding may join a seat or be drawn. `DeployMarkets`
   passes `registry.ledger()`. Items 7 and 8 are not redeployed on Base Sepolia yet: the live
   reobserve and panel contracts keep the old rules until the final release redeploys them.

Tests: `contracts/test/OnyxParametric.t.sol` (a policy pays end to end from a recorded value, one
expires, at-most, a void refunds premiums, each rule refused, a leaving underwriter, a vote-decided
value, a value accepted early, a donation, an emptied pool), `contracts/test/invariant/
ParametricInvariants.t.sol` (the contract holds what it owes; reserves never exceed the pool), and
the guard tests in `contracts/test/OnyxObservedFacts.t.sol`, `contracts/test/OnyxReobserve.t.sol`,
`OnyxPanelGateTest` in `contracts/test/OnyxPanelOracle.t.sol` and `contracts/test/DeployMarkets.t.sol`.
