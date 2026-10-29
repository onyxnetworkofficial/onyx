# Spec: usage revenue and the contributor pass

## Scope
Where each paid call's fee goes, who may read fresh data free, and how datasets are priced for
agents. Decided in ADR-0025; built in `contracts/src/OnyxRevenue.sol`,
`crates/onyx-coordinator/src/revenue.rs` and `crates/onyx-api` (`contrib`, `receipts`,
`catalog::DatasetPricing`). Prices themselves: pricing.md.

## Interfaces
- `OnyxRevenue(token, ledger, reporter, treasury, [minersBps, validatorsBps, labelsBps, treasuryBps])`.
  - `register(round, shard, CommitmentFields)`: once per final shard, by anyone.
  - `credit(Receipt[]) -> applied` (reporter, at most 64): `Receipt { round, shard, amount, payer,
    nonce }`; a failing receipt emits `ReceiptSkipped(round, shard, payer, nonce, why)`.
  - `setReporter(reporter)` (owner = the treasury), `labelHeld(labelSetHash)`.
  - `pay(round, shard, amount)`: direct payment (ERC-20 approve first).
  - `claim(round, shard, who) -> owed`, `withdraw()`, `balanceOf(who)`, `unallocated()`.
  - `lastAccepted(who) -> block`, `setLabelPayee(labelSetHash, payee)` (reporter),
    `split(amount, miners, validators, hasLabel)`.
- Rust: `onyx_coordinator::revenue::{RevenueSplit, Recipients, Shares, split}`.
- API: request header `ONYX-CONTRIBUTOR` = base64 JSON `{address, issued, signature}`;
  `contrib::{sign, verify, message, ContributorPolicy, ContributorSource, StaticContributors,
  ChainContributors}`; `receipts::{Receipt, ReceiptLog}` (`open`, `peek`, `commit`,
  `commit_with_retry`) at `ApiState.receipts` (`ApiState::with_receipt_log`);
  `reporter::{Reporter, Confirm, Registrar, ReporterHealth, ReporterStatus, credit_calldata}`
  (`run_once`, `spawn`, `with_state_file`, `with_max_in_flight`, `with_give_up`;
  `ApiState::with_reporter`); `evm_tx::Sender::with_gap_limit` (`SendError::Stuck`);
  `receipts::{Retry, RetryPolicy}`; `journal::{Journal, Binding}`; `client::PayToPin`
  (`from_manifest`, `check_code` -> `VerifiedPin::select`); `/v1/health` adds `payTo`,
  `contributorPass`, `receiptsPending`, `receiptLog`, `receiptsDropped`, `receiptWriteErrors`,
  `pendingSettleWriteErrors`, `acceptingPayments`, `reporter` (`posts`, `applied`, `inFlight`,
  `stuckSince`, `writeErrors`, `lastError`).
- Vectors: `fixtures/revenue/vectors.json` (written by `revenue::vectors`, read by
  `OnyxRevenueUnitTest.test_SplitMatchesRustVectors`).

## Rules
- R-REV-1 Recipients: fixed at registration of a final shard whose binding matches. Miners whose
  reveal equals the final root; validators with an Agree only when no case decided the shard; the
  label set the shard pinned (its payee is read when each fee is split, REV-L1-F3).
- R-REV-2 Split: `cut = amount * bps / 10000` per role, rounded down. No validators: their cut
  goes to the miners. No label set: the label cut goes to the miners. A label set with no payee
  yet: its cut is held for the set (`labelHeld`) and paid to the first payee named. No miners: the
  miners' cut goes to the treasury. Per head = cut / heads, rounded down. Treasury = amount minus
  everything paid to heads and the label share.
- R-REV-3 Receipts: each `(payer, nonce)` once; the token must report the authorization used;
  the batch total never exceeds `unallocated()`; the shard must be registered. These checks are
  total-capped only: `authorizationState` binds neither recipient nor amount, so which shard a
  payment bought is the reporter's word (REV-1, REV-L1-F4). A receipt failing a check is skipped
  with an event (why: 1 reused, 2 not reported used, 3 over the unallocated balance, 4 shard not
  registered), never reverting the batch; the reporter `peek`s a batch and `commit`s it only
  after the transaction succeeded (REV-L1-F6), putting back those skipped with why 2 or 4 (they
  can clear) for at most 8 posts in all (`commit_with_retry`, REV-L2-F5). The owner (the
  treasury) may replace the reporter, and the reporter names label payees, so held and future
  label shares follow whoever the owner trusts; credited miner and validator claims never move
  (REV-1, REV-L2-F3).
- R-REV-4 Pull payments only; claims are idempotent; any address may trigger a claim for anyone.
- R-REV-5 Contributor pass: signature by the named address over
  `onyx-contributor-v1\nresource: <canonical URL>\nissued: <unix>\nledger: <off-chain | eip155:<chain>:<contract>>`;
  issued within `max_skew_secs` (120) and at most 30 s in the future; a canonical signature (v 27
  or 28, low s); each proof once, keyed by keccak(address, digest) (REV-L1-F2; the used-proof table
  is per replica, so behind N replicas a proof can be honoured up to N times inside its window);
  accepted work within the window. Order (REV-L1-F1, REV-L2-F1/F2): signature; a used proof is
  refused before anything is charged; the sending client's `checks_per_minute` (30, per IP scope,
  at most the payment-attempt budget); an address the source said is not a contributor less than
  60 s ago is refused from memory; otherwise one unit of the server-wide `source_calls_per_minute`
  (600; 503 `contributor_checks_busy` when spent) before the source is asked (on chain the block
  number is reused for 2 s); a confirmed status is reused for 60 s; only confirmed addresses get a
  read budget, in their own table: `reads_per_minute` (60) and `rows_per_day` (5,000, a fixed 24 h
  window from the address's first counted read). Both budgets, the negative answers and the
  used-proof table are per replica (REV-L2-F4). A pass read is `Cache-Control: private` and records no receipt.
- R-REV-7 A pass never applies to `/v1/export` (REV-L1-F5: a proof is a bearer token its signer
  could hand out): the header is ignored there and the export is paid, or free under
  `free_exports_per_minute` when old.
- R-REV-8 Self-settle (ADR-0026): `settle(round, shard, authorization, signature)`, reporter
  only, on a registered shard: the token moves the payer's EIP-3009 transfer to `OnyxRevenue`;
  exactly `value` must arrive (`NotReceived` otherwise); `(payer, nonce)` is marked used for
  `credit` as well (`AlreadyCredited` on a second settle; why 1 on a receipt); the shard is
  credited in the same call (`Settled`, `Credited`). A payment whose authorization was already
  used on chain is never reported settled by the API; it goes to the receipt path.
- R-REV-9 Receipts persist (REV-11): with a journal, a receipt is synced to disk before the paid
  answer returns (and before a waiting retry sees the payment settled), and pending self-settles
  survive a restart. A journal is bound to one chain and `OnyxRevenue`, held by one process, and
  every batch is one checksummed line (REV-L3-F4, F6). The reporter commits a batch only after its
  `credit` transaction is final (`finalized` by default) and its events read; receipts the events
  skip with why 2 or 4, or never mention, go back and wait (REV-L3-F2, F3); anything less posts
  the batch again, and `receiptUsed` on chain makes the repeat a no-op (why 1). The chain is the
  source of truth; the journal only lists what is still worth posting.
- R-REV-10 Clients pin `payTo` (REV-4): a client signs only for the `OnyxRevenue` of a deployment
  manifest it trusts (chain, contract, token and runtime code hash, all required), after its own
  node confirmed the chain id and the code (REV-L3-F9), never for whatever `payTo` a server
  offers.
- R-REV-11 Fail closed on the receipt log (REV-L3-F5): while any journal write failed, the API
  refuses new paid requests with 503 before verifying them.
- R-REV-12 Level-2 observation books (ADR-0027 amendment 4): a sale of a level-2 value is billed
  to `(OBSERVATION_ROUND = 2^64 - 1, fact)`, where `fact` is the value's on-chain fact key
  (`OnyxRegistry.factKeyOf`). The reporter opens the book once with
  `registerObservation(fact)` (reporter only; `register` refuses that round). The book has no
  miners, validators or label set, so R-REV-2 sends its whole fee to the treasury until observer
  payouts exist on chain (THREATS L2API-3). Receipts and self-settles use it like a shard book;
  a receipt skipped as unregistered (why 4) is opened by the node's registrar
  (`observations::register_observation_calldata`). In self-settle mode the book must be open
  before the first paid request, or `settle` reverts and the API answers 503 with the unit given
  back (THREATS L2API-4).
- R-REV-6 `HEAD` never uses a proof (400 before any check). A free (old) row needs no pass; a pass never makes `Restricted` or `Forbidden` data
  readable.

## Assumptions
- The x402 asset implements EIP-3009 `authorizationState` (USDC does).
- The API's state directory is on a local disk that honours `fsync` and `flock` (a receipt
  journal on a network filesystem that acknowledges unsynced writes could lose the last receipts
  in a crash, and `flock` there may lock within one host only, REV-L4-F7; the payment then waits
  in `unallocated()` until a receipt with the same nonce is posted). Only the operator can write
  it: the journal checksum detects damage, not deliberate edits (REV-L4-F4).
- 2 s Base blocks for the on-chain window (1,296,000 blocks is about 30 days).

## Open questions
- Who may name label payees once label sets have their own signing keys.
- A sweep for payments that are never attributed, and for label shares held for a set that never
  gets a payee (REV-L2-F3; not added: a sweep is a new owner power over money owed to labelers).
- Partial discounts per tier of contribution instead of one discount.
