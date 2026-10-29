# ADR-0046: Prepaid buyer balances with signed running totals and batched settlement

## Status
Accepted for testnet, 2026-10-25 (partner tracker T7, a partner's questions 7 and 8; pre-public loop
PP5). Implemented by `contracts/src/OnyxPrepaid.sol`, `onyx_api::prepaid` and the shared vectors
`fixtures/prepaid/vectors.json`; spec docs/spec/prepaid.md. The HTTP path is ROADMAP S7.

## Context
An x402 payment is a transfer from the buyer's address per answer, so a fund's query wallet and
what it buys are public. A partner asked what a provider and the public see about a buyer.

## Decision
1. A buyer deposits once into `OnyxPrepaid`. Each answer is paid off chain with a voucher: the
   buyer's EIP-712 signature of its running total (`Voucher(address buyer,uint128 cumulative)`,
   domain `OnyxPrepaid` version 1, chain id, contract). A running total, not a per-query voucher:
   one signature covers everything before it, and the server keeps only the latest.
2. The reporter redeems the latest voucher of many buyers in one call (into a pool), and
   allocates the pool to books with `OnyxRevenue.pay` in other calls, which name no buyer.
3. A buyer leaves with `requestExit` and `withdraw` after `exitDelay`; the delay lets the reporter
   redeem the last voucher it took. A deposit cancels a pending exit.
4. Low s and v in {27, 28} only; no ERC-1271; testnet chains only (Anvil, Base Sepolia).
5. The payout split and books stay `OnyxRevenue`'s: this contract only changes how the money
   arrives.

## Alternatives
- A payment channel per buyer with a counterparty signature: more state, no privacy gain here.
- Per-query vouchers: one signature check and one storage slot per query on redemption.
- Shielded pools: real privacy, but legal advice first (design proposals T7).

## Consequences
- The chain shows deposits, redeemed totals and withdrawals per address, and book payments
  separately; it does not pair a buyer with a row. A buyer wanting more uses fresh addresses.
- The reporter is trusted for which book gets pooled money (as for `credit`, REV-1) and must
  redeem within the exit delay; it can never take more than a buyer signed.
- Not done: the API header, the server's chain reads and the redemption schedule (ROADMAP S7).

## Amendment 1 (2026-10-25): the HTTP path, part 1 (pre-public loop PP10, ROADMAP S7)
1. A paid route takes the header `ONYX-PREPAID-VOUCHER: 0x<buyer>:<cumulative>:0x<signature>`
   instead of `PAYMENT-SIGNATURE`, on a server built with `ApiState::with_prepaid`. The price is
   the route's x402 price; the voucher must add exactly that to the last total the server took.
2. The server reads the buyer's `OnyxPrepaid.accounts` entry through an `AccountSource` and
   charges only against a read at most `max_age` seconds old (default 60, far below the
   contract's shortest `exitDelay` of one hour), so an exit is seen before more is taken.
3. The voucher is checked and appended to a usage journal (owner-only, one JSON line per answer:
   buyer, total, signature, price, round, shard, request key, time), synced to disk, before the
   answer leaves. A voucher the server cannot record is refused (503 `prepaid_unavailable`) and
   nothing is served: a voucher is money.
4. The identical retry (same header bytes, resource and client) of the latest paid answer is
   served again without a new charge, also after a restart (the journal keeps the request key).
   Any other reuse is refused (402 `prepaid_refused`, with `last` and `price` to sign next).
5. Not in part 1: the node's configuration and its chain reader for `accounts`, the redemption
   and allocation schedule from the journal, and a rehearsal on a local chain (ROADMAP S7 part 2).

## Amendment 2 (2026-10-25): the chain side (pre-public loop PP10 part 2a, ROADMAP S7)
1. `onyx_api::prepaid_chain::ChainAccounts` reads `OnyxPrepaid.accounts` with `eth_call`: the
   server at the `finalized` block (a deposit a reorg could undo is never spent), the reporter at
   `latest` (what it sends, the contract rechecks).
2. `PrepaidReporter::pass` redeems each buyer's latest journalled voucher above the chain's total
   (at most 64 per call), then allocates the redeemed usage to its books, summed per
   `(round, shard)`. It never changes the server's books.
3. Redeem is safe to repeat (the contract refuses a total it already redeemed). Allocate is not,
   so the journal lines a pass allocates are marked in `<journal>.allocated` (owner-only) before
   the transaction is sent, and unmarked only when nothing was broadcast or it reverted. A crash
   or an unknown outcome leaves them marked: the money stays in the contract's `pool`, visible on
   chain, and is never paid twice.
4. Rehearsed end to end on anvil by `scripts/prepaid-anvil.sh` (in check-all).
5. Still open (part 2b): the node's configuration (contract, journal, reporter key, a pass at
   most every half `exitDelay`), and the pass on a timer.

## Amendment 3 (2026-10-25): in the node (pre-public loop PP10 part 2b, ROADMAP S7)
1. `ONYX_PREPAID=<OnyxPrepaid address>` turns vouchers on in `onyx-node` (docs/ops/config.md).
   Only with `ONYX_PAYMENTS=facilitator` and the reporter on: the pass sends with the reporter's
   own sender, so one key has one nonce sequence.
2. At start the node reads the contract's `reporter()`, `token()`, `revenue()` and `exitDelay()`
   and refuses a contract whose reporter is not the operator key or whose token and revenue are
   not the manifest's.
3. The usage journal is `<state dir>/prepaid.journal`; the pass runs every
   `ONYX_REPORTER_EVERY_SECS`, never less often than half the exit delay (docs/spec/prepaid.md
   rule 2), and stops with the other background tasks at shutdown (an allocation cut short stays
   marked: PRE-10).

