# ADR-0053: A generic level-2 fetcher: a reviewed JSON source manifest instead of per-source Rust

## Status
Accepted, 2026-10-30; built in part (docs/contributor-loop.md CL3). Built: items 1-5 (the
descriptor in `onyx_core::feed_json`, `crates/onyx-json` with the shared vectors also run on
wasm32, `crates/onyx-fetch` used by the observers and the submission check pull,
`onyx_level2::manifest`, the first manifest `sources/us.treasury.tga_total_deposits`), item 6
steps 2-4 (section 6 of docs/ops/data-sources.md, the review tool `onyx-source draft|check|ids`,
the rehearsal on a local anvil: `scripts/level2-manifest-anvil.sh`), item 7, item 8's manifest
fields and `ManifestSource::admit`, and item 9's registry check for the runner. Not built yet:
the `/v1/sources` routes, the intake's pull set from manifests, `ONYX_OBSERVE` and the per-day
cap for manifests (THREATS CL3-9), `admit` in the long-running coordinator (CL3-6), the browser's
`check_manifest_round`. Item 4 as built: `max_bytes` caps the body read, also when the reader
runs; a manifest's subject is dated or daily and its value numeric (the runner backs claims for
numbers only); `Text` and `Bool` are in the language and the vectors. Builds on
ADR-0027 (level 2: templates, draws, amendments 16 and 17), ADR-0030 (the on-chain template pin),
ADR-0038 (intake, SSRF-safe transport, rights review), ADR-0040 (duplicate-key refusal) and
ADR-0049 (rounds run only on payment). Overview: docs/spec/contributor-loop-overview.md.

## Context
Every level-2 source today is a Rust adapter: `adapters/source-fiscaldata` (Debt to the Penny, and
`src/series.rs` for the Daily Treasury Statement), `adapters/source-usgs`, `adapters/source-ncei`
and `adapters/source-edgar`. Each supplies an `onyx_level2::Source` (template, fetcher, spec,
registry, shown names) and a `Fetcher` whose id is `keccak256` of a version tag, for example
`SERIES_FETCHER_TAG = "onyx-fetcher:fiscaldata-series:v1"`.

`series.rs`, the newest, shows that what differs per source is data: a `Series` constant holds an
endpoint, a row field and its text, a value field, a metric, a subject prefix, a name and a unit.
Its one fetcher only checks shape: exactly one record, the record date and row field equal to the
expected ones, exactly the keys the URL asked for, and a value that is a JSON string of whole
units. Anything else is `Answer::Unavailable`.

What exists and must keep holding:
1. `FeedTemplate` (`crates/onyx-core/src/feed.rs`) pins the fetcher id, the https URL template,
   the selector, the value kind, the window, the observer count, the rights and the subject rule.
   Its id, `keccak256("onyx-feed-template-v1" ‖ borsh(template))`, is what
   `OnyxRegistry.registerMetric(metric, Feed | DatedFeed, template, 0)` stores (owner only,
   append-only). A round's spec must equal the instantiated template field for field
   (`FeedTemplate::matches`).
2. Extraction is rerun by every observer, by the coordinator when it loads a store, by the Rust
   client (`client::verify_observation`) and by the browser (`apps/web/verify`,
   `feed_round_check`, which compiles in each source's pins today).
3. The intake pulls only from `onyx_intake::CLEARED_HOSTS` (one host today) through an SSRF-safe
   transport (`onyx_api::submit::HttpsFetch`: every resolved address public, no redirects,
   256 KiB, 10 s). Observers do not use it: `onyx_level2::live` calls
   `curl --proto =https -m 20` with no size cap and no address check.
4. `onyx_coordinator::origin_budget::OriginLoad` (ADR-0027 amendment 16) is a library check the
   long-running coordinator does not call yet (TODO S5).
5. A source enters only after a terms review: a row in docs/ops/data-sources.md section 5
   (`scripts/check-source-terms.py`) and a PROVENANCE file.

## Decision
1. **One generic fetcher whose id binds its rules.** `GenericJsonFetcher { d: JsonDescriptor }`
   implements `Fetcher` with `id() = keccak256("onyx-fetcher:generic-json:v1" ‖ borsh(d))`. The
   template id, and so the on-chain pin, commits to every extraction rule, the unit included.
   `FeedTemplate` does not change, so no live template id moves. `template.selector` must equal
   `d.pointer` (checked by `JsonDescriptor::check_template`).
2. **Where the code goes** (all sector-agnostic, so `scripts/check-boundaries.sh` stays as is):
   - `onyx_core::feed_json`: the Borsh types below, `validate`, `fetcher_id`. No serde, no I/O.
   - New crate `crates/onyx-json`: a strict JSON reader (item 4) and `extract(d, spec, body) ->
     Answer`, plus `GenericJsonFetcher`. Depends on `onyx-core` only; builds for WebAssembly, so
     `apps/web/verify` and `crates/onyx-intake` (whose preview then stops re-implementing the
     JSON-pointer rules, ADR-0038 item 7) call the same code as observers.
   - New crate `crates/onyx-fetch`: `HttpsFetch` moved out of `onyx_api::submit`, used by the
     intake, by the observer in `onyx_level2::live` (replacing `curl`) and by the contributor
     client of CL2.
   - `onyx_level2::manifest`: `SourceManifest` (a JSON file) and `ManifestSource`, which
     implements `onyx_level2::Source`.
3. **The descriptor** (Borsh; every field bounded; `validate` refuses anything else):
   ```text
   JsonDescriptor {
     version: u8,              // 1
     pointer: String,          // RFC 6901, 1..=256 bytes: the value
     value: ValueRule,
     unit: String,             // 1..=32 visible ASCII, shown and pinned ("million USD", "degC")
     asserts: Vec<Assert>,     // 0..=8, sorted by pointer, no duplicates
     max_bytes: u32,           // 1..=1_048_576; the observer stops reading above it
     access: Access,           // Public here; Keyed is ADR-0054
   }
   ValueRule = DecimalString | JsonNumber | DecimalOrNumber   // Numeric kind, at the template scale
             | ArrayLen                                      // Numeric, scale 0: a count
             | Text { trim: bool } | Bool                    // Exact kind
   Assert    = { pointer: String, expect: Expect }
   Expect    = Str(String) | Int(i128) | SubjectDate         // SubjectDate: the subject's YYYY-MM-DD
             | KeysExactly(Vec<String>)                      // sorted, <= 32 keys
             | ArrayLen(u32)
   ```
   This expresses what `series.rs` checks by hand (one row, its date, its row field, its key set,
   its value form) and what a count source needs (`ArrayLen`, or an integer field).
4. **Deterministic extraction, version 1.** Any failure is `Answer::Unavailable`, never a guess:
   - The body is at most `max_bytes`, valid UTF-8, has no byte-order mark, nests at most 64 deep.
   - **A duplicate object key anywhere refuses the document**: parsers disagree on which copy
     wins, so the browser and an observer could read different values.
   - A number is read from its exact text, never through a binary float (ADR-0009):
     `-?(0|[1-9][0-9]*)(\.[0-9]+)?`; an exponent is refused; digits beyond the template's scale
     must be zeros (refused otherwise: the old `JsonPointerFetcher` truncates them, which hides a
     source that changed its precision); the mantissa must fit `i128`.
   - `DecimalString` is a JSON string of exactly that grammar (no spaces, `+`, or separators);
     `JsonNumber` is a JSON number token; `Text` is 1..=`max_len` visible bytes.
   - Every assert holds before the value is read. **A pointer through an array index needs an
     assert on that same element** (a row-pinning `Str`, `Int` or `SubjectDate`), or `validate`
     refuses the descriptor: an API that reorders a list must never move the value silently.
5. **The source manifest** is reviewed data: `sources/<source-id>/manifest.json` with
   `PROVENANCE.md` (terms quoted with link and date) and `recording/` (one recorded response
   for the rehearsal and the browser pin). Fields: `v`, `id`, `metric`, `name`, `terms_url`,
   `terms_quote`, `template` (every `FeedTemplate` field, the subject rule and its date lag
   included), `descriptor`, `limits` (item 8), `earliest_public` (for market listings, as
   `Source::earliest_public`), and the stated `fetcher_id` and `template_id`. Loading
   recomputes both ids and refuses a file whose stated ids differ (the stated ids exist for review
   and diffs). Served read-only at `GET /v1/sources` and `GET /v1/sources/{metric}` (OpenAPI,
   ADR-0045), so a client can rebuild the template itself.
6. **The rights review gate.** Nothing reaches level 2 without all five steps:
   1. A submission on the "public page or API, no login" route (ADR-0038) names the endpoint,
      the pointer and the unit, or the CL2 client proposes them.
   2. A reviewer records the host's terms (data-sources.md section 5 row "OK": redistribution
      allowed with no account, key, payment or contact header) and writes `PROVENANCE.md`; the
      value is about an entity or an aggregate, never a person.
   3. `onyx-source draft <url> <pointer>` (an example binary in `onyx-level2`) fetches once
      through `onyx-fetch`, saves the recording, proposes a descriptor with row asserts and prints
      the extracted value. The reviewer edits the asserts by hand.
   4. A recorded rehearsal round passes (`onyx_level2::rehearse` on a local anvil).
   5. The owner sends `OnyxRegistry.registerMetric(metric, kind, template_id, 0)` on Base
      Sepolia. Only then does `ONYX_OBSERVE` (ADR-0049) offer the metric.
   The submitter's URL never reaches observers: the reviewer's template governs (as
   `intake_feed.rs` already does for Debt to the Penny). `CLEARED_HOSTS` stops being a constant:
   the intake's pull set is the hosts of the loaded manifests, and `BLOCKED_HOSTS` still wins.
7. **SSRF defence at every fetch, node and observer alike** (`onyx-fetch`):
   - Resolve once; refuse unless every address is public (the `onyx_intake::is_public_ip`
     rules); connect only to those addresses (no second lookup, so DNS rebinding after review
     fails); https on port 443 only; webpki roots; no redirects (a 3xx is `Unavailable`); no
     cookies; no proxy from the environment; `Accept: application/json`; a neutral User-Agent
     `onyx-observer/1` naming no person or account.
   - Refuse a declared `Content-Length` above `max_bytes`; stop reading at `max_bytes`; accept only
     `identity` or `gzip` encoding and cap the decoded size at `max_bytes` too; 20 s for the whole
     fetch.
   - `FeedTemplate` validation for manifests adds: host a DNS name (no IP literal, user info,
     port or fragment), URL at most 2,048 bytes, no secret-named query parameter (ADR-0038 item
     4's list), host not in `BLOCKED_HOSTS`.
   An observer on its own machine runs the same checks, so a template cannot make it call its
   own network, a cloud metadata address or localhost.
8. **Rate limits** (protect the source, so it does not block observers): `limits` holds
   `max_rounds_per_day` (counted by the node's observe worker over every fact of the source; past
   it a paid request answers `503 source_budget_spent` with `Retry-After` and no hold is taken),
   `origin_budget` (at most `DEFAULT_MAX_FETCHES_PER_ORIGIN` = 16) and `min_fetch_gap_secs` from
   the source's terms. `OriginLoad::admit` becomes mandatory before `announceDraw` for every
   manifest source (closing TODO S5 for them), with the budget capped at `window_secs /
   min_fetch_gap_secs`. Fetch slots stay spread over the window (amendment 16).
9. **Clients.** For a manifest metric, `client::verify_observation` builds the template from the
   manifest and requires `template.id()` to equal `OnyxRegistry.metrics(metric).template` read
   on the client's own node. The browser gets `check_manifest_round(manifest, summary)`: the
   template id the build read from the registry replaces compiled per-source pins, and response
   and result pins come from the recording.
10. **Existing adapters stay.** Their templates are live and pinned on Base Sepolia; moving them
    would change fetcher ids and void markets. New level-2 sources are manifests only.

## Consequences
- A new public JSON source costs a terms review, a manifest, a recording and one owner
  transaction: no Rust, no release of the verifier.
- The extraction language is deliberately small: no arithmetic, no search inside arrays, no CSV,
  XML or HTML, no POST. A source that needs more stays a Rust adapter.
- A template is only as safe as its asserts; item 4's array rule makes the most common mistake
  (reading whatever row comes first) impossible to register.
- Observers fetch from any reviewed host; their exposure is the reviewed list plus item 7.
- More sources means more rounds, each about 14 to 15 transactions on Base Sepolia (ADR-0049
  context); rounds still run only when paid for, and item 8 caps them per source.

## Alternatives
- **A WebAssembly extraction program per source**, deterministic under ADR-0004: any logic, but a
  reviewer reads code again and the browser verifier runs submitted programs. Reconsider when a
  source needs arithmetic.
- **jq or JSONPath**: richer selection, but no single specification with identical Rust and
  browser implementations, and filters make the value depend on array order in ways a reviewer
  misses.
- **A descriptor field in `FeedTemplate`**: changes every template id, the live ones included.
  The fetcher id already carries it.
- **Fetch the submitter's own endpoint after approval**: refused, as in U4: the reviewer's
  template governs what observers fetch.

## Threats (rows CL3-1..CL3-9 go into THREATS.md with their tests when built)
| ID | Threat | Mitigation | Test to write |
|---|---|---|---|
| CL3-1 | A template or DNS change makes observers or the node call a private address (SSRF, rebinding) | Item 7: one resolution, public addresses only, connect by address, no redirects, port 443 | `fetch_refuses_private_rebinding_and_redirects` |
| CL3-2 | A huge or compressed body exhausts an observer | `max_bytes` on the wire and after decoding; 20 s | `a_body_over_the_cap_is_unavailable_and_not_read_further` |
| CL3-3 | Two parsers read different values (duplicate keys, float rounding) | Item 4's strict reader; shared vectors run by Rust and the browser build | `json_fetcher_vectors_match_in_rust_and_wasm` |
| CL3-4 | The value moves to another row when the API reorders a list | Array index requires a row assert (`validate`) | `a_descriptor_reading_an_array_without_a_row_assert_is_invalid` |
| CL3-5 | A unit or scale swap (thousands read as units) | Unit and scale are in the fetcher id, so in the chain pin | `the_unit_is_part_of_the_pin` |
| CL3-6 | Observers get rate-limited or banned by the source | `OriginLoad` mandatory, per-day cap, spread slots | `the_runner_refuses_a_round_over_the_origin_budget` |
| CL3-7 | A submitter's rights answer launders a closed source | The reviewer's terms row governs; a manifest whose host has no "OK" row does not load | `a_manifest_without_a_cleared_terms_row_does_not_load` |
| CL3-8 | A forged manifest on the site or the API | Template id compared with the registry on chain | `a_manifest_whose_template_id_is_not_registered_is_refused` |
| CL3-9 | Paid-request spam drives many rounds at one source | Per-source day cap before any hold (ADR-0049 holds after it) | `source_budget_spent_takes_no_hold` |

Tests to write first, in `onyx-json`: the vectors file `fixtures/json-fetcher/vectors.json`
(descriptor, spec, body, answer: duplicate key, exponent, extra precision, a byte-order mark,
depth 65, `max_bytes + 1`, a `SubjectDate` one day off, `KeysExactly` with one extra key,
`ArrayLen`, a mantissa above `i128`) and a property test that `extract` never panics on any
bytes. In `onyx-level2`: the TGA series rebuilt as a manifest extracts the same value as
`SeriesFetcher` from the same recording (proof that the language covers the newest adapter),
though its fetcher id differs and the live series stays on its own fetcher.

## Open questions
1. Is 1 MiB the right `max_bytes` ceiling? Some APIs return a whole table to give one row.
2. Should a reviewer key other than the owner register templates on chain?
3. Date formats beyond `YYYY-MM-DD` (`YYYYMMDD`, epoch milliseconds): a closed list in version 2?
4. Should `onyx-source draft` run on the hosted node (one more pull path) or only on a
   reviewer's machine (proposed)?
