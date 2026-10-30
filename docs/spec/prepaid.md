# Spec: prepaid buyer balances

Partner tracker T7 (a partner's questions 7 and 8), pre-public loop PP5, ADR-0046. Testnet only.

## Scope
With x402 every answer is a visible transfer from the buyer's address, so anyone can link a fund's
query wallet to what it buys. A prepaid balance shows a deposit and batched totals instead.

## Interfaces
- Contract `OnyxPrepaid` (contracts/src/OnyxPrepaid.sol), beside `OnyxRevenue`, which it pays
  through `OnyxRevenue.pay`:
  - `deposit(amount)`: the buyer's token moves in; a deposit cancels a pending exit.
  - `redeem(Voucher[] vs)` (reporter, at most 64): each voucher is the buyer's EIP-712 signature
    of `Voucher(address buyer, uint128 cumulative)` in the domain `("OnyxPrepaid", "1", chainId,
    contract)`; `cumulative` must be above the buyer's `redeemed` and at most its `deposited`; the
    difference moves into the `pool`. Low s and v in {27, 28} only.
  - `allocate(Item[] items)` (reporter, at most 64): pays pooled money to `(round, shard)` books
    with `OnyxRevenue.pay`; never more than the pool; names no buyer.
  - `requestExit()`, then `withdraw()` after `exitDelay` (1 hour to 30 days; the deployment's
    choice): the unspent balance `deposited - redeemed` returns; spent money stays spent.
  - `spendable(buyer)`: what is left, 0 while an exit is pending.
- Rust `onyx_api::prepaid`: `domain_separator`, `voucher_digest`, `Voucher::{sign, signed}`,
  `PrepaidBook::{charge, to_redeem, refresh}`. Shared vectors: fixtures/prepaid/vectors.json.

## Rules
1. An answer priced `p` is paid with a voucher whose total is exactly the last total the server
   took (or the chain's `redeemed`) plus `p`, signed by the buyer, within the deposit, with no exit
   pending. Anything else is refused before the answer is served.
2. The server redeems each buyer's latest voucher well inside `exitDelay` (at most half of it):
   after a buyer's exit is withdrawn, an unredeemed voucher is worth nothing.
3. Redemptions of many buyers go in one call; allocations to books go in other calls, so the chain
   does not pair a buyer with a book.
4. The reporter is trusted for which book receives pooled money, as with `OnyxRevenue.credit`
   (THREATS REV-1); it cannot take more from a buyer than the buyer signed.

## Assumptions
- The token is the testnet USDC the API already takes; `approve` and `transferFrom` behave as
  ERC-20 requires.
- Buyers sign with EOA keys (ERC-1271 contract wallets are not accepted, as for x402).
- The reporter key is the operator's, rotated by the owner as in `OnyxRevenue` (REV-L1-F4).

## What the chain still shows (the privacy limit)
Every deposit, every redeemed total per buyer and every withdrawal, by address; and, apart from
them, how much each book received. So an address is visibly a buyer, and its spend is visible in
steps of a batch. Not shown: which row or book a buyer paid for, or each query. A buyer who wants
more uses a fresh address per deposit. Shielded pools are out of scope (legal advice first).

## HTTP (ADR-0046 amendment 1)
- Request header `ONYX-PREPAID-VOUCHER: 0x<buyer, 20 bytes>:<cumulative, decimal>:0x<65-byte
  signature>`, on any paid route, instead of `PAYMENT-SIGNATURE`. One spelling only: no leading
  zeros, lower or upper hex.
- Response header `ONYX-PREPAID-RESPONSE: total=<cumulative>; spendable=<left>`.
- Errors: 400 `prepaid_not_offered` (the server takes x402 only), 400 `invalid_prepaid`, 402
  `prepaid_refused` (`last` and `price` say what to sign next), 503 `prepaid_unavailable`
  (account not readable, or the journal not writable; nothing charged).
- Rust `onyx_api::prepaid_gate`: `parse_voucher`, `voucher_header`, `AccountSource`,
  `PrepaidState::{open, refresh, charge, to_redeem}`.

- Rust `onyx_api::prepaid_chain`: `ChainAccounts` (finalized for the server, latest for the
  reporter), `PrepaidReporter::pass` (redeem, then allocate; allocated lines marked in
  `<journal>.allocated` before the send), the calldata of `accounts`, `redeem` and `allocate`.
  Rehearsal: `scripts/prepaid-anvil.sh`.

- Node: `ONYX_PREPAID` (docs/ops/config.md, ADR-0046 amendment 3).

## Credits ledger (CL6, ADR-0056 and its amendment 1)
`OnyxPrepaidV2` is the same balance with more ways in: `depositFor` (the origin tag is the
payer's: 1 deposit, 2 earned from `OnyxRevenueV2.withdrawToCredits`, 3 refund and 4 payout from
sources the owner names), `depositWithAuthorization` (EIP-3009 `receiveWithAuthorization`, for an
account with no ETH, through the relayer) and one more way out, `payRelayer` (relay fees from the
pool to the immutable relayer, capped per day). Rules R-CR-1..7 of the ADR hold: no credit from
nothing, earned only from paid demand, no transfer between accounts, no negative balance. The
voucher, its header and the exit are unchanged; vouchers are bound to their contract. HTTP:
`/v1/credits/schedule`, `/v1/credits/{account}` (public chain view; journal numbers with the
account's own `ONYX-ACCOUNT-PROOF`), `/v1/credits/{account}/lines` (proof only) and `POST
/v1/relay` (docs/api/openapi.json, tag Credits). Threats: THREATS.md CL6.

## Open questions
A deployed `OnyxPrepaid` on Base Sepolia, and an alert when the pool stays above zero (an
allocation that was cut short, THREATS PRE-10).
