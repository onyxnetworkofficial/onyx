# ADR-0049: Observe on the first paid query: a prepaid hold pays for a level-2 round

## Status
Accepted, 2026-10-27 (docs/NORTH-STAR.md NS10; docs/north-star-loop.md NL8; ROADMAP S9). First
slice implemented by `onyx_api::observe`, the hold, charge and release of
`onyx_api::prepaid_gate::PrepaidState` and `onyx_api::prepaid::PrepaidBook`, the `202` answer of
`/v1/observations/{metric}`, `/v1/tickets/{ticket}`, and `ONYX_OBSERVE` in `onyx-node`
(`onyx_node::observe`). Design draft: docs/spec/paid-observation.md (lane B, reviewed as THREATS
NL8-1..5).

## Context
A level-2 value costs the network a round (the B6 rounds on Base Sepolia: 14 to 15 transactions
each). Until now an operator ran rounds by hand (`feed-approved`, `scripts/level2-sepolia.sh`)
whether or not anyone wanted the value. NS10 asks that proposals stay free and that observers run
only when someone pays, so spam costs the spammer and there is still no gate on who may propose.

Two facts shape the payment:
1. The value exists only after the round, minutes after the request. x402 settles before the
   answer, so it cannot pay for a value that may never come.
2. A prepaid voucher (ADR-0046) is the buyer's signature of a cumulative total. The contract's
   `redeem` takes exactly a signed total, so the server cannot redeem part of a voucher.

## Decision
1. **A hold, not a charge.** A request for a level-2 subject no round settled yet, which the
   metric's pinned dated template allows a round for now, answers `402 observe_payment_required`
   with an `observe` offer: price, deadline, that the value becomes public on chain after the round,
   and the refund rule. Only `ONYX-PREPAID-VOUCHER` takes it. The voucher must be exactly the
   buyer's last total plus the price, within the deposit, with no exit pending; the server
   **holds** it: it journals it (owner-only, synced) with the ticket's hash and the fact, and never
   counts or redeems it. The answer is `202` with a ticket: 32 random bytes shown once.
2. **One hold per buyer.** While a buyer's hold is pending, no other voucher of that buyer is held
   or taken (`prepaid_refused`, "a hold ... is pending"). Cumulative vouchers allow nothing else:
   a second voucher must sign a total above the held one, and if the held round then failed, that
   second voucher would charge the buyer for it. It also gives R-OBS-7 (one deposit never backs two
   holds) and R-OBS-9 (one balance cannot fill the queue) without more state.
3. **One round per fact.** While a fact's round is queued or running, every other request for it
   answers `202` with the state and no ticket and is charged nothing.
4. **The worker.** `onyx_api::observe::step` releases holds past their deadline, then runs the
   oldest queued fact's round through the operator's `RoundRunner`, one round at a time. The
   transcript must be for the held subject, must check against the pinned template
   (`ObservationFeed::add`) and must arrive by the deadline. It is then appended to the metric's
   results store (atomically), added to the served catalog, and only then is the hold **charged**:
   an ordinary usage line billed to the fact's book, which the reporter redeems and allocates like
   any answer. Anything else **releases** the hold in full; the buyer's next voucher starts from the
   total before it.
5. **The ticket.** `GET /v1/tickets/{ticket}` is free and never cached: the state, and once
   observed the paid answer (the `/v1/observations` body, with its ladder). The journal keeps only
   the ticket's hash. Pending holds come back queued after a restart; ended ones still answer.
6. **The hold limit.** A hold lasts at most half the contract's `exitDelay` (`with_hold_limit`;
   `onyx-node` sets it from `OnyxPrepaid.exitDelay()`), and an offer whose deadline is longer is
   refused at start. A buyer who starts its exit after a hold cannot withdraw before the hold is
   charged and the reporter's pass (also at most half the delay) redeems it.
7. **Approval.** In this slice the approval of a fact is the registration of its template: every
   subject the pinned template allows a round for now gets the same offer (R-OBS-11), and a metric
   without an offer, or a subject the template refuses, is the usual `404 unknown_subject` before
   anything is held. A submitter who wants a submission observed now pays the same offer for that
   subject (the "submitter fee"); a receipt-page button for it is the next slice.
8. **The node.** `ONYX_OBSERVE` names a JSON file: the operator's round command and one offer per
   metric (price, deadline, the results store, which must be one of `ONYX_OBSERVATIONS` and
   writable). The command runs as `command... <metric> <subject>`, prints the path of a results
   store with the round, and is killed at the hold's deadline; the node loads that store with every
   check of a start and hands the subject's round to the worker, which checks it again. It needs
   `ONYX_PREPAID`.

## Consequences
- Proposals stay free and nothing runs without money behind it (R-OBS-1). A spammer's free
  proposals start no round.
- A buyer pays for the round, not for secrecy: the value is on chain for anyone afterwards
  (R-OBS-10). The offer says so.
- A buyer with a hold pending waits for it before paying anything else with a voucher (x402 still
  works). The deadline bounds the wait.
- A failed round is free to its buyer, so a buyer who can predict failures can make the operator
  pay gas for rounds that fail (THREATS NL8-6, TODO S9).
- The served round is checked against the template off chain when it is added, as at a start; the
  registry record of the round is checked by the client (`level2-check`), as for every level-2
  answer.
- Not in this slice: the receipt-page "observe now" button, a node rehearsal against an anvil
  `OnyxPrepaid` with a real round command (`OnyxPrepaid` is not deployed on Base Sepolia), several
  holds per buyer, a refund on a round the client check refuses after it was served.

## Alternatives
- **x402 with a refund.** The payment settles into `OnyxRevenue` first; a refund would be a second
  transfer the operator sends, trusted and gas-paid per failure. Refused.
- **Holds stacked on later vouchers.** Lets a buyer keep buying while a hold is pending, but a
  failed round would leave signed totals that include it. Refused (Decision 2).
- **An on-chain hold.** Visible to the buyer and enforced by the contract, at the price of a
  transaction per hold and a contract change. Left as the spec's open question 1.
