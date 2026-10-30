# ADR-0054: Keyed contributions with agreement: key holders form a drawn level-2 pool, and keys never leave them

## Status
Accepted, 2026-10-30; built on a local anvil (docs/contributor-loop.md CL4), with Amendment 1
below. Proposed 2026-10-29. Needs CL2 (the contributor client `onyx-contrib` and its key vault)
and ADR-0053 (the generic fetcher and `onyx-fetch`). Builds on ADR-0027 (rounds, draws, signed
commits, sealed reveals), ADR-0038 items 3 and 9 (keyed sources are level 4 and held for a rights
review) and ADR-0042 (bonds). No contract change. Not on Base Sepolia yet: no keyed provider's
terms are cleared (`OK keyed` rows), and bond age and history are the coordinator's word (TODO S2).

## Context
Many useful public series sit behind a free API key: the data is the same for everyone, the key
only meters use. Today such a source is a dead end: ADR-0038 routes it to level 4, always held, and
the proof path it named (a TLS proof made by the key holder, ROADMAP S2, NS6) is parked because
the upstream prover is still a pre-release alpha (docs/NORTH-STAR.md NS6). The contributor loop
asks for the other path: contributors who hold their own keys fetch locally and send only values;
several independent contributors who agree count like an observer round; one alone stays level 4.

A level-2 round already proves exactly "k of n drawn, bonded observers independently saw this
value at the source in this window" (ADR-0027). Nothing in that claim needs the source to be
keyless: it needs a fetch every drawn observer can make, and an answer that does not depend on
who fetched. The pool hash, the draw, commits on chain (`postCommit`, which anyone may relay with
the observer's signature), sealed reveals, receipts and `recordObservation` do not look at the
URL's access rule.

## Decision
1. **A keyed source is a manifest with `access: Keyed`.** ADR-0053's descriptor gets
   `Access::Keyed { place: Header | Query, name: String, scheme: Raw | Bearer }`, so the fetcher id
   and the on-chain template pin state that the feed is keyed and where the key goes. The template
   URL holds no key: ADR-0038's secret-parameter refusal still applies to it, and `name` is the
   only place the client inserts one.
2. **Keys never leave the contributor.** The key lives only in CL2's vault (the system keychain).
   `onyx-contrib` adds it inside `onyx-fetch` for that one request to the template's host (no
   redirects, so it is never sent anywhere else). It is never in the spec, the commit, the reveal
   (an `Observation` holds only the answer, `observed_at` and `page_hash`), a log line, a
   recording, a crash report or anything sent to Onyx. **The raw page of a keyed fetch is never
   sent or published** (a page can echo the key or the account); only its Keccak hash goes into
   the reveal, as for every round. Rehearsal recordings of keyed sources are synthetic.
3. **Agreement is an ordinary round over a keyed pool; no new lane.** Each keyed source has its own
   pool snapshot: the addresses that took a keyed seat for that source (item 5). The coordinator
   draws from it, and everything after the draw is unchanged: `announceDraw` commits to the pool
   hash, observers post commits, reveal sealed, get receipts, and `recordObservation` records the
   result. `OnyxRegistry.observationLevel` answers 2 for an agreed keyed fact. No contract
   changes in v1. The level number stays 2 because the claim is the same; the badge and the ladder
   evidence add "re-observed by key holders" (read from the template, so it cannot be dropped).
4. **One alone stays level 4.** A single key holder's value (CL2's "contribute") is a level-4
   claim shown as "stated by one key holder". A round with fewer agreeing reveals than a strict
   majority of the drawn observers is `NoResult` (the existing quorum over assigned observers). A
   keyed template needs `min_observers >= 3` and a pool of at least `KEYED_MIN_POOL` seats
   (proposed 9, the size of the rehearsal pool, ADR-0027 amendment 6 item 10) before the owner
   registers it.
5. **A keyed seat** is a signed, bonded statement, never anything about the key:
   ```text
   KeyedSeat { observer: Address, source: SourceId, operator: Option<DottedId>,
               terms_hash: Hash32, joined_at: u64, sig: [u8; 65] }
   message (EIP-191): "Onyx keyed seat v1\nsource: {id}\nobserver: {0x address}\n
                       terms: {terms_hash}\njoined: {joined_at}"
   ```
   `terms_hash` is the Keccak of the provider terms text the contributor was shown and accepted
   before joining. The seat needs a bond covering `outlier_slash` (as every pool member, ADR-0027
   amendment 6) held at least `KEYED_BOND_AGE` (proposed 7 days) before the draw's beacon height.
6. **Sybil: one person, many keys.** Onyx cannot tell five keys of one person from five people,
   and this design does not pretend to:
   - **Key fingerprints are refused.** Any deterministic function of the key, salted with public
     data or not, lets the provider (who knows every key) link an Onyx address to its customer;
     a randomly salted commitment proves nothing about distinctness. Onyx never asks for one.
   - **Cost per seat**: a bond per seat and the bond age (item 5); on Base Sepolia bonds are tONYX
     with no value, so keyed level 2 on testnet is a protocol demonstration, stated next to the
     value (as amendment 9 item 7 states for the one-machine rehearsals).
   - **One seat per declared operator in a draw**: the draw ranks seats by key as today and skips
     a seat whose declared operator already has a drawn seat (an additive rule, `draw_v2`, with its
     own tag; old draws unchanged). Self-declared, so it stops honest correlation (one company's
     staff), not a liar.
   - **History**: a seat is drawable only after `KEYED_MIN_HISTORY` agreed rounds on public
     sources (proposed 5), which a fresh Sybil address does not have.
   - **Shown odds**: next to every keyed value, the chance that a third of the pool colluding
     wins one draw, computed like `draw_commit::collusion_odds_of_the_rehearsal_pool`.
7. **Collusion with or by the provider.** The provider can answer each key differently. Agreement
   then fails (`NoResult`) or, worse, honest observers look like outliers. So keyed templates set
   `outlier_slash` equal to `miss_slash` (small): disagreement is not strong evidence of a lie when
   the source can target keys. A colluding majority still settles a false value (THREATS L2-F2,
   unchanged). The provider serving one wrong value to everyone is "what the source served", the
   level-2 claim, as for public sources.
8. **Only key-independent answers.** A template is admissible only if the value is the same for
   every key: a public series behind a metering key, never usage, quotas, balances or anything
   about the account. The reviewer checks the provider's documentation; the round enforces it,
   since account-scoped values do not agree.
9. **Rights: terms decide, not who holds the key.** Every keyed source stays held for a rights
   review (ADR-0038). It is admissible only when the provider's terms allow passing on data
   obtained with a personal key (or the data is public domain or openly licensed and the key only
   meters access). "Personal use only", "no redistribution" or "no automated access" refuse the
   source, with no "the contributor fetched it" workaround (contributor-loop hard limit).
   FRED stays blocked. A rate limit in the terms binds each contributor: one fetch per drawn
   observer per round, and `min_fetch_gap_secs` per key.
10. **Fetch slots are private for keyed sources.** ADR-0027 amendment 16 makes each observer's
    fetch time a public hash of the round id and its address. For a keyed source the provider
    would match that time to the key that fetched and link the key's account to the Onyx address.
    So a keyed observer picks its own random slot inside the window and never publishes it.
11. **The client as observer.** `onyx-contrib serve` runs the join-kit observer flow
    (docs/join.md, `level2-observer`) for the keyed pools it joined: reads `drawOf`, fetches with
    the key at its private slot, posts its commit (or has the CL6 relayer post it), sends the
    sealed reveal, waits for its receipt.

## Consequences
- A keyed public series can reach level 2 with no TLS prover and no key ever leaving a
  contributor's machine.
- Nobody without a key can re-read the source, so correction markets on keyed values (ADR-0057)
  need keyed challengers, and the level history shows that limit.
- The provider still sees which of its keys fetch at round times; Onyx adds no linkable
  identifier beyond that (items 6 and 10), and the contributor is told so before joining.
  (Amendment 1 item 9: this needed more than the private slot, and active probing by the
  provider remains.)
- On testnet the economic Sybil defence is nil (tONYX); history, operator spread and shown odds
  are what is real until bonds have value.

## Alternatives
- **A new on-chain lane** that collects keyed values over time and promotes a fact once k agree:
  rejected. Without a draw, whoever submits chooses when to stop (cherry-picking), and it repeats
  commit-reveal, bonds and receipts that the round already has.
- **TLS proofs per contribution** (ADR-0038 item 9, NS6): stronger (one proof is enough), still
  parked upstream. When it lands, a keyed value with a verified proof goes to level 3 by its own
  lane; this ADR stays for sources the prover cannot handle (TLS 1.3 only, large answers).
- **Key fingerprints for distinctness**: refused (item 6).
- **Onyx-held keys** (contributors lend keys to the observers): refused; keys never leave
  contributors, and most terms forbid it.

## Threats (rows CL4-1..CL4-8 go into THREATS.md with their tests when built)
| ID | Threat | Mitigation | Test to write |
|---|---|---|---|
| CL4-1 | The key leaks through a log, recording, reveal, crash file or error text | Item 2; the key is a vault handle until the request is built | `a_keyed_round_writes_no_byte_of_the_key_anywhere` (scans every file and message of a rehearsal for a planted key) |
| CL4-2 | The key goes to another host (redirect, template swap) | No redirects; the host is the pinned template's | `a_redirect_never_carries_the_key` |
| CL4-3 | One person fills a pool with many keys (Sybil) | Bond and bond age, history, one seat per operator per draw, shown odds | `a_fresh_seat_is_not_drawable`, `two_seats_of_one_operator_are_never_drawn_together` |
| CL4-4 | The provider answers keys differently and gets honest observers slashed | `outlier_slash = miss_slash` on keyed templates | `keyed_templates_slash_outliers_no_more_than_a_miss` |
| CL4-5 | An account-scoped value (usage, quota) enters as a fact | Review rule; disagreement gives `NoResult` | `account_scoped_answers_do_not_agree` (rehearsal with two synthetic accounts) |
| CL4-6 | Terms that forbid passing data on are bypassed | Held for rights review; manifest needs a cleared terms row | `a_keyed_manifest_without_cleared_terms_does_not_load` |
| CL4-7 | The provider links Onyx addresses to its customers by fetch time | Private slots for keyed sources | `keyed_fetch_slots_are_not_derived_from_public_data` |
| CL4-8 | An observer without a key copies others' answers | Commit-reveal with sealed reveals (unchanged) | existing round tests |

## Amendment 1 (2026-10-30): as built

Code: `Access::Keyed` in `onyx_core::feed_json`, `onyx_fetch::{ApiKey, KeyPlacement,
get_keyed, check_keyed_template_url}`, `onyx_level2::keyed`, the keyed rules in
`onyx_level2::manifest` and `onyx_level2::live`, the tool `crates/onyx-level2/examples/level2-keyed.rs`,
`onyx-contrib keyed join|observe|seats` and the MCP tools `contrib_keyed_join`,
`contrib_keyed_observe`, `contrib_keyed_status` (apps/contrib/src/keyed.mjs), the local demo
`scripts/level2-keyed-anvil.sh` with `fixtures/keyed-demo`. Where the build differs from or adds
to the decision above:

1. **Item 1, query keys.** A query key's template URL holds `name={api_key}` exactly once, as a
   query pair (the form CL2's proposals already send); a header key's holds no placeholder. Only
   the holder's own fetch fills it. With that one pair left out, the URL passes ADR-0053's
   template rules (no port, no OTHER secret-named parameter, host not blocked). The JSON form is
   `"access": {"keyed": {"place": "query"|"header", "name": "...", "scheme": "raw"|"bearer"}}`;
   `Bearer` only in the `authorization` header; reserved headers (host, cookie, user-agent,
   proxy-*, ...) refused. `Keyed` is appended after `Public`, so no public fetcher or template id
   moved.
2. **Item 2, the key's path.** The key is read from the vault by `onyx-contrib` and handed to the
   observer process on standard input only (`--api-key-stdin`), never a command line, an
   environment variable or a file; the observer holds it as `ApiKey` (no `Display`, a `Debug`
   that hides it, zeroed on drop) and every fetch error is passed through `ApiKey::redact`;
   every line the observer prints passes onyx-contrib's key filter. The site summary of a keyed
   round drops `response_body`.
3. **Item 5, the seat.** The signed message adds `operator: {id or none}`: without it anyone
   relaying a seat could drop the operator and defeat the operator rule. `terms_hash` is
   `keccak256` of the manifest's `terms_quote` (the text `keyed join` shows).
4. **Item 6, the operator rule.** Built as a rule of the round's pool snapshot
   (`keyed_snapshot`), not of the draw: of an operator's seats, only the one with the smallest
   `keccak256("onyx-keyed-operator-v1" ‖ spec_id ‖ address)` enters that round's snapshot. The
   draw, the pool hash, the contracts and every verifier stay unchanged (no `draw_v2` tag
   needed), anyone with the seats file recomputes the snapshot (`level2-keyed pool`; observers
   given `--seats` refuse another pool), and an operator with n seats has the chance of one seat.
5. **Items 5 and 6, eligibility.** A seat is drawable when its signature is the observer's, it is
   for this source and these terms, it joined before the window, its bond covers the round's
   `outlier_slash`, the bond is `KEYED_BOND_AGE` (7 days) old at the window start (the
   announcement lead before it is minutes), and it has `KEYED_MIN_HISTORY` (5) agreed public
   rounds. Bond, bond time and history are what the coordinator attests in the seats file (v1,
   like every pool snapshot's bonds today); reading them from the chain is TODO S2.
6. **Item 10, extended.** The private slot alone was not enough: the reveal's `observed_at` and
   page hash are public, and the provider knows the second each key fetched and the exact bytes
   it served each key. A keyed reveal therefore states `observed_at = window_start` and a page
   hash blinded with 32 fresh random bytes that are never kept
   (`keccak256("onyx-keyed-page-v1" ‖ blind ‖ keccak256(page))`). Agreement compares answers
   only, so nothing else changes.
7. **Item 9, terms.** A keyed manifest loads only under a docs/ops/data-sources.md section 6 row
   whose decision is exactly `OK keyed` (a plain `OK` clears public manifests only). The
   project's table has no `OK keyed` row today, so no real keyed source loads.
8. **Item 4, one holder.** With fewer than `KEYED_MIN_POOL` drawable seats the coordinator runs
   no round and prints the level-4 statement (`proof_level: 4`, "stated by one key holder").
   `KEYED_MIN_OBSERVERS = 3` is checked when the manifest loads.
9. **After the adversarial review (THREATS CL4-R1).** A keyed observer answers `Unavailable`
   when the value it read holds any piece of its key (a pointer or provider that echoes it);
   posts its commit at the window's end, not right after its private fetch (else the commit's
   time on chain gives the fetch second away); fetches once per round even when not drawn, as
   cover (else the public drawn sets, matched with the keys the provider saw fetch, map keys to
   addresses over a few rounds); refuses to run without the seats and refuses any pool but the
   round's keyed snapshot (a keyed announcement carries the seats). A seat counts 90 days after
   it was signed (`KEYED_SEAT_TTL_SECS`). The MCP tools read the terms folder from the user's
   `ONYX_CONTRIB_REPO`, never from an argument. Not removable, so stated: a provider that
   answers one key differently on purpose (or refuses it) finds that key's address from the
   public reveals (CL4-R1-4), and an offline seat is visible to it as a missing fetch.

## Open questions
1. `KEYED_MIN_POOL`, `KEYED_BOND_AGE`, `KEYED_MIN_HISTORY`: the proposed 9, 7 days and 5 rounds
   are placeholders (built as these constants in `onyx_level2::keyed`).
2. Should the badge read "Level 2 · re-observed by key holders", or should keyed rounds get a
   level of their own (2k)? Proposed: same level, evidence text.
3. Does a keyed observer earn the same as a public one? Observer pay is open (THREATS L2API-3,
   NS12).
4. Should the operator rule (item 6) also apply to public pools?
