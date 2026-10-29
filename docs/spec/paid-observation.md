# Spec: observe on the first paid query (NORTH-STAR NS10, north-star loop NL8, ROADMAP S9)

Status: **Slice 1 built 2026-10-27** (ADR-0049: `onyx_api::observe`, prepaid holds,
`/v1/tickets/{ticket}`, `ONYX_OBSERVE` in `onyx-node`). Drafted by lane B the same day. Prior
art: RESEARCH.md "Lane B: paid observation, storage tiers, browser TLS prover (2026-10-27)".

What the build changed from the draft (ADR-0049 says why):
- **M = 1:** one hold per buyer, and no other voucher of that buyer while it is pending
  (cumulative vouchers cannot carry a later charge past a released hold).
- **The second buyer** of a queued fact gets a 202 with the state and **no ticket** (the ticket
  is the payer's secret), not "the same ticket's state".
- **Approval** is the template's registration: every subject the pinned dated template allows a
  round for now gets the offer. The submitter's "observe now" is the same offer on its subject;
  the receipt-page button is not built yet.
- **A round served and then refused by a client** is not refunded (R-OBS-6's last case): the
  server's own check (`ObservationFeed::add`) runs before the charge, as at a start.
- **Not built:** the health watch alert on an old queue, a node rehearsal with a real round
  command against an anvil `OnyxPrepaid`.

## Scope

Today an approved submission is fed to observers by hand (`feed-approved`, PP9) and every fed
value costs the network a level-2 round whether anyone ever buys it. NS10 changes the trigger:

- a **proposal is free** (the five questions, the checks and the review of ADR-0038 stay as they
  are);
- **observers run only when someone pays**: a buyer's first paid query for a value nobody has
  observed yet, or the submitter's small fee to have it observed now;
- spam costs the spammer, not the network, and there is still no gate on who may propose.

Out of scope: the price itself (docs/spec/pricing.md), token emission (NS12, waits for the
partners), level 3 and the keyed path (NL11).

## Interfaces

- **States of a fact** (one approved proposal and one subject, e.g. a day): `proposed` (approved,
  never observed), `queued` (paid for, round not final), `observed` (a final round, served like
  the B6 values), `failed` (the round did not finalize; the money goes back).
- **Buyer path.** `GET /v1/observations/{metric}?subject=...` for a `proposed` fact answers
  `402` with an `observe` offer: the price of the round plus the first answer. Only a prepaid
  voucher (ADR-0046, `ONYX-PREPAID-VOUCHER`) can take it, because the answer comes later than the
  HTTP request: the node **holds** the amount on the balance, answers `202` with a ticket
  (`{ticket, state: "queued", retryAfter}`), and starts the round. `GET /v1/tickets/{ticket}`
  answers the state; once `observed`, the held amount is **charged** and the value is served with
  its ladder (NL2); on `failed` or past a deadline, the hold is **released**. x402 stays for facts
  already `observed` (pay, then served at once), because x402 settles before the answer.
- **Submitter path.** The receipt page offers "observe now" for the submitter's own approved
  proposal: the same hold, round and release, paid from the submitter's prepaid balance. The fee
  is the round's cost; the first buyer later pays the normal per-query price.
- **Coordinator.** A queue of paid facts, oldest first, at most N rounds in flight; each round is
  the `onyx_level2::live` coordinator of B6 for the fact's source and subject.
- **Health.** `/v1/health` gains `observe: {queued, oldestQueuedSecs, failed}`; the watch alerts on
  a queue older than the round deadline.

## Rules

1. **R-OBS-1.** No observer runs for a fact without a hold or a charge behind it (a test: a
   `proposed` fact with no payment starts no round).
2. **R-OBS-2.** A hold is charged only when the fact's round is final and its value is served; a
   failed or late round releases the hold in full (tests for each).
3. **R-OBS-3.** One round per fact: a second buyer of a `queued` fact gets the same ticket's
   state and is not charged for a second round; the first payer's hold covers the round.
4. **R-OBS-4.** The fact and subject are fixed before payment: the subject rule
   (`SubjectRule::Daily` etc.) must accept the subject, so a buyer cannot pay for a day the
   template refuses, and the coordinator cannot swap the day afterwards (B6-3).
5. **R-OBS-5.** A ticket names nothing about the buyer; its id is random and unlinkable to the
   voucher (privacy of what a buyer asked, the bounty board rule of NS15 applies to queues too).
6. **R-OBS-6.** The refund rule is public: the held amount goes back in full on `failed`, on a
   deadline passing, or on a round the client check refuses.
7. **R-OBS-7 (review NL8-1).** A hold is accepted only if the buyer's deposit covers the sum of all
   its pending holds plus its last redeemed total; one deposit never backs two holds.
8. **R-OBS-8 (review NL8-2).** A hold's deadline ends at most half of the prepaid `exitDelay` after
   it is taken (the redeem rule of docs/spec/prepaid.md), and the node refuses a hold from a buyer
   whose exit is pending; a round still running at the deadline is released, never charged late.
9. **R-OBS-9 (review NL8-3).** Each account may have at most M facts `queued` at once (M small,
   e.g. 3), and the queue serves accounts in turn, so one large balance cannot fill the N rounds
   in flight and starve other buyers.
10. **R-OBS-10 (review NL8-4).** What the buyer pays for is said plainly: the round, not secrecy. A
   level-2 round records its value on chain (ObservationRecorded, B6), so after the round anyone
   can read it; the offer states this, and a source whose values must stay private never takes
   this path (it is level 4 keyed, NL11).
11. **R-OBS-11 (review NL8-5).** The `observe` offer is the same for every subject the template
   accepts, approved or not, until a payment: probing subjects does not reveal which proposals
   exist; a payment for a subject with no approved proposal is refused before any hold.

## Assumptions

- A prepaid voucher (ADR-0046, docs/spec/prepaid.md) is the buyer's signature of a cumulative
  total, and the server redeems each buyer's latest one. A hold fits that shape: the server keeps
  the voucher with the higher total as pending and does not count it; on `observed` it counts it
  (charge); on `failed` it drops it and accepts the buyer's next voucher at the earlier total
  (release). The pending holds need a journal on disk, like the settle intents of OPS-L2-F1, and
  the server must never redeem a pending voucher on chain.
- The observer set is the B6 one until NS4 (independent observers) lands, so a paid round is still
  one operator's round on testnet (THREATS B6-5).
- A round's cost is known before it runs (gas of the B6 rounds: 14-15 transactions, about
  0.000012 test ETH each round on Base Sepolia on 2026-10-27).

## Open questions

1. Hold in the node's journal only (above; the buyer trusts the node not to redeem a pending
   voucher until it serves), or a hold the buyer can see on chain? Leaning: journal first, and a
   test that a pending voucher is never in a `redeem` batch.
2. Who pays when two buyers race on the same `proposed` fact? R-OBS-3 says the first hold; the
   second waits for free. Is a split fairer?
3. Does a submitter fee need a cap per address per day, or is the price enough spam control?
4. What deadline? The B6 rounds took minutes on Base Sepolia; a deadline of 1 hour with a retry
   looks safe, to be measured.

## Threats the build must test

Review NL8 (lane B, 2026-10-27): THREATS NL8-1..5.


A buyer who pays then cancels (the hold stays until final or released by rule), a node that
charges and never serves (the ticket shows `observed` only with a value whose client check
passes), a spammer with many free proposals (no round without money), a coordinator that picks a
convenient day (R-OBS-4), a queue that grows without bound (N in flight, oldest first, an alert).
