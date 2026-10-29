# ADR-0025: Money loop: usage revenue split, contributor pass, dataset prices

## Status
Accepted, 2026-10-10 (the owner chose option A: "Do A"). Implemented in
`contracts/src/OnyxRevenue.sol` (+ the `revealsOf` reader in `OnyxSettlementExt`),
`crates/onyx-coordinator/src/revenue.rs`, `crates/onyx-api/src/{contrib,receipts,catalog}.rs`;
tests in `contracts/test/OnyxRevenue.t.sol` and `crates/onyx-api/src/tests.rs`; shared vectors in
`fixtures/revenue/vectors.json`. Deploy script `contracts/script/DeployRevenue.s.sol`. Not deployed.
Spec: docs/spec/revenue.md. Research: RESEARCH.md "Money loop".

## Context
Until now customers pay per call (x402, ADR-0023) and miners and validators are paid from a round
budget the coordinator funds (ADR-0022), but no fee reached the people whose work was sold
(pricing.md "Revenue split" was open). Option A, as put to the owner: split each paid call's fee
by role (suggested 50% miners, 20% validators, 10% label contributors, 20% treasury); "add data,
get data": accepted work in the last 30 days reads fresh data free or discounted; no token, USDC
only; AI agents pay per call or per dataset (0.1-1 cent per answer, 1-5 cents per dataset), old
data stays free. Facts that shape it: the x402 `exact` facilitator settles with EIP-3009
`transferWithAuthorization` straight to `payTo`, so the receiving contract is not told what was
bought; the token can say whether a `(payer, nonce)` authorization was used
(`authorizationState`). `OnyxSettlement` has about 1.4 KB left under EIP-170.

## Decision
1. **No token.** Fees, shares and withdrawals are in the x402 asset (USDC on Base Sepolia).
2. **`OnyxRevenue` is the x402 `payTo`**, a new contract beside the settlement (which it only
   reads). Fees come in two ways. (a) `credit(receipts)`: the API's reporter attributes x402
   payments that already landed: each receipt is `(round, shard, amount, payer, nonce)`; the
   contract refuses a reused `(payer, nonce)`, an authorization the token does not report as used,
   and any total above the unallocated balance (received minus attributed). (b) `pay(round,
   shard, amount)`: a direct on-chain payment, split at once, no reporter involved.
3. **Recipients are fixed from the ledger, once.** `register(round, shard, fields)` is
   permissionless and only for a final shard (`isFinal`); `fields` must hash to the ledger's final
   binding, which proves the label set the shard pinned. Miners: every reveal equal to the final
   root (each once). Validators: round members with at least one Agree, only if no case decided
   the shard (`closed` false: it finalized unanimous and undisputed, so every Agree was right); a
   case pays its own panel. The label share goes to the payee the reporter named for that label
   set hash before registration. Registration also records, per address, the block of its newest
   accepted work (`lastAccepted`).
4. **Split** (R-REV-2): shares in basis points, default 5,000 / 2,000 / 1,000 / 2,000, fixed at
   deploy. A role with nobody to pay passes its cut on (validators and labels to the miners, the
   miners' cut to the treasury); each head gets its role's cut divided by the heads, rounded down;
   every remainder goes to the treasury, so the parts always sum to the fee. Rust
   (`revenue::split`) and Solidity (`OnyxRevenue.split`) share vectors. Payment is pull only:
   `claim(round, shard, who)` (anyone may call) moves earnings into a balance; `withdraw()` pays it.
5. **Contributor pass.** A request may carry `ONYX-CONTRIBUTOR`: an EIP-191 signature of
   `onyx-contributor-v1 / resource / issued / ledger` by an address with accepted work in the
   window. The server checks the signature, the exact canonical URL, the ledger, `issued` within
   120 s (30 s ahead at most), single use, a budget of 60 reads per address per minute, and the
   status: on chain `OnyxRevenue.lastAccepted` fewer than 1,296,000 blocks behind (about 30 days
   of 2 s blocks); off-chain an indexer-fed list with a 30-day window. Default discount: 100%
   (free). A free read is `private`, settles nothing and leaves no receipt. Bad proof or no
   accepted work: 403 (`invalid_contributor_proof`, `not_a_contributor`); budget spent: 429.
6. **Dataset prices for agents.** An export costs
   `min(rows * per_row, max(flat, min(rows, floor_rows) * per_row))` with the decayed per-row
   price: never more than buying each row, never less than `floor_rows` rows, free once old.
   Defaults: flat 20,000 micro-USDC (2 cents), floor 4 rows. Recommended per-answer price: 2,500
   micro-USDC (0.25 cents). Without a dataset policy an export is still priced per row.
7. **Receipts in the API.** Each settled payment records one receipt for the shard actually
   served (a metric row from an older shard bills that shard); a replayed answer records none.
   The operator drains them (`ApiState.receipts`) and posts batches of at most 64.

## Consequences
- Trust: the reporter cannot mint or double-count, but it decides which shard a payment is
  attributed to (THREATS REV-1), and it names label payees. Receipts are public events; anyone can
  match them to x402 settlement transactions. Removing that trust needs Onyx to settle x402 itself
  into `OnyxRevenue` with the shard in the call: done in ADR-0026 (with `transferWithAuthorization`,
  since x402 clients sign that type, not `ReceiveWithAuthorization`); the shard choice remains.
- A shard must be final and registered before fees attributed to it can be credited; until then
  they wait in the unallocated balance. A payment never attributed stays there (no sweep yet).
- The contributor pass gives a miner who runs several bonded addresses several budgets; each needs
  a bond and accepted work, and each is rate limited (THREATS REV-3).
- `OnyxSettlementExt` gains one reader (`revealsOf`); `OnyxSettlement` is unchanged.
- The pricing rule "an export costs as much as each row" now holds only without a dataset policy.

## Amendment (review REV-L1, loop 2)
1. **Contributor checks never touch shared limiter state before the status is known (F1).** The
   signature is checked locally, then the sender's own per-IP budget (`checks_per_minute`, 120) is
   charged, then the source is asked (a confirmed status is reused for 60 s). Read budgets live in
   their own table that only confirmed contributors enter.
2. **One signed message is one proof (F2).** Canonical signatures only (v 27/28, low s); the
   replay key is keccak(address, digest). The used-proof table is per replica (accepted).
3. **The label share follows the label set, not a snapshot (F3).** A shard records the label set
   it pinned; each fee's label share goes to that set's payee at that time, or is held for the
   set and paid to the first payee named. Item 3's "payee named before registration" is replaced.
4. **Receipt checks are total-capped only (F4).** Item 2's checks prove a payment was made and cap
   the total, not that it paid Onyx or how much: attribution is the reporter's (REV-1). The owner,
   which is the treasury, may replace the reporter (`setReporter`); deploy with a treasury key
   other than the reporter's. Self-settled x402 stays the trustless fix (TODO M8).
5. **No pass for exports, and a daily row cap (F5).** A proof is a bearer token its signer could
   hand to anyone, so it never buys a whole shard: on `/v1/export` the header is ignored. Free
   reads are capped at `rows_per_day` (5,000) per address as well as 60 per minute.
6. **A bad receipt never sinks a batch (F6).** `credit` skips a failing receipt with
   `ReceiptSkipped(why)` and returns how many applied; the API keeps a batch until it is
   committed after a successful transaction (`ReceiptLog::peek` / `commit`).

## Amendment (review REV-L2, loop 3)
1. **A used proof costs its signer nothing (F1).** Whether a proof was used is checked before any
   budget is charged; it is marked used right after its read is charged.
2. **Contributor-source load has a ceiling (F2).** A "not a contributor" answer is reused for
   60 s; every other lookup takes one unit of a server-wide budget (`source_calls_per_minute`,
   600; 503 `contributor_checks_busy` beyond); `checks_per_minute` defaults to 30, the payment
   attempt budget; `ChainContributors` reuses the block number for 2 s.
3. **Held label shares and owner trust (F3).** The owner rotates the reporter, and the reporter
   names label payees, so the owner can steer held and future label shares (credited claims never
   move). Recorded under REV-1. No sweep of held shares: it would add an owner power over money owed
   to labelers; it stays an open question in the spec.
4. **The daily row cap is a fixed 24 h window, per replica (F4)**, as built; the spec now says so.
5. **Skipped receipts that can clear are posted again (F5).** `ReceiptLog::commit_with_retry`
   puts back receipts skipped with why 2 (payment not seen as used yet) or 4 (shard not registered
   yet), at most 8 posts each; then they are dropped and counted.

## Alternatives considered
- A token with fee rebates (The Graph, Ocean): rejected for now (option A: USDC only).
- Splitting at credit time into every balance: O(heads) per receipt; pull claims keep `credit`
  O(receipts).
- Operator batch payouts off-chain: no on-chain record of who earned what; rejected.
