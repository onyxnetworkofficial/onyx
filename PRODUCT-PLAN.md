# Onyx product and backend build tracker

Updated: 2026-10-22. Branch: `product-onboarding`. U1–U3 passed; U6A concept-only Markets UI is implemented (ADR-0041). U4/U5 and any backed market system (U6B) remain open.
This is the implementation queue for the contributor-first app, requested by the project owner on October 21.
The editable frontend is `apps/web` (SvelteKit), not the hosted Claude artifact. The artifact is a
preview destination. Rust APIs, workers and contracts remain the backend; do not rebuild the
protocol in a frontend framework.

## Product we are building

**A person arrives with useful data, describes or supplies it, understands the next step, and can
follow its review and verification. A buyer finds data and sees what supports it before using it.**

The homepage asks people to add their own data. Browse data is the secondary journey. Technical
proofs, validator demonstrations, chain transactions and protocol vocabulary belong under How it
works and its specialist pages. A contributor does not have to understand a proof level to start.

This plan is a proposed implementation of the call, not a claim that every backend detail was
agreed on the call. The call did not settle token economics, staking parameters, source licences,
a production deployment, or a production zkTLS verifier.

## Evidence from the October 20 call

Source: the locally transcribed 1:00:32 recording, `transcript-timestamped.txt` in the
`onyx-meeting-2026-10-20` meeting folder. The automatic transcript contains errors; timestamps are
navigation aids, not speaker-attributed quotations. Neither recording nor transcript is committed.

| Time | Requirement or question from the conversation | Product consequence |
|---|---|---|
| 00:00–15:00 | Contributors, customers and checking data | Distinct contribute and browse journeys; evidence attached to data |
| 16:38 | Useful data already held by market participants | Accept proposals for contributors' own datasets, not just our examples |
| 24:00 | Reduce friction | Plain questions; no wallet or bond just to propose a dataset |
| 25:56–30:44 | Staking and wealthy attackers | Economics remains a separate design gate; do not promise rewards or safety from a bond |
| 37:41 | Repeated submissions and rewards | Group duplicates for review; never grant multiple rewards for repeated uploads |
| 44:07–47:08 | Demand beyond onchain data; sample versus actual usage | Source-neutral intake; label recorded examples and testnet activity honestly |
| 50:12 | Walk through a new contributor and a buyer | Test both complete user journeys, including failure and held states |
| 50:59 | Form with description, source and endpoint; automatic pulls | Endpoint adapter and safe preview, followed by a reviewed feed |
| 51:00–55:00 | API keys, privacy and proofs | Keys stay with contributor; storing an encrypted key on our server is not zkTLS |
| 55:48–56:48 | Flows/economics and verification/FRED follow-up | Separate UX work from proof integration and unresolved economics |

## Pages and user language

| Destination | Purpose | What appears here |
|---|---|---|
| Home `/` | Invite a contribution | Add your data, examples of kinds of data, three next steps, Browse data |
| Add data `/submit` | Contribute at the user's pace | Source type, description, rights, optional structured content, review, receipt |
| Browse data `/data` | Find useful data | Dataset purpose, source, freshness, supported uses; evidence behind details |
| How it works `/how-it-works` | Explain and demonstrate verification | Proof methods, browser checks, live-run evidence, specialist links |
| Run / Reserves / Explorer / Labels / API | Inspect a particular mechanism | Existing working tools, accessible through technical links and footer |
| Submission status (U2) | Follow an existing receipt | Status, safe explanation and next action; no raw submission in public responses |

Header: Browse data, How it works, Add data; wordmark returns Home. Footer and command palette
retain all destinations. No technical console or automatic verifier/data download on Home.
Submission copy must distinguish **proposed checking method**, **review approved**, **verified**
and **published**. None implies the next. A forecast is an unproven statement until resolved;
its track record is history, not a certificate of truth.

## Backend: concrete target architecture

```text
Svelte app
  │ same-origin JSON; bounded requests; no API keys
  ▼
Caddy → onyx-node HTTP guards → onyx-api submission handlers
                                │
                                ▼
                      durable intake journal
                                │ local reviewer CLI (no public admin API)
                                ▼
                      reviewed feed configuration
                                │ bounded scheduler / worker
                                ▼
                 source adapter → observation / recipe / signed-source lane
                                │ existing validators + finality rules
                                ▼
                     existing store + chain registry
                                │ serving gate + proofs
                                ▼
                  dataset catalogue + existing query APIs
```

### 1. Intake and durable review

Reuse `crates/onyx-intake` for pure validation/classification and
`crates/onyx-api/src/submit.rs` for transport, budgets and journal. Mount it behind the same request
limits, client-address attribution, concurrency and deadlines as the node's other routes. A local
standalone development example is not a deployed API. Intake is enabled only with an explicit
journal path; broken persistence must refuse a submission rather than acknowledge it.

Existing wire contract (keep compatible):

| Interface | Result | Current state |
|---|---|---|
| `POST /v1/submit/check` | Parsed submission, advisory route and bounded source preview | Mounted in the node; source checks follow existing rights/adapter limits |
| `POST /v1/submit` | `201 {id,status,hash,route}` after durable append | Mounted in the node; starts in private `pending_review` state |
| `GET /v1/submit/{id}` | `{id,status}`, `no-store`; no content or listing | Implemented; only receipt-safe status is public |
| Local `submit-review list/show/decide` CLI | Bounded private queue and append-only decision events | Implemented under ADR-0039; never accessible through Caddy |
| Feed import / worker CLI | Consume only an approved, supported configuration | U4, to build |
| Public dataset catalogue | Only published, rights-cleared dataset metadata and proof references | U5, to build |

An anonymous receipt ID is a locator, not authentication or proof of ownership. The existing ID is
derived from sequence and submission hash. Therefore its public status cannot disclose the title,
source, payload, reviewer notes, duplicate groups or personal information. Ownership/editing/private
status would require a separate secret capability or authenticated identity; neither is silently
inferred from possession of a receipt.

Target records, with versioned encodings and limits in the spec before implementation:

| Record | Fields / invariant |
|---|---|
| Submission | Existing validated answers and metadata; content hash; immutable received timestamp |
| ReviewEvent | Submission ID, monotonic revision, previous state, next state, bounded reason code, timestamp, reviewed content hash |
| Payload (U3) | Bounded UTF-8 CSV/JSON, format/schema, content digest, byte count; private until rights and content review |
| FeedConfig | Approved submission hash, adapter ID/version, endpoint, extraction path, integer unit/scale, cadence, rights decision, limits |
| VerificationJob | Feed/version, observation window, pinned inputs, attempt, state, resulting evidence references |
| PublishedDataset | Stable dataset ID/version, title/description, units, licence, source, freshness, proof method and finalized reference |

Use the existing durable journal for the first local workflow and replay it on startup. Append
before acknowledging, lock against concurrent writers, reject malformed/reordered events and
stale review revisions, cap records and event counts. Capacity admission, durable append and live
index publication must be one serialized operation that completes even if the request is cancelled;
a handler timeout cannot leave durable records absent from the live index. Do not add a database just to duplicate the
journal. A multi-node deployment and retention/migration policy need a later storage decision.
Protocol commitments remain Borsh + Keccak, no floats; transport can remain JSON. Reuse existing
protocol stores rather than assigning a new verification level in the submission database.

Initial states already implemented: `pending_review`, `held_rights_review`,
`held_keyed_source_review`. Target review states: `approved`, `rejected`. Approval means permission
to create a supported verification job, not permission to assert that a row is verified. Job state
is separate: `queued → running → verified | failed`; publication is a separate serving gate.
An unsupported own-data claim or forecast can remain held with a clear explanation; it must not
be relabelled as verified merely to finish a flow. Exact allowed review transitions and restart
semantics will be frozen in the U2 spec/ADR before code.

No public submission listing, reviewer endpoint, raw error log or automatic publication. Group
identical hashes privately for reviewer efficiency; do not expose whether another person supplied
the same content. Repeated submissions are not paid work. Do not promise rewards in the UI.

### 2. Flexible data input

Accept a proposal without requiring a template from Onyx. Preserve existing types: onchain,
public API, keyed API, source-signed data, own data, forecast. Let contributors edit all relevant
fields and review the exact outgoing submission.

U3 adds optional CSV/JSON file selection and paste for **data the contributor has permission to
share**, with explicit limits, parse errors and preview before sending. ADR-0040 caps exact UTF-8
content at 8 KiB, checks its digest and byte count, and rejects malformed files, duplicate JSON
keys, formula-like CSV cells, markup, common credentials, contact data and personal-record fields.
These are safety signals only; the reviewer still checks the rights and sample. The browser reads
text only and never evaluates expressions or uploaded code. No public upload directory is created;
connected samples remain in the owner-only review journal and safe status responses disclose no
content. Own-data contributions work without a public source URL; any supplied URL still receives
the HTTPS/secret checks. Metadata-only contribution remains available. Real private-dataset access
control is separate and is not implied by an “own data” choice.

### 3. Source fetching and verification jobs

Keep S1 controls: HTTPS only, reviewed host allowlist, no redirects, DNS resolved and pinned to
public addresses, private/loopback/metadata addresses refused, 256 KiB response cap, 10 s fetch
timeout, bounded per-client and global budgets. A contributor-supplied URL cannot expand the
allowlist. A browser preview hash proves only which bytes were previewed; it is not independent
verification.

U4 first supports one local, deterministic public-source integration: approved Treasury source
configuration → existing level-2 observer flow → checked result and receipt/evidence link. Tests
use fixture responses and local chain; no paid service or production chain. Configuration pins
JSON pointer, integer unit/scale, observation time/window and adapter version. Failures retain a
reason and can retry within bounded budgets; retries cannot publish or pay twice. Source drift,
changed rights, failed checks and unfinalized roots prevent publication.

Onchain recipes continue to run through existing deterministic WASM actors. Signed sources use
the existing key registry and signature lane. Own data is a claim unless independent evidence
supports it. Forecasts use source-specific resolution rules and existing track-record machinery;
no fabricated future resolution, general-market accuracy score or unsupported level upgrade.

### 4. Keyed sources and rights

Current keyed-source proof is explicitly a stub. Keep keys in page memory, never in submissions,
URLs sent to Onyx, journal, exports, browser persistence or logs. S2 needs an actual verifier,
source/endpoint/time binding, selective disclosure and replay protection before calling a keyed
response proven. TLSNotary research is a starting point, not a deployed integration.

FRED is a held example until reuse terms and proof requirements are resolved. Publicly reachable
does not mean licensed for redistribution. Keep existing blocked sources and public-build rights
fallback. No contributor can approve their own rights or authorize publication through a form flag.

### 5. Buyer delivery and economics

The catalogue exposes only eligible published metadata. Data rows use existing proof-bearing query
APIs, finalized-chain checks and x402 testnet payments. Show freshness, unit, source, verification
method and clear empty/error states. Evidence details stay available without dominating the browse
flow. No unauthenticated preview of a paid row and no “live” label for a recorded demo.

The call's staking/rewards ideas remain open: bond sizing versus attack profit, duplicate reward
policy, Sybil/affiliate treatment, challenge funding, appeal trust and actual observer payouts.
The current observation revenue path is not a completed contributor reward system. Level-4
mechanism design remains parked; do not unpark or deploy it as a shortcut. These dependencies do
not prevent building honest intake, review and a local verified dataset flow.

## Prediction markets and data futures (October 21 follow-up)

The project owner asked whether Onyx can support Polymarket-like positions on events or data. **Technically
feasible, but no working exchange exists in this repo today.** Add it as a proposed product layer
on the data network. This follow-up records the design direction; it does not silently activate
real-money markets, invent settled token economics, or replace U1–U5's contributor workflow.

The call contains two different mechanisms:

- **31:52–32:20 and 38:01–38:18:** forecast a future value (tomorrow's close / next data release),
  where the outcome is not yet known. This can become a prediction market.
- **29:34–30:43, 33:01–33:15 and 56:10–56:39:** stake behind a submitted claim's correctness,
  including a proposal to let betting act as the oracle. This is a verification/incentive design;
  the call itself raises a wealthy attacker at 30:44–31:13 and duplicate rewards at 37:41–37:54.
- **44:32–47:38:** compute/GPU prices and reported model token usage as possible underlyings.
  At 47:08 the discussion explicitly distinguishes an aggregator's sample from global usage.
- **48:56–49:20:** futures/prediction-system expansion was discussed as later work. It was not a
  settled specification for an exchange.

Recommended first prototype: a fully collateralized **binary market on one reviewed dataset**.
Example (illustrative, not an offered market): “Will source S report metric M above threshold X
for date D?” Start on a local chain/testnet with test tokens and predefined market creation.
General event markets require their own outcome adapter and equally precise settlement rules.

Before any position can be opened, freeze a versioned market definition: dataset/source identity,
metric and unit/scale, observation date/timezone/window, comparator and threshold, trading cutoff,
resolution deadline, first-release versus revision policy, accepted evidence method, finality rule,
and missing/ambiguous-data cancellation rule. A source's claim is not automatically world truth:
“Source S reports X” and “global usage is X” are different markets.

Required components beyond U1–U5:

| Component | Responsibility / invariant |
|---|---|
| Market registry | Immutable terms hash and deadlines; reviewed source adapter; no changing rules after trading starts |
| Collateral and positions | Test-token escrow; fully backed Yes/No positions; balances and payouts conserved; no unbacked reward minting |
| Trading service | A chosen order-matching or liquidity mechanism, positions and fills; prevent replay, overspending and duplicate fills; do not implement both mechanisms in the first prototype |
| Market index/API | Read market terms, state, actual quotes, positions and fills; distinguish stale quotes and completed fills from submitted orders |
| Resolution adapter | Consume eligible finalized evidence from the selected dataset and mechanically evaluate the predeclared rule |
| Challenge/cancellation path | Explicit policy for contested evidence, missing source, revisions and deadlines; no arbitrary creator-selected winner |
| Settlement contract | Finalize once; winning payout or predefined void treatment; repeat redemption cannot pay twice |
| Frontend | Separate Markets area, market detail, Yes/No order preview, positions, resolution evidence and redeem state |

Market lifecycle: `draft → open → closed → resolution_pending → resolved | void`.
Trading closes before the target observation becomes available. A delayed resolver cannot reopen
trading. Redemption stays disabled until the declared dispute process and finality requirements
have completed. Fees, rewards and challenge bonds must not consume collateral backing outstanding
positions. After final resolution, winners redeem according to the immutable payout rule. Void
payouts are fixed per outcome token and redeem independently, with a predetermined integer rounding
rule; for example equal halves for Yes and No, without needing to hold both. They are not vaguely
promised refunds of every trader's purchase price after secondary trading.

**Do not settle from the market price, stake majority or the submitter's own claim.** Independent
resolution evidence and an explicit dispute/failure model are essential. Existing source-verification
and track-record components can supply evidence/history, but they do not implement exchange custody,
liquidity, order matching or payouts. Existing coordinator-arbitration limitations must not be
hidden behind a “trustless market” label. S3/level-4 staking remains a separate unresolved design.

Binary event positions are the proposed first product. Numeric futures/perpetual long-short markets
are a further design: index construction, margin, liquidation, funding (for perpetuals), insurance
and insolvency handling are additional systems, not a small UI variation.

Proposed follow-up milestones (design backlog, not claimed built or substituted for U1–U5):

1. **Markets design:** select one measurable underlying and source, freeze settlement/cancellation
   rules and trading mechanism, ADR/spec and adversarial review.
2. **Testnet market vertical slice:** create one market → fund test positions → trade → close →
   resolve from eligible evidence → redeem. Test missing data, forged evidence, revisions, late
   trades, double fills, duplicate settlement/redemption and full collateral conservation.
3. **Market frontend:** readable question, source and deadline; actual quotes and positions; clear
   testnet label; no synthetic volume/liquidity represented as customer activity.
4. **Further expansion:** new reviewed sources/event adapters; only then evaluate numeric futures.
   Public real-money launch and its jurisdiction/deployment requirements are outside this prototype.

Primary sources checked 2026-10-21: [Polymarket resolution rules](https://docs.polymarket.com/concepts/resolution)
(predefined source, date, edge cases; outcome resolution distinct from trading) and the
[Polymarket UMA adapter](https://github.com/Polymarket/uma-ctf-adapter) (an explicit adapter between
resolution evidence and conditional-token payout). These inform the separation of responsibilities;
this plan does not claim Onyx implements or inherits the security of those systems.

## Execution loop and acceptance

The active Codex build goal follows this queue. This file is its durable handoff; it is not a cron
job and does not claim unattended hosting. Each milestone: inspect current state → research/spec →
small implementation steps → required checks → fresh adversarial review → record findings/tests
in THREATS → update this file, PLAN and STATUS → task-only commit and push to private origin.
Maximum six loops per session. Three repeats of the same failure require a narrower design or user
input. Do not tick a milestone on partial tests, a mock receipt, or an unreviewed implementation.

- [x] **U1 — Contributor-first frontend and this blueprint.** Home asks for data; simple header;
  old proof tools on How it works; all routes discoverable; mobile/accessibility/navigation checks;
  existing verification and privacy assertions preserved at their new route.
- [x] **U2 — Real receipt and review workflow.** Node integration and Caddy parity; durable decision
  events; local reviewer CLI; visible receipt/status UX with honest offline fallback; replay,
  malformed event, stale revision, unauthorized transition, rate-limit, concurrent last-slot,
  cancellation-during-append and restart tests. Passed 2026-10-22 with full check-all and actual-node
  browser rehearsal; see [submission-review.md](docs/ops/submission-review.md).
- [x] **U3 — Bring your own data.** Bounded CSV/JSON file/paste and metadata-only workflows; clear
  errors and preview; format/credential/personal-data guardrails; absent source URL supported for own data; canonical versioned payload
  contract; no public raw uploads; browser/server parity tests. Passed 2026-10-22 with full check-all,
  connected node/browser rehearsal and fresh adversarial review; see ADR-0040 and docs/spec/submissions.md.
- [ ] **U4 (PARTIAL 2026-10-24: `onyx_source_fiscaldata::intake_feed`; an approved Treasury submission runs the pinned observer round on recorded fetches; status, endpoint, field and rights gating and retry tested in `tests/u4_intake_feed.rs`; worker CLI `examples/feed-approved.rs` over the review journal, run end to end 2026-10-24: a real `submit-dev` POST was refused while pending, then approved with `submit-review` and fed: round `0xea56…f407`, value 40305316210829.72 served from the written store; retries bounded by a feed ledger beside the journal (fed once, 3 failed attempts, reasons kept; run twice 2026-10-25: the second run returned the recorded evidence and ran nothing); receipt-to-evidence link done 2026-10-25 (ADR-0039 amendment 1, docs/pre-public-loop.md PP1: the restarted node shows the round id, result hash and record date on the fed receipt, checked in the connected browser rehearsal); open: live observers for dates without a recording) — One approved source through the real pipeline.** Reviewed configuration feeds a local
  observer round; checked result with verifiable evidence; retries/restart/dedup/failure/rights
  gating tested. No claim that arbitrary sources work without adapters.
- [x] **U5 — Browse and local end-to-end handoff.** Dataset-first browsing; details disclosed on
  demand; contributor-to-review-to-verification-to-buyer rehearsal; runbook; clean build; all
  required gates; concrete deployable preview bundle. Done 2026-10-25 (docs/pre-public-loop.md
  PP4): Browse data opens with one card per dataset (`src/lib/catalog.ts`: level, source, unit,
  rights, API) that brings its section into view; `scripts/level3-book.sh local` runs submit,
  review, feed, serve and the buyer's own `check-observation` on a real node; runbook
  docs/ops/local-handoff.md; `scripts/preview-bundle.sh` (public fallback site, API file, units,
  settings template, checks, SHA256SUMS; never uploads). Open: freshness per dataset card, live
  observers (U4), a host (LC-8).

External/deferred work stays explicit and separate from U1–U5: public host/domain and secrets,
source rights clearance, a production zkTLS prover/notary arrangement, real private-data access
control, token/bond economics, contributor payouts and contract/deployment audit gates. Do not
count these as completed by a local demo. Re-evaluate scope with the owner if any becomes essential
to the requested usable workflow.

## Progress and evidence

- 2026-10-21: traced the full one-hour call and current code. S1 exists, but shipped frontend builds
  are offline; the API example is not mounted in the production node and review/publication are
  absent. Created branch and persistent build goal; U1 acceptance is recorded below.
- Review backlog: `/v1/signed/{fact}` is mounted by the node but missing from Caddy's v1 matcher;
  fix and add parity coverage with U2's route integration. Separate existing limitations:
  coordinator-controlled dispute arbitration and incomplete observer revenue distribution.

- U1 fresh adversarial review: no merge blockers; preserved proof/fallback gates and command-palette routing. U1-R1-1's mandatory-URL obstacle is fixed by U3's URL-free, bounded own-data sample flow.

- October 21 follow-up: added the call-backed distinction between data-validity staking and future-outcome trading, plus proposed prediction-market architecture and testnet milestones. No exchange or market contract is represented as built.

- Market-design review: no blocker or U1–U5 scope change. Clarified dispute/finality-gated redemption, separate position backing and independent void payouts with fixed rounding (U1-MR-1).

- U2 preparation review confirmed intake races: concurrent requests could exceed the queue cap, and cancellation could skip live index publication after a durable append. U2 fixed these with serialized cancellation-safe mutation, a bounded worker semaphore, and regression tests before mounting the routes.

- U1 acceptance (2026-10-21): `scripts/check-all.sh` passed, including Rust workspace checks,
  local Anvil rehearsals, documentation/security checks, 595 contract tests (one optional fork
  test skipped), and 556 gas-profile tests with the committed snapshot unchanged. Separately,
  `ONYX_PUBLIC=0 ONYX_RIGHTS= scripts/build-web.sh` passed with 1,101 browser assertions,
  Svelte check/build and rebuilt Rust/WASM verification. Desktop and phone screenshots inspected.
  Public cleared/fallback builds and Docker are opt-in and were not run for U1; no public deploy.

- U2 implementation: versioned strict ledger replay, local review CLI, serialized admission/index updates and cancellation-safe bounded workers; node/Caddy integration and receipt UI added. Fresh adversarial review found and helped close resource-bound, in-flight draft, market URL/series and test synchronization issues. Actual node approved a submission, restarted, returned the safe receipt status, and the browser completed the delayed single-POST, receipt download, storage/privacy and error-state checks. `scripts/check-all.sh` passed on 2026-10-22: 595 contract tests (one optional fork test skipped), 556 gas tests with unchanged snapshot, workspace Clippy/tests, local Anvil rehearsals, docs and security checks. Private website gate passed cross-device route/accessibility/E2E checks. Operator procedure: [submission-review.md](docs/ops/submission-review.md).

- U3 passed 2026-10-22: [ADR-0040](docs/adr/0040-own-data-payloads.md) follows RFC 4180, RFC 8259 and OWASP CSV formula-injection guidance. Own-data intake works through browser file/paste preview, guarded node validation, payload-aware hash versioning and owner-only journal storage. Tests cover exact files and digests, malformed samples, JSON-escaped credential markers, stale file-selection state, request/replay, and receipt-only public status. Full `scripts/check-all.sh`, connected node/browser rehearsal and fresh adversarial review passed. Build limits and content heuristics are safeguards, not rights or personal-data proof.

- U6A frontend passed 2026-10-22: [ADR-0041](docs/adr/0041-market-frontend-sandbox.md) defines a complete navigable but local prediction-market concept UI. Browser tests exercise the explore/detail/math-only preview/empty-position/resolution/draft screens at desktop and phone widths, and assert no simulated positions, network orders, quotes, or redemption. U6B remains open; this prototype does not determine whether Onyx should build a backed prediction market.


- U6B progress 2026-10-24 (partner tracker T10, ADR-0042, by hand): testnet contracts for staked
  submissions (observation first, then a 2/3 stake vote with a quorum, then a fallback oracle) and
  parimutuel futures that settle only from level 1-2 facts accepted after the value can be seen, so
  this plan's rule holds: no market settles from a price or a stake majority. 45 contract tests,
  a demo ledger, and Markets page simulators that run the same integer rules (41 shared vectors).
  Not deployed; the binary Yes/No order book of this plan is not built (parimutuel pools instead);
  the drawn-panel oracle, custody review and legal review remain open.

## Markets UI scope (October 22)

The owner asked for the futures/predictions idea to appear in the frontend and was unsure whether it
is the right product. `/markets` now offers a complete navigable **local prototype**: Explore, an
illustrative future-data question, a user-entered math-only Yes/No limit calculation, empty positions,
evidence/resolution/redemption states, and an unsigned term builder/export. It shows no live listing,
quote, liquidity, activity, balance, order, evidence, or position. Redemption remains unavailable.
The example is only about a named provider's published sample, not global model usage. The page
explicitly separates forecasts of future observations from the unresolved idea of staking on whether
existing contributed data is correct.

The call discussion (timestamps and first-party resolution-system research in RESEARCH.md) supports
the data network and independent verification as the near-term direction. It explored both validity
staking and later futures, while naming manipulation/duplicate-reward risks and deferring a full
prediction system. U6A therefore completes the UI prototype only. U6B is open: decide product fit,
then separately design and build a registry, collateral, oracle/dispute/finality, positions, void and
settlement path with its own threat model and tests. The homepage data invitation remains primary;
Markets stays discoverable in navigation.
