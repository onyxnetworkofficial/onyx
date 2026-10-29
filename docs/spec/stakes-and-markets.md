# Spec: staked submissions and data futures

## Scope
The testnet prototype of ADR-0042 (ROADMAP S3, S4, S6; partner tracker T10): a submitter stakes
test tokens that a value of a fact is right, others stake for or against it, observers re-read
past data, a fallback oracle decides what neither can, and parimutuel futures markets pay on a
future value of a fact. Contracts in `contracts/src`: `OnyxStakes`, `OnyxReobserve`,
`OnyxFutures`, `OnyxPayout`, `OnyxTestnetOwnerOracle`, `OnyxTestToken`, `IOnyxMarkets`.

## Interfaces
- `OnyxStakes.submit(fact, value, kind)`: `kind` is `Observed` (re-readable past data) or `Voted`.
  Takes `submitBond`. A value equal to the fact's open or accepted value is a `Duplicate` (nothing
  taken, nothing paid); a different value while one is open is `FactBusy`, after acceptance
  `AlreadyFinal`, except that an `Observed` claim may replace a level-4 (vote or fallback)
  value (STK-14).
- `commitStake(claim, commitHash(claim, staker, side, salt), amount)` before `commitClose`;
  `revealStake(claim, side, salt)` before `revealClose`. Side 1 = correct, 2 = wrong. The
  submitter, the observers and the arbiter may not stake (`mayStake`).
- `OnyxReobserve.commit/reveal(task, status, value, salt)`: the claim's id is the task id; status
  1 = value seen, 2 = unavailable. `outcome` needs a strict majority of all assigned observers.
- `resolve(claim)` after `revealClose`: observation (an `Observed` claim with a majority), else
  `voteOutcome(correct, wrong, quorum, decisivePpm)`, else `Fallback`. `settleFallback` reads the
  oracle's answer (1 correct, 2 wrong, 3 void) or voids after `fallbackWindow`.
- `owed(claim, who)`, `withdraw(claim)`: winners' payout `stake + stake × distributable /
  winningPool`, rounding down; dust to the treasury after the last winner.
- `factValue(fact) -> (final, value, level, decidedAt)`: level 2 when decided by observation, 4
  otherwise.
- `minAttackStake(paramsId, honest)`: the smallest winning attack stake (0 = impossible).
- `OnyxFutures.createMarket(fact, edges, tradingClose, observableAt, resolveBy)` (owner),
  `bet(market, bucket, amount)` before `tradingClose`, `resolve(market)` from `factValue` at level
  at most `maxLevel` accepted at or after `observableAt`, `withdraw(market)`.
- `scripts/demo-markets.sh`: the whole story with a ledger (`OnyxMarketsDemoTest`).
- `OnyxObservedFacts` (ADR-0042 amendment 2): the futures' `IOnyxFacts` on Base Sepolia.
  `list(metric, template, subjectKey, windowEnd, dated, notBeforeBlock)` (owner, before the value
  exists); `prove(fact, round, value)` (anyone) accepts the number whose
  `resultHash(spec, round, value)` equals the registry's recorded result, recorded at or after
  `notBeforeBlock`; `factValue` answers level 2 at the proving block's time.
- `scripts/markets-sepolia.sh deploy|market` (`onyx_level2::markets`): deploy next to a registry;
  open, bet, settle and read markets on a dated source's facts (docs/ops/demo-testnet.md).

### Parameters (placeholders; a partner's token-economics table replaces them)

| Parameter | Default | Meaning |
|---|---|---|
| `submitBond` | 10 tONYX | Bond a submitter stakes |
| `submitReward` | 1 tONYX | Paid to a right submitter from the reward pool |
| `minStake` / `maxStakePerAddress` / `maxStakePerClaim` | 1 / 100 / 1,000 tONYX | Stake bounds |
| `quorum` | 20 tONYX | Least revealed stake for a vote to decide |
| `decisivePpm` | 666,667 (2/3) | Share one side needs |
| `feePpm` | 20,000 (2%) | Treasury fee on the losing side's money |
| `commitWindow` / `revealWindow` / `fallbackWindow` | 1 / 1 / 3 days | Timing |
| Futures `minBet` / `maxBetPerAddress` / `maxPoolPerMarket` | 1 wei / 100 / 10,000 tONYX | Bet bounds |
| Futures `feePpm` / `minCloseLead` / `maxBuckets` / `maxLevel` | 2% / 60 s / 8 / 2 | Fee, close lead, buckets, weakest level that settles |

## Assumptions
- Testnet only; tONYX has no value and mints only on chain 31337 or Base Sepolia.
- One owner key registers observers (testnet only). The fallback arbiter is the drawn panel
  `OnyxPanelOracle` (ADR-0042 amendment 1); the demo and test fixture still use one owner key.
- Observers are honest in majority for `Observed` claims; the port lacks draws, bonds and
  tolerance (TODO S4b).
- A futures market is about one fact whose value appears at a known time.

## Open questions
- Token economics (tracker T12): bond, reward, fee and caps; whether rewards are token emissions.
- What pays a validator who backs a correct claim that nobody disputes (it earns nothing today;
  tracker T4/T6).
- Cap squatting on `Voted` claims (THREATS STK-6): one party filling `maxStakePerClaim` first.
- Panel pay and seat-sharing limits (STK-10, STK-16); numeric tolerance in re-observation (S4b).
- Legal review before anything of value is staked or traded.
