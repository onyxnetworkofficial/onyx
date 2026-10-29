# ADR-0028: Proof level 3, source-signed data (and zkTLS deferred)

## Status
Accepted, 2026-10-10 (loop 62; the loop decides, per the owner's instruction to keep the project
moving without consulting him). Implemented in `crates/onyx-core/src/signed.rs` (types),
`crates/onyx-coordinator/src/signed.rs` (key registry, verification, intake) and
`adapters/source-signed` (example source on synthetic fixtures). Spec: docs/spec/proof-ladder.md
"Level 3". Research: RESEARCH.md "Proof ladder level 3". Amendment 6
(2026-10-17, ROADMAP C2) puts the books on chain: `contracts/src/OnyxSignedBook.sol`. Amendment 7
(ROADMAP C10) wires it: deploy tooling and manifest pin, coordinators posting, the API serving
level 3 only against `settleable` at a finalized block; the live deploy is the lead's.

## Context
Level 3 means "the source proves it": the publisher signs the value, or a proof of the exact
HTTPS response shows it (zkTLS). Before this ADR, level 3 existed only for labels: an entity signs
"address A is ours" (`FirstPartyAttestation`, ADR-0015). There was no lane for a source to sign
ordinary data (a reserve figure, a rate, a reading), and nothing tied a signing key to a source.
Level 3 is in the money default (`MarketPolicy::money_default`, levels 1 and 3), so whatever this
lane admits can settle markets: the key link and replay rules carry real weight.

## Decision
1. **New method `SignedSource { key, report }`**, appended as tag 6 (earlier encodings unchanged,
   pinned in fixtures/claims/vectors.json). Objective lane, level 3. `Claim::decode` refuses it
   (`NeedsSignedSourceIntake`); `validate` checks only the shape.
2. **One intake.** `SignedSources::accept_claim(claim, report, reg, now)` (and `decode_claim`) is
   the only way a `SignedSource` claim enters, like level 2's `ObservationResults::accept_claim`.
   It binds every claim field to the signed payload (value, metric, subject, source, snapshot =
   `Document { observed time, report hash }`, no label set, evidence = exactly the report hash, no
   confidence). The store is not `Clone`.
3. **What is signed.** `SignedPayload { source, key, metric, subject, value, observed_unix,
   sequence }`; the key signs, with EIP-191 `personal_sign`, the 88-byte text
   `onyx-signed-source-v1:0x<keccak256("onyx-signed-source-v1" ‖ borsh(payload))>`. A report is
   payload plus signature; `report hash = keccak256("onyx-signed-report-v1" ‖ borsh(report))`.
4. **Scheme: secp256k1 EIP-191, canonical form only** (`v` 27/28, low `s`, EIP-2), reusing the
   pinned verifier of the labels (`onyx_labels::attest::eip191_recover`, ADR-0015). Reasons: no
   new dependency; any EVM wallet or HSM can sign; the EVM verifies it with `ecrecover`, which the
   on-chain port needs. Ed25519 is a TODO for sources that cannot sign secp256k1.
5. **Key provenance.** A key is registered as `SourceKey { source, signer, provenance }`, where the
   provenance is where the source published it, under its own origin: `https://<origin>/.well-known/
   onyx-source-keys.json` (RFC 8615) or a TXT record at `_onyx-source-keys.<origin>`, with the
   Keccak of the fetched bytes and the fetch time. The source must be in the registry (its rights
   reviewed). All keys of one source share one origin (`OriginMismatch` otherwise). The intake does
   not re-fetch the web (it must be deterministic): the provenance record is the reviewer's
   evidence that the origin, and so the key, belongs to the publisher. Key id =
   `keccak256("onyx-source-key-v1" ‖ borsh(source, signer))`.
6. **Key life cycle.** `Active`; `Retired { at }` (rotation: reports observed before `at` still
   count); `Compromised { since }` (nothing signed by it counts any more, whatever its stated time,
   because a thief can backdate; `mark_compromised` returns accepted reports observed or accepted at
   or after `since` for re-review); `Suspended` (after an equivocation, until reviewed).
7. **Replay and equivocation.** Each key's `sequence` is strictly increasing over accepted reports
   (an exact replay, an older sequence or a second signature over the same payload is `Replay`).
   A different payload under a used sequence, or a different value for a fact the source already
   stated (`(source, metric, subject, observed_unix)`, across all its keys), is an equivocation:
   both signed reports are kept as evidence and the key is suspended. Reports must be at most
   7 days old and at most 300 s in the future at intake.
8. **zkTLS (lane b): design only.** Interface reserved (a future `VerificationMethod` variant
   carrying a proof of the HTTPS response, accepted through its own intake that binds the server
   name to the source's origin). Not built: TLSNotary, the most mature open implementation found,
   is MIT or Apache-2.0 but says it must not be used in production and supports TLS 1.2 only;
   DECO has no open verifier. Adopt when a verifier is (a) a non-alpha release, (b) supports
   TLS 1.3, (c) has a public audit, (d) is MIT/Apache-compatible, and (e) its Notary trust can be
   made Onyx-bonded (several Notaries, or Notaries drawn and slashed like panels).

## Amendment 1 (loop 63, review L3R)
The first review found four blocking holes in item 6 and 7, all in the suspension logic. The fact
book is redesigned as one model (spec R-SIG-3, 5, 6, 6a):
1. **Retired keys have a grace period.** A retired key's reports (observed before `at`) count only
   while `now < at + 1 h`. The signer sets `observed_unix`, so without an acceptance bound a
   retired key stolen later could sign "old" facts for the whole 7-day window (L3R-F1).
2. **Same value is a restatement, not an equivocation** (L3R-F2): re-publishing, or two keys of
   one source signing the same value during a rotation, suspends nobody.
3. **A contradiction disputes the fact and suspends every key involved** (L3R-F3): the first
   report is withdrawn, the fact holds no value until a reviewer resolves it (`resolve_fact`), and
   both signers are suspended until reinstated (`reinstate_key`) or marked compromised. Before,
   only the second key was suspended, so a thief stating a lie first got the honest key suspended.
4. **A compromise withdraws what the key stated** (L3R-F4) from the compromise point on, including
   its part in disputes, so the source's new key can restate the truth.
5. **No high-water mark** (L3R-F5): the never-pruned `(key, sequence)` book and the 7-day window are
   the replay guard; reports may arrive in any order, so nobody censors earlier reports by
   delivering a later one first. The fact book runs before acceptance (L3R-F6), and `u64::MAX` is
   reserved (L3R-F8).
6. **One source per origin** (L3R-F7): a second source cannot register keys under an origin that
   already publishes another source's keys.

## Amendment 2 (loop 64, review L3R2)
Review 2 found that `reinstate_key` turned a suspended *retired* key back into an active one
(L3R2-F1): the life cycle and the suspension were one enum, so clearing one reset the other. This
is the second review on the "a stolen or retired key still signs accepted data" family, so the key
model is redefined as two independent parts and proven by an exhaustive test:

1. **Life cycle, forward only.** `Active` -> `Retired { at }` -> `Compromised { since }`; any
   state may jump to `Compromised`. Nothing moves it back:

   | State \ event | retire (reviewer) | compromise (reviewer) | equivocation | reinstate (reviewer) | report signed by it |
   |---|---|---|---|---|---|
   | Active | -> Retired `at` (`at <= now`) | -> Compromised | suspension flag set | clears the flag only | accepted unless suspended |
   | Retired `at` | refused (`NotActive`) | -> Compromised | flag set | clears the flag only | accepted only if observed in `[at - 1 h, at)`, arriving before `at + 1 h`, and not suspended |
   | Compromised `since` | refused | `since` may only move earlier | flag set | clears the flag only | never |

2. **Suspension is a flag with causes.** An equivocation suspends every key involved and records
   the cause (the disputed fact, or the key's own sequence equivocation). Reinstating clears the
   flag and never touches the life cycle. The invariant, checked by
   `level3_lifecycle::every_state_and_event_pair` (every sequence of up to 5 of the events
   fresh report, backdated report, retire, compromise, contradiction, reinstate, time passing,
   against a reference model, all 56 state/event pairs reached): a report is accepted only if its
   key's life cycle permits it at the report's observation time and at arrival and the key is not
   suspended, and no sequence of reviewer actions re-enables a retired or compromised key.
3. **Reviewer capability and decision log** (L3R2-F2, F6). `retire_key`, `mark_compromised`,
   `reinstate_key` and `resolve_fact` need the `Reviewer` issued with the store
   (`SignedSources::with_reviewer`; not `Clone`, refused by any other store) and append to a
   read-only decision log (who, what, when, reason). `retire_key` and `mark_compromised` refuse
   times after `now`; `since = 0` is allowed and logged. `resolve_fact` keeps only a report whose
   key may sign it now (life cycle and no suspension).
4. **Automatic recovery after a theft** (L3R2-F3). When `mark_compromised` removes a key's reports
   from a dispute and the remaining reports agree, the fact resolves to them and their keys lose
   that fact as a suspension cause; a key with no cause left is reinstated. Both are logged as
   `auto`. A key that contradicted itself (sequence equivocation, or two values in one dispute)
   keeps its suspension until a reviewer acts.
5. **Retired window both ways** (L3R2-F4): observed in `[at - 1 h, at)`, so a retired key inside
   its grace cannot backdate a week.
6. **Bounded books** (L3R2-F5): restatements are not stored; sequence and report entries observed
   more than 7 days + 300 s ago are pruned at each intake (any report that old is refused as
   stale), except reports in an open dispute or on the re-review list. Accepted reports live on in
   the accepted map.
7. **Settlement quiet period** (L3R2-F7): the intake returns the fact and report ids; an
   equivocation returns the accepted reports it withdrew; consumers settle only on
   `settleable(fact, now)`: accepted and standing for `DISPUTE_WINDOW_SECS` (1 h).

## Amendment 3 (loop 65, review L3R3; q17 (a): simplify)
Review 3 found the third key-misuse hole in a row, this time in the automatic recovery of
amendment 2 item 4: after the other side of a dispute was compromised, the store reinstated a key
that had contradicted itself inside the dispute, freeing a stolen key with no reviewer (L3R3-F1),
and re-accepted reports a reviewer would refuse (L3R3-F3). The owner chose (a): remove the
automatic path rather than patch it.
1. **No automatic recovery** (supersedes amendment 2 item 4). `mark_compromised` withdraws what
   the key stated from `since` on (accepted reports to re-review, its reports out of disputes; a
   dispute left empty is cleared so the source can state the fact again) and restores nothing: a
   dispute left with reports stays disputed and every suspension stays. Only the reviewer clears
   them, with `resolve_fact` (same rule as before: the kept report's key may sign it now) and
   `reinstate_key`. The decision log holds reviewer decisions only.
2. **A contradicting joiner is suspended** (L3R3-F2). A key that joins an open dispute with a value
   differing from any report in it is suspended over that fact, and the pair is kept as
   equivocation evidence; a joiner agreeing with every report is only recorded.
3. **Registering a key is a reviewer action** (L3R3-F4): `register_key(by, key, reg, now, reason)`
   needs the store's `Reviewer` and is logged (`Register`).
4. **Invariants, checked exhaustively** (L3R3-F5). Three keys, one shared fact plus backdated facts,
   22 events (each key signs either of two values, signs a backdated fact, is retired, compromised
   or reinstated; the reviewer resolves keeping the first, the last or no report; time advances):
   every sequence of four events up to renaming the keys (44,128 paths) and 3,000 seeded sequences
   of twelve keep (1) a report is accepted only if its key's life cycle permits it and it was not
   suspended, (2) no suspension clears and no dispute becomes accepted without a reviewer action,
   (3) the store's life cycle equals a forward-only model, so no retired or compromised key is
   re-enabled. Re-adding an automatic reinstatement makes both runs fail.
5. **Bounds stated** (L3R3-F6). Reports held past the window as evidence (open dispute or
   re-review list) are released by the next sweep after their dispute or review ends. The fact
   book and accepted map hold one entry per accepted fact (the data set itself); the re-review list
   grows only with compromises and equivocations. Pruning and staleness assume the coordinator clock
   never steps back by more than 300 s (TODO with L3R-F8: a chain-anchored clock).

## Amendment 4 (loop 66, review L3R4; q20 (a): narrow fix)
Review 4 found the key-misuse family closed per key id but not per signing key: one private key
registered under two sources stayed active under one after a compromise under the other
(L3R4-F1), and the model could not see it because it read suspension from the store, compromised
only from time 0 and had one source (L3R4-F3). The loop chose (a), a narrow fix.
1. **One signer, one registration, ever** (L3R4-F1). A signer -> key-id index refuses
   `SignerTaken` for a signer already registered under any source, in any life-cycle state, so a
   compromise covers the private key everywhere.
2. **A recorded payload is a replay** (L3R4-F2). Any payload the store has recorded (accepted,
   evidence, either side of an equivocation), under any signature, is `Replay` and changes
   nothing; the payload book is pruned with the report book.
3. **No dispute ends without a reviewer** (L3R4 info). A compromise removes the key's reports from
   open disputes but leaves the dispute open, even empty, until `resolve_fact`.
4. **Independent model** (L3R4-F3). The multi-key test now keeps its own life cycles, suspensions,
   fact, sequence and payload books, accepted set, re-review list and pruning, written from the
   spec, and requires the store's exact results and state after every event. Events add sequence
   equivocation, compromise from now, a second source offered the same signer, relayed replays
   and time past the pruning window; all 4-event sequences (up to key renaming) and 4,000 random
   14-event sequences. Mutation-checked: dropping the first signer's suspension (an L3R-F3
   regression), re-allowing cross-source registration, or removing the replay guard each fail.

## Amendment 5 (loop 67, review L3R5: key-misuse family closed)
1. **Retirement withdraws what it no longer covers (L3R5-F1).** A report accepted while its key
   was active but observed at or after the retirement point `at` (dated ahead by up to the clock
   skew, or under a retroactive `at`) stops counting when the key retires: it goes to the re-review
   list and its fact is cleared so the new key can state it. Reports observed before `at` keep
   counting. The decision log's `Retire` entry lists what it withdrew.
2. **A sequence equivocation also meets the fact book (L3R5-F2).** The second payload under a
   used `(key, sequence)` is never accepted, but it is checked against the fact book on its own
   fact: a different value from another key's accepted report disputes that fact and suspends
   that key too (every key involved, as R-SIG-6); a dispute it is not in takes it as evidence; the
   same value or an unknown fact changes nothing more. The first report's fact is disputed whenever
   that report is withdrawn (with both reports when they state the same fact): the spec text now
   says so explicitly.
3. **The model follows the spec, and reaches more (L3R5-F3).** The reference model's sequence
   branch is written from the spec text above. New events: reports dated ahead of now (within the
   skew), retirements with a past point, compromises from an arbitrary time, sequence
   equivocations on the shared fact, and a backdated fact shared between keys. Runs: every
   4-event sequence of the base set, every 3-event sequence of the full set (up to key renaming),
   and 6,000 random 14-event sequences of the full set. Mutation-checked: dropping the first
   signer's suspension, re-allowing cross-source registration, removing the replay guard,
   keeping late reports on retirement, and skipping the fact book on the sequence path each fail.

## Amendment 6 (2026-10-17, ROADMAP C2: books on chain, chain clock; THREATS L3-F7, L3R-F8)
One coordinator's `SignedSources` could not see what another accepted: a second coordinator could
take a replay or the other side of an equivocation (L3-F7). Every time rule took the caller's
`now` (L3R-F8). Research: RESEARCH.md "Level 3 books on chain".
1. **One book on chain.** New contract `OnyxSignedBook` (no proxy, immutable `reviewer`) holds the
   key registry (R-SIG-1), the `(key, sequence)` and payload books (R-SIG-5) and the fact book
   (R-SIG-6) with the same rules and refusals as the store. Anyone may relay a report
   (`postReport`); only a registered key's holder can produce one. Spec "Level 3 on chain"
   (R-SIG-ON-1..6). What a source signs names no chain id and no book: a report is a statement
   that also verifies off chain, so it is valid in every book that registered its key, and a key
   keeps one sequence space everywhere (review C2-R2, THREATS L3C-11).
2. **The EVM hashes Borsh here (exception to ADR-0011).** Sources sign the digest of the Borsh
   payload (item 3), so the contract hashes the bytes they signed rather than a second packing,
   and parses their layout, which has one reading: two length-prefixed strings around a 32-byte
   key, a subject tag and string, a value checked to its variant's exact length, two trailing u64.
   A new packing would have changed what every source signs. Rust vectors pin digest, key id, fact
   key, value hash and report hash for every subject and value variant, and 12 malformed payloads. The contract
   does not check UTF-8 or the registry's metric and subject rules, so any validly signed payload
   burns its `(key, sequence)` on chain, even one the off-chain intake refuses: sources must never
   re-sign under a used sequence (review C2-R1 finding 1, THREATS L3C-1).
3. **No high-water mark.** The request named "the highest accepted sequence per key"; that is the
   mark L3R-F5 removed (whoever holds a later report censors earlier ones). The chain keeps the
   `(key, sequence) -> payload` book instead, never pruned, the Permit2 "unordered nonce" shape:
   a used sequence stays used whatever the order or the time, so a reuse after the 7-day window
   is still an equivocation.
4. **Withdrawals read lazily** (R-SIG-ON-3). `retireKey` and `markCompromised` are O(1); what they
   withdraw is decided when read, from the key's retirement point and compromise point. The eager
   store and the lazy contract give the same result, key and fact states on 160 seeded
   traces of 20 events replayed in Foundry (the generator asserts it reaches 18 result codes
   and enough resolutions that keep a report); the review's surviving mutants (retirement kept
   after a compromise, own-member check, resolution membership, resolution time, grace edge) are
   each killed by a named test.
5. **Chain clock** (R-SIG-ON-4). The contract uses `block.timestamp` for every time rule. On Base
   the sequencer does not choose it (OP Stack: previous plus the block time, bounded by the L1
   origin and the sequencer drift), though within that drift it can run block time up to 1800 s
   ahead of real time (review C2-R2, THREATS L3C-12). The off-chain store keeps one monotonic clock: any action with
   a `now` earlier than one it used is `ClockWentBack`, pruning with an earlier time does nothing,
   and coordinators pass the latest block's timestamp. This closes the step-back replay left open
   in amendment 3 item 5 (L3R3-F6, L3R4-F5).
6. **Who settles on what.** A consumer reads `settleable(fact)` at a finalized block and compares
   its report and value hashes with the claim's; one coordinator's store is a pre-check only. No
   Onyx contract reads the book and the money default is unchanged: nothing here lets a level-3
   fact settle money it could not before (settlement unwind stays ROADMAP H6).
7. **Not done here** (ROADMAP C10): deploying the book (a new contract: lead only),
   coordinators posting every report they receive and the API serving level 3 against
   `settleable`, a reviewer multisig (H1), and censorship by the sequencer past the dispute window
   (THREATS L3C-2).

## Amendment 7 (2026-10-17, ROADMAP C10: the book wired; THREATS C10-1..C10-8)
1. **Deploy.** `DeploySignedBook.s.sol` and `scripts/deploy-signed-book.sh` deploy the book on its
   own, next to the running ledger (it has no link to the settlement). The reviewer is never the
   deployer (the settlement's coordinator): refused by the script's dry run, by the constructor
   (`ReviewerIsDeployer`, a contract change made before any deployment) and by the node's start
   check. The manifest pins address, runtime code hash (only after a build check with the
   `reviewer` immutable masked, as the registry pin L16) and reviewer. The live Base Sepolia
   deploy is the lead's (docs/ops/runbooks/deploy-signed-book.md); rehearsed on a local anvil
   and an anvil fork of Base Sepolia.
2. **Coordinators post every report they take** (`signed_intake::BookIntake`), whatever their
   own store says, and run the store on the latest block's timestamp, never back, refusing a
   chain clock beyond the sequencer drift plus skew ahead of the host or five minutes behind.
3. **The API serves level 3 only against `settleable` at a finalized block** (`signed_serve`,
   `GET /v1/signed/{fact}`), every read pinned to that block's hash, with the serving margin,
   the hold on facts with pending posts and the delayed-post breaker of spec R-SIG-ON-7.
4. **Censorship policy** (L3C-2, L3C-12): window measured with a 1,800 s margin; a fact's window
   extended while any of its posts known to the operator has not landed and become final; level
   3 stopped when a post is pending past 600 s. Fresh review C10-R1 added two: the margin also
   runs on the host clock since the report was taken (block time runs fast after a sequencer
   stall), and a contradiction refused only for its time is retried and holds the fact. Review
   C10-R2 added: a coordinator refuses a payload over 4,096 bytes before posting (one source's
   unsendable report would otherwise trip the global breaker for good), the API stops when its
   clock runs more than 300 s behind the coordinator's, and the margin's arithmetic is in spec
   R-SIG-ON-7 (a). The rest (contradictions censored that the
   operator never saw, forced inclusion in hours) is ROADMAP H6, before any level-3 money.
5. **Money unchanged.** The route is free, not priced and not part of `/v1/settlement`; no
   market policy, catalog level or settlement path reads it (fenced in check-boundaries).
   `MarketPolicy::money_default` (levels 1 and 3) is unchanged; a catalog shard is level 3
   only through its label set's level or a registry attestation, never through this book.
6. **Reviewer multisig** stays ROADMAP H1: the manifest names one reviewer address, so a
   multisig or timelock contract address drops in without code change; the node checks only
   `reviewer()` equality.

## Amendment 8 (2026-10-18, ROADMAP C61, C62: the client's check and level-3 monitoring)
1. **Clients check a level-3 answer themselves** (spec R-SIG-CL-1): the browser's verifier
   (`apps/web/verify/src/signed_view.rs`, usable from Rust) recomputes every hash and the
   signer's key id from the signed payload and compares them with `settleable` and `factView`
   read through the client's own RPC at its finalized block, with the code at the book's address
   hashed against the pin, the block fresh (3,600 s) and the 5,400 s margin on the block and on
   the client's clock. The body's word is never used. Its mirrors (fact key, value hash, key id,
   margin, body keys) are pinned by native tests to the coordinator's and the API's own code,
   and the shipped rehearsal body is byte for byte what `signed_serve` writes.
2. **The website shows the recorded rehearsal** until a live book is deployed: one answer from
   `scripts/level3-book.sh local` with the chain answers recorded at the same moment, pinned by
   digest in the verifier, labelled a rehearsal (chain 31337) everywhere. The reader may read the
   chain through an RPC they choose (a public Base Sepolia RPC by default; https, or http on
   127.0.0.1; no key, no credentials, only on a click); against the rehearsal pin it is refused on
   the chain id. No live address or number is shown.
3. **Public builds show it** (decision 2026-10-18, TRACKER): a signed fact is free (no level-2
   style paid value is given away), derives from no entity label set (the L14 fallback has
   nothing to hold back), and the rehearsal's source is an invented test source with test keys.
   A real source's answer needs its rights review first (THREATS L3-F8) and a new pin.
4. **`/v1/health` reports the level-3 post log** (`level3`: pending, refused and retrying
   counts, the oldest pending post's age on the node's own clock, the breaker and the stop a
   request would get, from one shared function), and the health watch alerts at 300 s pending,
   before the 600 s breaker with room for its slowest runs (the config refuses a threshold past
   385 s: the breaker less a 90 s `deadline_s` run, the timer's 60 s and 5 s accuracy, and one
   more 60 s run, review C61-R2-1; the watch checks the node's `breakerSecs` the same way), plus stopped, new refusals and
   retrying refusals.

## Consequences
- Any source that will sign its own data can enter Onyx at level 3 and settle money markets by
  default, without observers.
- The weak point moves from "is the majority honest" (level 2) to "is the key the source's and is
  it safe": a stolen key signs anything until marked compromised. Bounded by the 7-day intake
  window, equivocation suspension and the re-review list; not removed (THREATS L3-*).
- Across coordinators the chain book enforces sequences and facts (amendment 6); one coordinator's
  store is its pre-check.
- No real source is wired in: each needs a rights review first (hard limit).

## Alternatives considered
- **Reuse `FirstPartyAttestation`.** It is the subjective label lane (an entity speaking about
  itself, with a confidence); ordinary source data is objective and needs a key registry and
  replay rules the labels do not have.
- **JWS or RFC 9421 HTTP signatures as the signed format.** Adds JSON or HTTP canonicalization to
  verify; the fixed text over a Borsh digest is simpler and matches Onyx encoding rules. A source
  that already signs JWS can sign the Onyx text with the same secp256k1 key.
- **Monotonic time instead of a sequence.** Two values at one second would collide; an explicit
  sequence lets the source order its own reports and makes equivocation provable.
