# ADR-0056: Credits ledger: one balance per account on OnyxPrepaid, earned and usage credits, relayed gas netted from credits

## Status
Accepted, 2026-10-30, with Amendment 1 below; built and rehearsed on a local anvil
(docs/contributor-loop.md CL6, `scripts/credits-anvil.sh`), not deployed on Base Sepolia
(docs/ops/runbooks/deploy-credits.md). Proposed 2026-10-29. Builds on
ADR-0046 (prepaid balances: deposit, cumulative EIP-712 vouchers, batched redemption, exit delay),
ADR-0025 and ADR-0026 (`OnyxRevenue`: fee split, pull payments, the contributor pass) and
ADR-0049 (holds). Testnet only. No token, emission or price logic: the asset is whatever ERC-20
the deployment names, and the API never names it in a field.

## Context
Money paths today:
1. A buyer pays per answer with x402, or deposits into `OnyxPrepaid` and pays with vouchers that
   sign a running total (`Voucher(address buyer,uint128 cumulative)`, header
   `ONYX-PREPAID-VOUCHER`). The reporter redeems the latest voucher of up to 64 buyers into a pool
   and allocates the pool to `(round, shard)` books in `OnyxRevenue` with calls that name no
   buyer.
2. A contributor earns shares of fees in `OnyxRevenue` (`claim`, then `withdraw` to its wallet).
   Level-2 observers earn nothing on chain yet (THREATS L2API-3). Accepted work also gives the
   contributor pass: free reads for about 30 days (ADR-0025 item 5).
3. Every on-chain action costs the actor gas in ETH: an observer's `postCommit`, `postReceipt`
   and sealed reveal posts, a buyer's `deposit`.

So a contributor who also buys holds two balances (earnings in `OnyxRevenue`, credit in
`OnyxPrepaid`), and needs ETH for gas before it can take part at all. The contributor loop asks
for one ledger per account: usage credits and earned credits, gas netted from credits, on
`OnyxPrepaid`, so that a token can replace the asset at mainnet without changing the API.

## Decision
1. **One account, one spendable balance.** An account is an EVM address. Its credits are its
   `OnyxPrepaid` account: `spendable = deposited - redeemed - held` (`held`: ADR-0049 holds the
   server journalled but did not charge). Credits arrive in two ways and spend the same way; the
   ledger tags every line with its origin for display and for the anti-gaming rules (item 6).
2. **`OnyxPrepaid` v2** (a new deployment; no proxies, ADR-0005) keeps the voucher type, the
   header and the redemption and exit rules unchanged, and adds:
   - `depositFor(address account, uint128 amount, uint8 origin)`: anyone pays the asset in for
     `account`; `origin` is 1 deposit, 2 earned, 3 refund, 4 payout (event
     `Credited(account, amount, origin)`). Only the account's own `deposit` cancels its pending
     exit; `depositFor` never touches `exitAt`, so nobody can keep another account from leaving.
   - `depositWithAuthorization(...)`: the EIP-3009 `transferWithAuthorization` path
     `OnyxRevenue.settle` already uses (ADR-0026), so a buyer with no ETH can deposit through a
     relayer.
   - `payRelayer(uint128 amount)` (reporter only): pays pooled gas lines (item 4) to the
     immutable `relayer` address set at deploy, never above the pool.
   `OnyxRevenue` v2 adds `withdrawToCredits(IOnyxPrepaid p)`: moves the caller's whole earned
   balance into `p.depositFor(caller, amount, 2)` in one call. `OnyxStorage` refunds (ADR-0055)
   and correction-market payouts (ADR-0057) may pay into `depositFor` the same way.
3. **The ledger journal.** The prepaid usage journal (ADR-0046 amendment 1: one JSON line per
   answer, synced before the answer leaves) becomes the credits journal, append-only:
   ```text
   CreditLine { seq: u64, account: Address,
                kind: Answer | ObserveHold | ObserveCharge | Release | Gas { action } |
                      Deposit | Earned | Refund | Payout,
                amount: u128,         // asset's smallest unit
                cumulative: u128,     // the voucher total after a spend line; 0 otherwise
                reference: Hash32,    // request key, ticket hash, transaction hash
                at: u64 }
   ```
   Spend lines are checked and journalled exactly as vouchers are today; arrival lines
   (`Deposit`, `Earned`, `Refund`, `Payout`) are read from `Credited` events at the `finalized`
   block, as `ChainAccounts` reads accounts (ADR-0046 amendment 2). The reporter's pass is
   unchanged; gas lines are allocated with `payRelayer`.
4. **Gas netted from credits through a relayer.** Only calls that already carry the actor's own
   signature are relayed, so the relayer cannot act for anyone: `OnyxRegistry.postCommit`,
   `postReceipt` and the sealed reveal posts (signed by the observer), and
   `OnyxPrepaid.depositWithAuthorization`. Calls that use `msg.sender` (`OnyxStakes.commitStake`,
   bets) are not relayed in v1.
   - `POST /v1/relay` takes `{action, args}` with an `ONYX-PREPAID-VOUCHER` that adds exactly the
     action's fee from a fixed, published schedule (`GET /v1/credits/schedule`, credit units per
     action). A fixed schedule needs no ETH-to-credit price feed; the relayer refuses
     (`503 relay_busy`) when the base fee is above its cap, as the live runner's
     `MAX_FEE_PER_GAS` does.
   - The voucher is held, not charged (ADR-0049's hold): the relayer simulates the call
     (`eth_call` at `latest`), sends it, and charges only when the transaction is mined without a
     revert; anything else releases the hold.
   - The relayer key is its own: the runner refuses to start when it is also the coordinator, the
     receipt signer, the reporter, the registry owner or a pool member (as `check_distinct`,
     ADR-0027 amendment 9). Caps: per account 60 relays an hour; per relayer a daily gas budget.
5. **The API does not change when the asset does.**
   - `GET /v1/credits/{account}`: `{account, asset: {symbol, decimals, chainId, address},
     deposited, earned, spent, held, spendable, exitAt}`, amounts as decimal strings in the
     asset's smallest unit. Without a proof it returns only what the chain already shows
     (`deposited`, `redeemed`, `exitAt`). With `ONYX-ACCOUNT-PROOF` (EIP-191 over
     `onyx-account-read-v1 / account / issued / ledger`, 120 s, single use: the contributor pass
     rules, R-REV-5) it adds the journal's numbers.
   - `GET /v1/credits/{account}/lines?after=` (proof required): that account's own lines, paged.
   - Existing price fields gain an `asset` sibling; no field or header is named after USDC.
   - At mainnet a new `OnyxPrepaid` is deployed with the token as its immutable `token`; routes,
     headers, voucher type and shapes stay. Testnet balances never carry over (stated on the
     site).
6. **Anti-gaming: no credit is created from nothing.**
   - R-CR-1. Every credit is the asset paid in by someone: a deposit, an earned share of a fee a
     buyer paid, a refund, a market payout. No sign-up grant, no reward for an approved
     submission, no Onyx-side faucet. So an account that "takes credits without contributing"
     can only spend what it or a buyer put in.
   - R-CR-2. Earned credits come only from paid demand (dataset-backing R-DB-0's rule): approval
     or acceptance alone earns nothing; a share is earned when a buyer pays for the data.
   - R-CR-3. Buying one's own data never pays: the sum of every share one address can receive
     from one fee stays below the fee (this ADR adopts the draft rule R-DB-6 of
     docs/spec/dataset-backing.md, `MIN_TREASURY_BPS > 0`, for every split), so each wash loop loses
     at least `MIN_TREASURY_BPS` of the price.
   - R-CR-4. The contributor pass stays what it is (free reads, rate limited) and never turns
     into credits.
   - R-CR-5. A released hold restores `spendable`; it never adds an `Earned` line.
   - R-CR-6. Credits are not transferable between accounts (no `transferCredits`), so there is no
     market in credits and no way to move a farmed balance; leaving is the exit delay, as today.
   - R-CR-7. No negative balance: no relay or hold without spendable credit.

## Consequences
- A contributor can earn, spend on data and pay for its own posts from one balance, without
  holding ETH.
- The operator fronts gas and is repaid from pooled gas lines; a mined transaction it was not
  repaid for is its loss (the hold is charged only after mining, so the buyer side is safe).
- The reporter stays trusted for allocation, now also for `payRelayer` (as REV-1).
- Two contracts are redeployed (`OnyxPrepaid` v2, `OnyxRevenue` v2); the hosted node's
  `ONYX_PREPAID` points at the new one, and old vouchers stay valid only on the old contract.

## Alternatives
- **A separate `OnyxCredits` contract with its own non-transferable balances**: more surface, a
  second exit path, and the same trust in the reporter.
- **ERC-2771 meta-transactions everywhere**: changes `msg.sender` in contracts that were
  reviewed with it; refused.
- **An ERC-4337 paymaster**: needs a bundler and an entry-point deployment; reconsider when the
  relayer's allowlist is too narrow.
- **Per-action gas charged at the real gas price**: needs an ETH price on chain or a trusted
  quote; a fixed schedule is predictable for contributors.

## Threats (rows CL6-1..CL6-7 go into THREATS.md with their tests when built)
| ID | Threat | Mitigation | Test to write |
|---|---|---|---|
| CL6-1 | Wash buying to farm earned credits | R-CR-3's bound, fuzzed over every split | `testFuzz_NoAddressEarnsBackAFullFee` |
| CL6-2 | The relayer sends a call the actor did not sign | Only self-signed actions relayed; simulated first | `the_relayer_refuses_an_action_without_the_actors_signature` |
| CL6-3 | A failed relay is charged | Hold, charge only on a mined success | `a_reverted_relay_releases_its_hold` |
| CL6-4 | Relay spam drains the operator's ETH | Prepaid fee first, per-account rate, daily budget | `relays_over_the_hourly_cap_are_refused_before_sending` |
| CL6-5 | A balance read reveals what an account bought | Lines need the account's own proof; public view shows chain data only | `credits_lines_need_the_accounts_own_fresh_proof` |
| CL6-6 | `depositFor` used to cancel someone's exit (griefing) | An exit is cancelled only by the account's own `deposit`; `depositFor` never touches `exitAt` | `OnyxPrepaidV2.t.sol::test_DepositForDoesNotCancelAnExit` |
| CL6-7 | Earned credits double counted (claimed in `OnyxRevenue` and credited) | `withdrawToCredits` zeroes the balance first, in one call | `OnyxRevenueV2.t.sol::test_WithdrawToCreditsMovesTheBalanceOnce` |

## Open questions
1. The relay fee schedule (credit units per action): set from measured gas on Base Sepolia.
2. Should earned credits be withdrawable to the wallet at once (proposed, as today in
   `OnyxRevenue`) or only spendable?
3. Observer pay (THREATS L2API-3) and any emission are NS12 decisions; this ledger only carries
   what they decide.

## Amendment 1 (2026-10-30): as built

1. **Origin tags are the payer's, not the caller's choice.** `depositFor` takes `origin` but it
   must equal `originOf(msg.sender)`: `revenue` credits only 2 (earned), a source the owner named
   with `setSource` only its own 3 (refund) or 4 (payout), anyone else only 1 (deposit). Without
   this, anyone could make a bought balance look earned (THREATS CL6-8).
2. **`deposit` emits `Credited(account, amount, 1)`** like every other arrival (no `Deposited`
   event in v2), so the arrival lines come from one event.
3. **`depositWithAuthorization` uses EIP-3009 `receiveWithAuthorization`**, not
   `transferWithAuthorization`: only the payee (the ledger) may submit it, so a copied
   authorization cannot be front-run into an uncredited transfer (CL6-11). It credits the signer
   only, exactly the amount that arrived, and never touches `exitAt` (an old signed deposit must
   not undo a later exit).
4. **`payRelayer` has a daily cap**: the immutable `maxRelayPerDay` per UTC day, besides the pool;
   the relayer is immutable and never the reporter or the owner (constructor and `setReporter`).
   The reporter's pass reads `relayDay` and `relayPaidToday` and pays only what the day has left.
5. **`OnyxRevenueV2.MIN_TREASURY_BPS` = 100 (1%)**, checked in the constructor (R-CR-3).
   `withdrawToCredits(p)` requires exactly the balance to leave and resets the allowance after.
6. **Voucher domain unchanged**: name `OnyxPrepaid`, version `1`; the contract address in the
   domain is what keeps v1 vouchers worthless on v2. A node's prepaid journal is bound to one
   contract (`<journal>.domain`; every voucher checked at start) and new journals are named per
   contract (review CL6-R1-1).
7. **Relay as built:** actions `post_commit`, `post_receipt`, `post_sealed_reveal`, `deposit`;
   order of checks: the action's signature, the voucher's buyer and signature, the deposit's
   validity window (valid now and for the hold time plus 60 s), the hold (exactly the fee; a
   relayed deposit may cover its own fee from the amount it pays in), then the per-account caps
   (60 an hour, 3 failures a day), the base-fee cap, the simulation, the daily gas budget and each
   account's share of it (2.5M of 20M gas), and only then the send. A mined success charges a gas
   line (round `u64::MAX - 1`, shard `keccak256("onyx-relay-gas-v1")`), paid to the relayer with
   `payRelayer`, never allocated to a book. Anything else, including an unknown outcome
   (`relay_pending`), releases the hold: an action mined without its fee is the operator's loss.
8. **Fees (open question 1):** provisional `post_commit=1000, post_receipt=1000,
   post_sealed_reveal=3000, deposit=2000` (smallest units of a 6-decimal asset), set by
   `ONYX_RELAY_FEES`; measured on anvil: deposit about 94,800 gas, commit 179,700, receipt 112,700.
   A flat fee lets many funded accounts together use the day's gas budget (THREATS CL6-R1-2,
   TODO S7: a fixed credits-per-gas rate from Base Sepolia measurements).
9. **Reads:** `GET /v1/credits/{account}` gives `deposited`, `redeemed`, `exitAt` publicly and,
   with the account's own proof, `credited` (by origin, from `Credited` events at the finalized
   block), `spent`, `held`, `spendable`. `GET .../lines` returns the journal lines (`seq`, `kind`
   answer, observe_hold, observe_charge, relay_hold, gas, release) and, beside them, the on-chain
   arrivals as their own list (block and transaction), not interleaved by `seq`. A relayed deposit
   becomes spendable once the finalized read shows it, as every deposit.
10. **Open question 2 answered for testnet:** earned shares stay withdrawable to the wallet
    (`withdraw`) as well as movable into credits.

