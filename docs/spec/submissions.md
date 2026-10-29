# Spec: data submissions (ROADMAP S1, ADR-0038)

## Scope
How a person outside the network proposes data: the onboarding questions, the route they lead to
(proof level and next step), the submission's fields and checks, which endpoints a server may pull
to check it, the API that records it for review, and the keyed-source path where the submitter's
own API key never reaches Onyx. U2 adds local review and node integration (ADR-0039). Out of scope: publishing, re-observing (S4), staking
(S3), real proofs of keyed responses (S2), hosting (S5).

Code: `crates/onyx-intake` (rules, no I/O), `crates/onyx-api/src/submit.rs` (routes, journal,
transport), `apps/web/src/lib/intake.ts` and `apps/web/src/routes/submit/+page.svelte` (the
page). Shared vectors: `fixtures/intake/vectors.json`.

## Interfaces
- `onyx_intake::Answers { what, no_login, rights, license_id, names_person, changes }`; JSON
  `{"what", "noLogin", "rights", "licenseId"?, "namesPerson", "changes"}`, unknown fields refused.
  `what`: `chain | public_api | keyed_api | signed | own | forecast`; `rights`: `public_domain |
  open_license | own_data | not_sure | terms_forbid`; `changes`: `once | daily | live`.
- `onyx_intake::classify(&Answers) -> Route { outcome, level, path, reasons }`;
  `Submission::route()` adds the blocked-host rule. `Route::status()`: `pending_review`,
  `held_rights_review`, `held_keyed_source_review`, `refused`. JSON: `{"outcome", "level",
  "path", "reasons": [codes], "status"}`; `to_json_explained` adds `levelName` and `reasonTexts`.
- `onyx_intake::Submission` (JSON camelCase): `answers`, `title`, `description`, `sourceName`,
  `sourceUrl` (empty only for `own` + `own_data`), and optional `dataPayload {format, content,
  bytes, keccak256}`, `endpointUrl`, `jsonPointer`, `unit`, `endpointTemplate`, `seriesId`,
  `keyProof { prover, real, responseKeccak256?, responseBytes?, proof? }`.
  `Submission::parse(bytes)` = shape + `validate()`; `SubmissionError::code()` is stable.
  `Submission::hash()` = Keccak-256(`"onyx-submission:v1"` ‖ Borsh bytes).
- `onyx_intake::fetch_policy(&Submission, &Route, allow) -> FetchPolicy::{Pull, NoFetch(reason)}`;
  `CLEARED_HOSTS`, `BLOCKED_HOSTS`, `is_public_ip`, `preview(body, pointer)`.
- `POST /v1/submit/check` -> 200 `{valid, hash, route, fetch, preview, fetchError}` or 400 with
  the submission error's code, 413 `body_too_large`, 429 `rate_limited`.
- `POST /v1/submit` -> 201 `{id, status, hash, route}` (the same content again is a new entry
  with a new id), 400, 413, 422 `refused` (with `route`), 429 `rate_limited` or `daily_quota`,
  503 `intake_full` or `intake_unavailable`. Request cap is 64 KiB to carry a bounded sample.
- `GET /v1/submit/{id}` -> 200 `{id, status}` or 404 `not_found`. All answers `no-store`.
- `onyx_api::submit::{SubmitStore, SubmitLimits, PageFetch, HttpsFetch, routes}`;
  `examples/submit-dev.rs` (loopback only).

## Rules
- **R-SUB-1 (refusals first).** `names_person` or `terms_forbid` gives `refused`, with both reasons
  when both hold, whatever the kind. A refused submission is never recorded and never pulled.
- **R-SUB-2 (levels).** chain -> 1 `recipe_request`; public_api with no login -> 2 `auto_pull`
  (+ `live_window` when live); public_api behind a login -> 4 `claim` (`needs_login`,
  `unverified`); signed -> 3 `signed_source`; own -> 4 `claim` (`own_claim`, `unverified`);
  forecast -> 5 `track_record`; keyed_api -> 4 `keyed_proof` (`keyed_source`, `proof_not_real`,
  `unverified`). Level 4 always carries `unverified`.
- **R-SUB-3 (holds).** Rights `not_sure` holds the route (`rights_review` first). A keyed source is
  always held. A submission any of whose URLs names a blocked registrable domain (stlouisfed.org)
  or any subdomain is held with
  `source_blocked` first. Held submissions are recorded but never published until a reviewer
  clears them.
- **R-SUB-4 (fields).** Byte caps as in ADR-0038 item 4 (UTF-8 bytes, not characters); required
  texts not blank (blank: only Unicode White_Space and Cf format characters); no control characters
  (line breaks and tabs only in the description) and no byte-order mark; no e-mail-shaped text or
  `mailto:` in any free text; https URLs with a lower-cased DNS host name,
  no user info, no IP literal (including a last label of digits or `0x`), no fragment, port 1-65535;
  no URL gives a value to a secret parameter (names percent-decoded, lower case, array suffix
  dropped, pairs split on `&` and `;`; secret: holds key, token, secret, signature, passw or
  credential, starts with `x-amz-`, or is appid, auth, authorization, pwd, sig); RFC 6901 pointer
  only with an endpoint or template. Duplicate JSON keys and trailing text are refused. Empty
  `sourceUrl` is accepted only for `what=own` and `rights=own_data`.
- **R-SUB-5 (keyed fields).** `endpointTemplate` holds `{api_key}` exactly once, is an https URL
  with it filled, and has no other secret parameter; `endpointTemplate` and `keyProof` only with
  `keyed_api`; `keyProof` only as the stub (`prover = "stub-s2"`, `real = false`, no `proof`;
  `responseKeccak256` lower-case `0x` + 64 hex, given together with `responseBytes`, which is a
  whole number at most 2^53 - 1).
- **R-SUB-6 (pull policy).** A pull only when the route is accepted on `auto_pull`, the endpoint's
  host is in `CLEARED_HOSTS` and the port is 443 or absent. Otherwise the reason, in this order:
  `not_while_refused_or_held`, `no_endpoint`, `not_auto_pull`, `host_not_cleared`,
  `port_not_allowed`.
- **R-SUB-7 (transport).** Every resolved address public (`is_public_ip`) or no connection; connect
  only to those; IP literals refused; https only; no redirect followed (3xx is an error); body at
  most 256 KiB; 10 s for the whole fetch; one pull per check; a global pull budget.
- **R-SUB-8 (journal).** Each recorded entry: `hash`, `status`, `receivedUnix`, `route`, the
  submission as parsed. Never the client's address or any key. Same content twice: two entries,
  two ids; the reviewer groups by `hash`.
- **R-SUB-9 (the key).** The website keeps a typed key in component memory only; it is not part of
  any submission object, request, file or storage; a body containing it is refused on the page.
- **R-SUB-9a (key guard).** The page trims the typed key, refuses one shorter than 12 characters,
  and refuses a submission any of whose text fields holds it in any letter case.
- **R-SUB-11 (limits).** Per client and per UTC day as in ADR-0038 item 8; an IPv6 /56 and /48
  share one client's budget.
- **R-SUB-10 (offline).** A build without `VITE_ONYX_SUBMIT_API=1` sends nothing: steps 3 and 4
  say the API is not connected and the page offers the submission as a JSON file.
- **R-SUB-12 (own-data samples, ADR-0040).** Optional `dataPayload` is exact UTF-8 CSV/JSON, at
  most 8,192 bytes, with exact byte count and Keccak-256 digest. Payloads are accepted only with
  `what=own`, `rights=own_data`; server recomputes size and digest. Browser and server reject bad
  format/structure, obvious contact or credential patterns, personal-record fields, executable
  markup and spreadsheet-formula-like CSV cells. These heuristics are not a safety or rights proof;
  all payloads remain in the operator-private review journal and require review before approval.
  Receipt and public status responses remain `{id,status}` only. Metadata-only submissions still
  work, and no public upload path is created.

## Assumptions
- A submitter's answers can be false. The route is advice; a reviewer checks the rights row and
  the content before anything is published, and no proof level is granted by the intake.
- `api.fiscaldata.treasury.gov` keeps its reuse statement (re-read under data-sources.md section 5).
- E-mail detection is a shape check: it catches ordinary addresses, not obfuscated ones; the
  reviewer is the second line.
- The stub prover proves nothing; anyone can hash anything. Its only use is to fix the data shape.

## Open questions
- Reviewer tooling: who reads the journal, how a held submission is cleared or dropped, and how
  long a refused or stale entry is kept (S5).
- Which zkTLS stack and notary arrangement make a keyed response level 3 (S2; recommendation in
  docs/research/2026-10-20-api-keys-zk.md).
- Staking: bond size against the profit of a wrong value, and who disputes (S3).
- Whether an accepted level-2 submission becomes a feed template automatically or by review (S4).

## U2 receipt and review extension

ADR-0039 defines the exact versioned journal, bounded transitions, expected-revision/hash guard,
strict legacy replay, cancellation-safe serialization and maintenance-mode CLI. Node intake is
opt-in via `ONYX_SUBMISSIONS`; public status remains exactly `{id,status}` (plus the observers' public `check` once an
approved receipt was fed: ADR-0039 amendment 1) and never exposes the review queue. The receipt UI must retain a successfully recorded ID, offer an explicit status
refresh and receipt download, and explain uncertain delivery without automatic POST retries.
The local review queue and journal are private operator data, never web build assets.
