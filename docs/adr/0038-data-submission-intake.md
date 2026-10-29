# ADR-0038: Data submission intake: five questions, a proof-level route, rights-cleared pulls only, no personal data, offline by default, staking later

## Status
Accepted, 2026-10-20 (branch `submit-onboarding`, ROADMAP S1; scope addition the same day for
sources behind the submitter's own key). Built, not deployed: no shipped website build talks to
an API, and onyx-node does not mount the routes (S5).

## Context
On 2026-10-20 the project owner and a partner, a partner, asked for the lowest-friction way for anyone to
submit data: a website or API form where a person gives a description, the source and an
endpoint, so Onyx can pull the data itself and check it. The project owner wants an onboarding
questionnaire that decides the data type and its proof level (docs/spec/proof-ladder.md); any
data may be submitted with what it means, and that becomes an unverified level-4 claim. Their
demo: "I'm a random public-data user, walk me through submitting." FRED is blocked for
redistribution (docs/ops/data-sources.md N10), so the demo uses U.S. Treasury Fiscal Data, whose
rights are reviewed (adapters/source-fiscaldata/fixtures/PROVENANCE.md). Later the same day
The project owner added a first-user flow for a FRED user with an API key, with zk proofs as the direction:
the key must never reach Onyx. A further idea, submitters staking money on their data being
right (Polymarket-style), is recorded as S3 and not built.

Hard limits that shape every choice: entities only, never individuals; never ingest data whose
licence forbids redistribution; testnets only; no personal identifier in any request, file or log.
Prior art: RESEARCH.md "Data submission and bonded claims"; keyed APIs:
docs/research/2026-10-20-api-keys-zk.md.

## Decision
1. **A new core crate, `crates/onyx-intake`**, sector-agnostic, depending on `onyx-core` only,
   with no I/O. Not a module of `onyx-core`: the intake is JSON-facing policy (serde, the website's
   wire form), while `onyx-core` holds protocol data and has no serde; keeping it separate keeps
   the protocol crate small. `onyx-api` depends on it; no adapter does, and it depends on no
   adapter (scripts/check-boundaries.sh).
2. **Five closed questions** (`Answers`): what it is (chain data, a public page or API, a public API
   that needs my own key, signed by the source, my own or private data, a forecast); fetchable with
   no login, key or payment; rights (public domain, open licence with its id, my own data, not
   sure, terms forbid); names a person; how often it changes. Unknown fields and values are refused.
3. **`classify` is a pure function of the answers** (docs/spec/submissions.md R-SUB-2): names a
   person, or terms forbid: refused (not recorded). Chain: level 1, a recipe request. Public with
   no login: level 2, pulled by observers (a note when it changes live). Public behind a login:
   level 4, unverified. Signed: level 3. Own or other: level 4, labelled unverified. Forecast:
   level 5. A public API that needs the submitter's own key: level 4 on a keyed-proof path, always
   held. Rights "not sure": the same route, held for a rights review. A held submission is never
   published until a reviewer clears it. The route is advice, never a level granted to a value:
   a value still earns its level only through the ladder's own mechanisms.
4. **A submission carries no personal contact.** Fields: answers, title (120 bytes), description
   (2,000 bytes), source name (120), source URL, optional endpoint URL, JSON pointer (256), unit
   (32), and for a keyed source an endpoint template, a series id (64) and a key proof. U3 adds the optional bounded `dataPayload` contract under ADR-0040. A testnet
   wallet field was removed after review S1-R1-7: the same address on several submissions links
   them to one person in the journal; it returns with S3 only if staking needs it. There is no e-mail, phone or name field (unknown fields are refused), free text
   holding an e-mail address is refused, blank means only white space or invisible format
   characters and a byte-order mark is refused anywhere (S1-R1-4), URLs are https with a host name
   (no user info, no IP literal, no fragment), and a URL that gives a value to a secret parameter is
   refused: names read percent-decoded, split on `&` and `;`, array suffix dropped, any name holding
   key, token, secret, signature, passw or credential, `x-amz-*`, `appid`, `auth`, `sig` (S1-R1-5). The content hash is Keccak-256 over a domain tag and the Borsh bytes.
5. **Server pulls only from rights-cleared hosts** (`CLEARED_HOSTS`): hosts whose
   data-sources.md row says OK and that need no account, key, payment or contact header. Today
   that is `api.fiscaldata.treasury.gov` (S10/N3). SEC EDGAR (S11) is OK but its fair-access rule
   asks for a declared contact in the User-Agent, which waits on a role contact (P2g); BLS (N8)
   carries per-row duties. A pull happens only on the level-2 route, port 443, once per check.
   Every other endpoint is never fetched; the check says why. Hosts whose row says blocked
   (`BLOCKED_HOSTS`: the registrable domain stlouisfed.org and all its subdomains, FRED and ALFRED
   among them, S1-R1-3) hold a submission that names them, whatever the answers.
6. **SSRF defence in the transport, even for allowlisted hosts** (`onyx_api::submit::HttpsFetch`,
   feature `http-client`): a resolver that answers only when every address of the name is public
   (no private, loopback, link-local, metadata, shared, documentation, multicast, reserved, or
   IPv6 forms that carry such an IPv4 address), the connector connects only to those addresses,
   IP literals are refused before connecting, https only with webpki roots, no redirects, at most
   256 KiB, 10 s for the whole fetch, User-Agent `onyx-submit-check/1`.
7. **The preview reads the JSON-pointer value with the JSON-pointer fetcher's normalisation**
   (strings trimmed, integers and booleans as written, fractions refused, ADR-0009), reimplemented
   in the crate: the fetcher lives in `adapters/source-page`, which a core crate may not depend on.
   The preview is for a check by eye; no proof level comes from it. Moving the fetcher into core is
   left to S4, which needs it for re-observation.
8. **API** (`onyx_api::submit`): `POST /v1/submit/check` (validate, route, maybe pull, preview and
   body hash; records nothing), `POST /v1/submit` (records accepted and held submissions in the
   append-only journal of `journal.rs` as `pending_review`, `held_rights_review` or
   `held_keyed_source_review`; the same content again is a new entry, so no answer reveals
   whether someone sent it before (S1-R1-8); refused ones are a 422 and not recorded),
   `GET /v1/submit/{id}` (status only; no listing). Ids are `sub_` and 16 bytes of Keccak over
   the journal sequence and the content hash. Limits (S1-R1-2): 64 KiB body (ADR-0040); per client (an IPv4
   address, or an IPv6 /64 whose /56 and /48 share the same budget, no multiplier) 6 checks a
   minute and 60 a day, 3 submissions a minute and 10 a day, 30 status reads a minute; per server
   10 submissions a minute and 500 a day, 20 pulls a minute; 10,000 recorded (then 503
   `intake_full`). Filling the cap takes at least 20 days and 50 addresses or /48s a day. The journal keeps no client address. The
   routes are not mounted in onyx-node in S1; `examples/submit-dev.rs` serves them on loopback for
   local development. Hosting is S5.
9. **Keyed sources: the key never leaves the submitter's browser.** It is not part of the
   submission, never sent, logged or stored by Onyx, never in a URL to Onyx, never in the
   downloaded file, and the page refuses a submission that contains the typed key. The direction
   (S2 research) is that the submitter's side makes the request and produces a proof of the
   response (zkTLS); Onyx receives only the proof and metadata. In S1 the prover is a stub that is
   labelled "proof: not yet real (S2)": it makes no request, uses no key, and outputs
   `{prover: "stub-s2", real: false}` plus, optionally, the Keccak-256 and length of a body the
   submitter pasted. The API refuses any other prover or `real: true`. A keyed source stays level 4
   until a real proof exists; it reaches level 3 only when no single party can forge the proof
   (the reader verifies it, or k-of-n independent notaries sign). Whoever holds the key, the
   source's terms decide whether its data may be passed on, so every keyed source is held for a
   rights review; FRED stays blocked and its submissions stay held, not published.
10. **Website `#/submit` ("Add data")**: four steps (Questions, Details, Check or Key, Send), the
   route shown with its level and plain reasons, "Try an example" (Treasury) and a keyed example
   (FRED). Offline by default: every shipped build (preview and both public builds) says the API
   is not connected and offers "Download submission (JSON)"; it sends nothing anywhere. Only a
   build made with `VITE_ONYX_SUBMIT_API=1` posts, and only to its own origin (no CORS, no other
   host).
11. **One rule set, two implementations, one vector file.** `apps/web/src/lib/intake.ts` mirrors the
   crate; both run `fixtures/intake/vectors.json` (answers to routes, submissions to verdicts,
   statuses and pull decisions, texts, hosts and caps), written by hand, so the two cannot drift.
12. **Staking is deferred** (S3): a bond on a submission's correctness needs the attack-cost
   question answered first (what a wrong value earns against what the bond loses), and a dispute
   that ends by recompute or re-observation, not a token vote (RESEARCH.md: UMA, Polymarket).
13. **scripts/check-threats.py names Phase S items** (`S1`..`S5`) as milestones, so THREATS rows
   can point their TODOs at S2-S5.

## Consequences
- Anyone can be walked through a submission with no account and nothing sent; a reviewer gets a
  structured, rights-tagged queue once an API is hosted (S5).
- Only one host is pulled today; every new pull host needs its own rights row first.
- The route and its texts change only with the vectors, the crate and the page together.
- A keyed source is a documented dead end until S2 delivers a real prover and a rights path; the
  UI and data shapes are in place, and the stub cannot be mistaken for a proof.
- The journal grows with submissions; the recorded cap and global budget bound it until a
  reviewer tool exists (TODO S5).
- Not built: reviewer tools, publication of accepted submissions, observer re-observation of
  accepted level-2 submissions (S4), staking (S3), real proofs (S2), hosting (S5).
