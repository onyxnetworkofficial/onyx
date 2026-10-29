# ADR-0026: Self-settled x402 payments into OnyxRevenue

## Status
Accepted, 2026-10-10 (loop 57, M8 live prep). Implemented in `contracts/src/OnyxRevenue.sol`
(`settle`), `crates/onyx-api/src/{evm_tx,self_settle}.rs` and the `Facilitator::settle_for` /
`credits_on_settle` hooks; tests in `contracts/test/OnyxRevenueSettle.t.sol`,
`contracts/test/OnyxRevenueFork.t.sol` (real Base Sepolia USDC bytecode, gated by
`ONYX_FORK_RPC`) and `crates/onyx-api/src/{evm_tx,self_settle}.rs`; end to end in
`scripts/demo-testnet.sh fork` (docs/ops/demo-testnet.md). Closes the TODO in ADR-0025
Consequences and THREATS REV-1 / REV-L1-F4 ("trustless attribution needs self-settled x402").
Research: RESEARCH.md "Self-settled x402".

## Context
With the public facilitator, x402 USDC lands in `OnyxRevenue` with no word about what it bought,
so the API's reporter attributes each payment afterwards (`credit`), and the contract can only
check totals: `authorizationState(payer, nonce)` binds neither recipient nor amount (REV-L1-F4).
ADR-0025 named `receiveWithAuthorization` as the fix. It does not fit x402: an x402 `exact`
client signs an EIP-712 `TransferWithAuthorization` (x402 `scheme_exact_evm.md`, "EIP-3009"
method), while `receiveWithAuthorization` checks a `ReceiveWithAuthorization` signature (a
different type hash), so a contract cannot redeem an x402 payload that way. But
`transferWithAuthorization` may be submitted by anyone, and the signature fixes `to`, `value`, the
validity window and the nonce.

## Decision
1. **`OnyxRevenue.settle(round, shard, authorization, signature)`**, reporter only. It calls the
   token's `transferWithAuthorization(from, address(this), value, validAfter, validBefore, nonce,
   signature)` (FiatToken v2.2 bytes form), checks that exactly `value` arrived (balance before and
   after), marks `(from, nonce)` used for the receipt path too, and credits `shard` in the same
   transaction (events `Settled` and `Credited`). Recipient and amount are proven by the token,
   not attested. Only the choice of shard rests on the reporter: the payer's signature says
   nothing about the shard, so a permissionless `settle` would let anyone who sees the payload
   (for example in the mempool) pick the shard; reporter-only keeps that choice where it was.
2. **The API is its own facilitator** (`self_settle::SelfSettle`). Verify: the local `exact`
   checks (amount, recipient = `OnyxRevenue`, window, signature recovers to `from`), then an
   `eth_call` of the token's `transferWithAuthorization` from the operator (the real transfer
   minus the credit; x402 exact verify step 5). Settle: `settle_for(req, (round, shard))` builds
   `OnyxRevenue.settle`, estimates gas, signs an EIP-1559 transaction in process
   (`evm_tx::Sender`: the key never leaves memory; nonces taken under a lock) and waits for the
   receipt. A self-settled payment records no usage receipt (`credits_on_settle`).
3. **Front-running and unknown outcomes.** If the authorization is already used on chain (a third
   party submitted it to the token directly, or an earlier settle of ours landed after its
   deadline), the funds are in `OnyxRevenue` unallocated, or were spent by the payer elsewhere with
   the same nonce. A receipt is queued only when the token's own logs prove the USDC reached
   `OnyxRevenue`: an `AuthorizationUsed(from, nonce)` and a `Transfer(from -> OnyxRevenue, value)`
   log in the same successful transaction (loop 58, M8-L58-F3). The server then reports the
   outcome as unknown (M7-F5: the unit stays spent, nothing is served) and `credit` applies the
   receipt total-capped or skips it (already credited, why 1). A nonce spent elsewhere is the
   payer's refusal (`invalid_transaction_state`), with no receipt. A broadcast settle with no
   receipt before the deadline is kept as pending; `resolve_pending` (run before every settle and
   by the reporter before posting) drops it once it mined successfully (it credited itself) and
   otherwise applies the same proof. The server waits at least the settle's own budget
   (`Facilitator::settle_budget`: deadline plus node-call slack), so the pending handling always
   runs (M8-L58-F2); a settle that cannot take the key within half its deadline sends nothing.
   A settle that reverts while the token would take the payment (reporter rotated, shard not
   registered) or a gas estimate that fails for a transport or funding reason is an unknown
   outcome (503), never blamed on the payer (M8-L58-F4).
4. **Both paths stay.** The public facilitator path (`HttpFacilitator` + receipts) remains for
   deployments without an operator key on chain; self-settle is configured by building the API
   with `SelfSettle` as its facilitator and its receipt log as `ApiState::receipts`.
5. **The operator pays gas** (testnet ETH, one `settle` transaction per payment); the payer still
   pays no gas.

## Consequences
- REV-1 narrows to attribution only: the reporter can still credit a payment to the wrong shard
  of a registered round, but can no longer attach someone else's payment or a different amount.
  Binding the shard cryptographically needs the client to commit to it (for example an x402
  extension or an Onyx client that derives the EIP-3009 nonce from the shard); generic x402
  clients pick random nonces, so that is left open (THREATS REV-1).
- The API holds a hot key that can move nothing but gas: `settle` only pulls a payer's signed
  transfer to `OnyxRevenue` itself. A leaked operator key lets its holder misattribute and burn
  the operator's gas; the owner rotates the reporter (REV-L1-F4).
- `OnyxRevenue` grows to 9,013 B (15,563 B margin); `OnyxSettlement` is unchanged.
- The M8 driver (`examples/demo-local.rs`) gains `--mode fork|testnet`: per-actor keys, a funding
  plan from measured gas, and the self-settle path, rehearsed on an anvil fork of Base Sepolia.

## Amendment (REV-11, REV-4)
1. **Persistent state.** `SelfSettle::persist(dir)` keeps the receipt log (`receipts.jsonl`) and
   the pending settles (`pending-settles.jsonl`) in append-only journals (`journal::Journal`: one
   synced write per change; a torn last line is cut on start, any other damage refuses the
   start). A settle broadcast just before a crash is resolved after the restart by the same
   arrival proof (M8-L58-F3); a server behind a public facilitator uses
   `ApiState::with_receipt_log`.
2. **The reporter** (`reporter::Reporter`, sharing the self-settle's sender so settles and credits
   never race for a nonce) resolves pending settles, posts up to 64 receipts per `credit`, and
   commits only after reading the mined transaction's events. Idempotence comes from the chain
   (`receiptUsed`), not from the journal: anything not committed is posted again and skipped as
   already credited.
3. **Clients pin `payTo`** (`client::PayToPin`) from the deployment manifest the deploy run
   writes (chain id, `OnyxRevenue`, USDC, runtime code hash). Trust in the manifest itself is
   THREATS REV-12.

## Amendment (review REV-L3)
1. **Journal format.** Each journal starts with a header naming its kind, chain id and contract;
   a journal of another deployment refuses to open, and drivers keep one state directory per
   `OnyxRevenue`. Each batch of changes is one line `<sum> <json>` (first 16 bytes of keccak-256
   of the JSON), so a batch is applied whole or not at all and an edited line refuses the start.
   The journal compacts itself from its own copy of the live entries, never from a caller's view.
   An exclusive lock on `<file>.lock` allows one process per journal (REV-L3-F1, F4, F6).
2. **Fail closed.** While any journal write failed, new paid requests get 503 before verify; the
   operator drains the log with the reporter before restarting (REV-L3-F5).
3. **Reporter finality and fees.** The key is held only while sending. A `credit` batch stays in
   the log until the transaction is in the `finalized` block (configurable) and still canonical;
   a stuck `credit` is replaced at the same nonce with higher fees under a cap. Receipts the
   events skip with why 2 or 4, or never mention, go back with time-based waits; a `Registrar`
   lets the reporter register a shard skipped as unregistered (REV-L3-F2, F3, F8).
4. **Pin.** The manifest's code hash is required, the client's node must report the pinned chain
   id, and only the `VerifiedPin` that `check_code` returns can select a requirement (REV-L3-F9).

## Amendment (review REV-L4)
1. The reporter keeps up to 4 `credit`s in flight; a batch mined but not yet final no longer
   holds up the next, and no receipt is in two batches at once (REV-L4-F3).
2. An unmined `credit` is re-broadcast as signed on every post. With
   `Reporter::with_state_file` the `credit`s in flight are on disk, so a restart resumes them and
   bumps the lowest unmined one at its own nonce. Past its last bump and 30 minutes it is
   reported stuck (`/v1/health` `reporter`) and new batches wait. The operator key's `Sender`
   refuses any new send (a settle answers 503 and sends nothing) while `latest` has been behind
   `pending` for over 60 s, so no payer is charged behind a stuck transaction (REV-L4-F1).
3. Retry waits never lie further than `max_wait` ahead (clock jumps, REV-L4-F2); receipts no
   event mentions wait about 20 hours over their posts (REV-L4-F5); the self-settle's own
   receipt log counts in its state write errors (REV-L4-F6); journals retry their lock for 1 s
   and need a local disk (REV-L4-F7); the journal checksum detects damage, not deliberate edits
   (REV-L4-F4).
