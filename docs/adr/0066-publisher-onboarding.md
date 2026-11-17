# ADR-0066: Publisher onboarding on the test network (READINESS B1, ROADMAP S27)

## Status
Proposed (draft 2), 2026-11-17, after design review PUBON-R1 ("build with changes": 2 High,
8 Medium, 5 Low; every finding checked against the code, all taken, three with a change:
section "What draft 2 changed"). Slices 1 (the application and the doctor), 2 (limits per
publisher on the node, item 10) and 3 (the book read from a chain node, the reviewer's tool, the
record, the fetch job, the rehearsal) are built: "Build notes" below. Slice 4 is not. Test
network only. No contract changes. Threats: THREATS.md "PUBON".

## Context
An outside person cannot become a bulk-data publisher alone. The newcomer test (READINESS A8)
found that publishing "stops at 'Ask the Onyx operator'".

What exists today:

- **The tools.** `onyx-publish` (`keygen`, `keys-doc`, `build`, `sign`, `anchor`, `upload`) and
  `onyx-check --bulk`. Guide: docs/ops/bulk-publish.md.
- **The gate.** A node keeps a manifest only when the signed book shows its report recorded and
  its key active (ADR-0059 item 6). A key enters the book only through `registerKey(source,
  signer, origin, provenance, reason)`, which only the book's one immutable reviewer key may call
  (`contracts/src/OnyxSignedBook.sol`). ADR-0059 item 7: "the reviewer's key registration is the
  review."
- **The reviewer's tool.** The `book-reviewer` example of `onyx-level2` sends that call from a
  key file. The guide's "For the operator" section has two sentences on what to check.
- **The contact step.** A public GitHub issue with three lines: id, domain, address.
- **The publishers registered so far** were registered by the reviewer by hand
  (docs/ops/deployment.md lists them).

Missing: a defined flow, a request a reviewer can verify offline, a checklist, a status the
applicant can see, a tool that says what is missing, and limits per publisher.

Six facts in the code shape the design:

1. **The book never forgets.** `registerKey` binds one id to one origin and one signer to one key
   for good (`originOfSource`, `sourceOfOrigin`, `keyOfSigner`). No call removes a publisher.
2. **A second key needs only the origin.** `registerKey` takes another signer for a registered id
   when the origin is the same, and a node takes the next version from a new key (ADR-0059
   item 6). So whoever holds the origin later (an expired domain) could ask for a key of the id.
3. **The reviewer cannot pause a key.** Only the book suspends, after an equivocation. The
   reviewer can `retireKey(id, at)`: final, and every report of that key observed at or after
   `at` stops counting, on every node and for every reader. An early `at` withdraws all of a
   publisher's data: per key, never per dataset. `markCompromised` is as final.
4. **The provenance hash is only an event.** `registerKey` emits the keys document's Keccak in
   `KeyRegistered` and stores nothing ("not checked here"). Nothing fetches an origin again.
5. **Limits are per node.** `ONYX_BULK_QUOTA_GIB` and `ONYX_BULK_MAX_DATASETS` give every
   registered publisher the same numbers: on the hosted node 16 GiB and 50,000 datasets each,
   24 GiB in all (`scripts/deploy-api.sh`); the first publisher holds 10.43 GiB (READINESS A4).
   Whoever fills the node makes every other publisher's upload fail with `bulk_node_full`.
6. **Anyone may relay a signed report and upload a manifest.** `postReport` checks no sender and
   `POST /v1/bulk/manifests` takes no credential. A report is refused once it is 7 days old
   (`MAX_REPORT_AGE_SECS`); an older manifest can still be anchored through a catalog (ADR-0059
   amendment 1 items 3 and 7).

A lighter path stays as it is: **proposing** a dataset (the website's Add data page,
`/v1/submit`, the AI connector's propose tool; ADR-0038, ADR-0039, ROADMAP S1 and S5). A proposal
publishes nothing and needs no key.

## Decision

### 1. Two paths, named apart
- **Propose**: "I know of data Onyx should have." Five questions, no key, nothing published.
- **Publish**: "I hold data and will sign it." A key, an application, a review, then self-serve.
  This ADR covers Publish only (READINESS B2 covers the proposal queue).

### 2. The applicant's steps

| # | Step | Artifact | Leaves the machine? |
|---|---|---|---|
| 1 | `onyx-publish keygen`, twice: a working key and a spare kept offline (item 12) | two key files (mode 0600) | Never |
| 2 | Choose a publisher id and an origin (items 3 and 4) | two strings | Yes, public |
| 3 | `onyx-publish keys-doc`, then put the file on the origin | `onyx-source-keys.json` | Yes, public |
| 4 | Build and sign a small first dataset: `build`, `sign` (item 5) | a version folder | Yes, public |
| 5 | `onyx-publish apply` (item 6); put the file on the origin | `onyx-publisher-application.json` | Yes, public |
| 6 | `onyx-publish doctor` until it prints no problem (item 11) | none | No |
| 7 | Point the reviewer at the origin (item 7) | one issue of two lines | Yes, public |
| 8 | Wait for the decision; read the status (item 9) | none | No |
| 9 | `upload` the first dataset (the reviewer's tool anchored it, item 8) | none | Yes, public |
| 10 | `onyx-check --bulk` on the served rows, as the guide's step 10 says | a report | No |

Steps 1 to 7: about 30 minutes. Step 8: up to 5 days. Steps 9 and 10: about 10 minutes. After
that the publisher works alone, inside its limits; from its second anchor on it needs test ETH
from a faucet (one `postReport` is about 250,000 gas).

### 3. The publisher id
- **Grammar**: `onyx_core::ids::is_dotted_id`, as today: lowercase letters, digits, `-` and `_`,
  segments joined by single `.` or `:`, at most 128 bytes.
- **Neutral**: the rule of the guide's step 1 stays. The id describes the data. It names no
  person, company, product or project, and never Onyx. Example (made up): `river-levels.open`.
- **Reserved words**: the tool cuts the id into words at `.`, `:`, `-` and `_`. It refuses the
  words `official`, `verified`, `reviewer` and `operator`, and any id that contains `onyx`
  anywhere. So `onyx-official.data` is refused. The reviewer still reads the id.
- **Look-alikes**: the tool reads the book's `KeyRegistered` events and refuses an id that equals
  a registered one after it drops `.`, `:`, `-` and `_`. The same id with the same origin is not
  a look-alike: it is a second key (item 12).
- **Squatting**: no id is reserved by asking. An application needs a real first dataset (item 5)
  and expires (item 6). A refused, withdrawn or expired application holds nothing. Of two valid
  applications for one id, the first valid one received wins. The reviewer registers one id per
  application and never "to keep it safe".

### 4. The origin, and what proves control
The origin is where the keys document lives. It is the only thing a reader can fetch again to
check that a key belongs to the id.

- **Form**: an HTTPS host, or a host and one path (`data.example.org/river-levels`), as the guide
  allows today. The document is at `https://<origin>/.well-known/onyx-source-keys.json`.
- **What counts as control**: the applicant can put that file there, and it names the id and the
  address. For a first key that is enough. For a later key it is not (fact 2; item 12).
- **What does not count**: a signed statement alone. It gives a reader nothing to fetch.
- **Neutral too**: no person or known organisation in the origin.
- **No overlap**: a new origin is refused when it is a prefix of a registered origin or a
  registered origin is a prefix of it (`example.org` and `example.org/a`): whoever holds the
  shorter one can write under the longer one. The origins registered before this rule stay.
- **Shared pages hosts**: allowed only where a path cannot be registered again by someone else
  once it is given up. The tool carries the short list of such hosts; any other shared host is
  refused. The page and `doctor` warn: "lose this path and you lose the proof of your id."
- **One origin, one id**: the book enforces it (`OriginTaken`, `OriginMismatch`).
- **A gap this design closes**: `onyx_core::signed::is_origin_host` takes a host only, while the
  guide allows a path. Slice 1 writes one origin grammar into code, and every tool uses it.

### 5. The first dataset is the sample
The applicant builds and signs version 1 of one real dataset, small: one shard, at most 8 MiB.
It puts the version folder on the origin, next to the keys document.

- The check needs no chain and no node: schema, rows, roots, the manifest's rights
  (`Redistribution::Allowed`, the licence id, the fixed statement `onyx-bulk-rights-v1`) and the
  signature over the manifest root.
- **It is not a throwaway.** Its signed report is public. Once the key is registered anyone can
  relay it (fact 6). A different version 1 of that dataset id would then be an equivocation, and
  the book would suspend the key (ADR-0059 item 3). So the sample is version 1 of its dataset,
  and `build` refuses another version 1 for a dataset id that a local application carries.
- **The reviewer's tool anchors it**: in the same run as `registerKey` it relays the sample's
  report (item 8). Nobody else can get there first with anything but the same report.
- **A sample that went stale** (older than 7 days at approval) cannot be relayed by anyone. The
  publisher keeps the same folder and anchors it through its catalog 0. It never builds the rows
  again: a new build has a new creation time and so a new root.

### 6. One request: the publisher application
`onyx-publish apply` writes `onyx-publisher-application.json` (under 4 KiB):

```json
{"version": 1, "chain_id": 84532, "book": "0x<the signed book>",
 "publisher": "river-levels.open", "origin": "data.example.org/river-levels",
 "signer": "0x<address>", "signers_document_keccak": "0x<32 bytes>",
 "data": "Daily water levels of 40 public gauges, from the publisher's own sensors.",
 "sample": {"dataset": "river-levels.daily", "manifest_root": "0x<32 bytes>", "bytes": 412310,
            "path": "sample/"},
 "asks": {"quota_gib": 1, "datasets": 16},
 "created_unix": 1794873600, "expires_unix": 1795392000,
 "signature": "0x<65 bytes>", "cosignature": null}
```

- **Signature**: EIP-191 over the text `onyx-publisher-application-v1:0x<digest>`; the digest is
  `keccak256("onyx-publisher-application-v1" ‖ borsh(every field but the two signatures))`
  (ADR-0002, ADR-0003). The domain differs from `onyx-signed-source-v1`, so an application can
  never be posted to the book as a report.
- **Bound to one book**: `chain_id` and `book` are signed.
- **Expiry**: `expires_unix` is 6 days after `created_unix`, and a first application's sample
  must be built within the hour before. So an application in time always has a sample the tool
  can still relay. An application after an expiry carries the old sample (items 9 and 12).
- **No rights block.** The manifest already carries the rights and the signed statement
  (ADR-0059 item 7); `verify-application` prints them for the reviewer. Nothing is stated twice.
- **No personal data**: no name, contact or wallet field. Unknown fields are refused. `data` is
  at most 500 bytes; e-mail-shaped text and `mailto:` are refused, as in the proposal intake
  (THREATS SUB-4). The sample is named by a path under the origin, never by another host.
- **`cosignature`**: only for a second key (item 12): the same text signed by an active key of
  the id.
- **Offline check**: `onyx-publish verify-application --origin <origin>` fetches the three files,
  recovers the signer and runs every check of item 8 that needs no person. `--rpc` adds the book.

The same shape, `onyx-publish request`, signs what a publisher asks for later: `withdraw`,
`raise`, `key-stolen`, `retire-key` (item 12). A plain issue without one changes nothing.

### 7. How the application travels: a pointer
The applicant never sends the application. It sends where the application is.

- **Now**: one public issue, titled `Publisher application: <publisher id>`, with two lines: the
  origin and the Keccak of the application file. The route exists (READINESS A17).
- **A CI job fetches it** (`.github/workflows/publisher-application.yml`): on such an issue it
  runs `verify-application` and the anvil rehearsal on a hosted runner and keeps the report and
  the fetched bytes as a build artifact. The reviewer reads the artifact and never connects to an
  applicant's host. The job gets no secret and a read-only token; the issue's text reaches the
  tool as a file, never as part of a command (THREATS PUBON-15).
- **Later (slice 4)**: `POST /v1/publishers/pointer` on the node takes `{"origin": "..."}` and
  nothing else: no account, no applicant bytes stored, the node fetches nothing. It keeps a
  bounded list of origins under the proposal intake's budgets (THREATS SUB-2); the same CI job
  reads the list on a schedule.

### 8. The reviewer's steps and checklist
The reviewer decides within 5 days. The application lives 6 and a report 7.

Run by the tool in CI (a failure is a refusal with its line):

1. Both hashes match: the file at the origin and the issue's line. The signature recovers
   `signer`; `chain_id` and `book` are this network's; the application has not expired.
2. The keys document names this id and this address, and its Keccak equals
   `signers_document_keccak`. Every fetch is HTTPS, follows no redirect, connects only to a public
   address and stops at a byte cap (the rules of THREATS SUB-1 and SUB-11).
3. The id passes the grammar, the reserved words and the look-alike check; the origin passes its
   grammar, the overlap rule and the shared-host list. The book shows id, origin and signer free.
   **An id already bound to this origin passes only with a valid `cosignature`** from a key the
   book shows active and not suspended, and with both keys in the keys document.
4. The signer is in no refused record that still blocks it (item 9).
5. The sample is at most 8 MiB, fits its schema, hashes to `manifest_root`, carries `Allowed`
   and the fixed statement, and its report is signed by `signer`.
6. The rehearsal (`scripts/publisher-onboard-anvil.sh`): register the address in a local book,
   relay the sample's report, upload to a local node, read rows, `onyx-check --bulk` exits 0.

Read by a person (each answer goes into the decision file):

7. **Neutral names**: the id and the origin name no person, company, product or Onyx.
8. **Not someone else**: nothing in the id, origin or `data` reads as a known organisation's
   voice. An applicant who is that organisation still gets a neutral id.
9. **Rights**: the licence fits the data. For data the applicant did not make, the source's
   terms are re-read and quoted in the decision (docs/spec/rights.md "Assumptions").
10. **Entities only**: the sample's rows and column names are read. No row is about one person,
    also not under a company or project name (THREATS SUB-5).
11. **Not illegal to host**: a plain look at the sample's text columns.
12. **Operator grouping**: `declareOperator` works only before a source's first key. The reviewer
    decides now whether this publisher belongs to a declared operator (ADR-0036 amendment 3).
13. **Limits to grant**: at most what `asks` says; by default 1 GiB and 16 datasets (item 10).

Then one run of `book-reviewer approve --application <artifact>`:

14. It repeats steps 1 to 5 on the artifact's bytes and sends nothing on a failure.
15. It writes the publisher's entry into the node's limits file first, so a registered key never
    meets a node without its limit.
16. It sends `registerKey` (id, address, origin, keys-document hash, a reason naming the
    decision), then relays the sample's report with `postReport`. A report already recorded with
    the same digest counts as done. A stale sample is not relayed (item 5).
17. **Gas**: the reviewer key pays `registerKey` (only it can call it); the relay, about 250,000
    gas, may come from any funded key. The operator sends no test ETH to a publisher: a transfer
    would link the keys on chain, and the supply is short (READINESS A11).
18. It writes the decision record. The reviewer key never goes on the node.

### 9. What is public, and the status
One folder in this repository per application, `deploy/publishers/<publisher id>/`:

- **Approved**: `application.json` (the bytes from the artifact) and `decision.json`: state,
  date, the checklist answers 7 to 13, the limits, the two transaction hashes and blocks.
- **Refused, withdrawn or expired**: `decision.json` only, with the application's Keccak, the
  state, the date and a reason code. No `data` text, no sample path, no bytes of the sample. For a
  refusal it also holds the signer's address and `blocks_signer_until`: the creation time of the
  refused sample plus 7 days and 300 seconds. Until then that key is refused (step 4), because
  its refused report could still be relayed once the key is registered (fact 6). After that the
  report is stale for good. A new key is never blocked. A new dataset id would not help: the old
  report stays valid under the old key. An expired record holds the signer's address and the
  sample's manifest root: a later application of that key must carry the same root.

States: `received`, `approved`, `refused`, `withdrawn`, `expired`, `paused`, `retired`. Reason
codes: `name-not-neutral`, `name-taken`, `origin-unreachable`, `origin-overlap`, `sample-failed`,
`rights-unclear`, `personal-data`, `not-hostable`, `duplicate`, `no-capacity`, `no-cosignature`.

`scripts/check-publishers.py` (run by check-docs, offline) verifies every folder: the
application's signature, the fields a state must and must not hold, no e-mail-shaped text. An
application without a decision after 5 days is a printed warning, never a failed check: a clock
alone must not turn the build red. With `--rpc` (the reviewer, after each decision) it requires
that the book's `KeyRegistered` event equals each approved file. It writes
`apps/web/static/publishers.json`, which the site and `doctor` read. The book stays the source of
truth for the key; the folder adds what the book cannot hold: why.

A spam pointer gets no folder; its issue is closed with one line.

### 10. Limits per publisher
The node gets a limits file, `ONYX_BULK_PUBLISHERS` (JSON: `reserve_gib`, and per publisher id
`quota_gib`, `datasets`, `state`: `open` or `paused`). With it:

- A publisher gets its entry's numbers. They replace `ONYX_BULK_QUOTA_GIB` and
  `ONYX_BULK_MAX_DATASETS`, which apply only on a node without the file. An entry's `datasets`
  may go up to the catalog's 50,000 leaves; only the first publisher's entry has that. A new
  publisher's entry is 1 GiB and 16 datasets.
- **A registered publisher without an entry is refused** (`bulk_publisher_not_listed`).
  Registration in the book is not a grant of disk; each node's operator lists whom it takes.
- **Grants are summed.** The node refuses to start when the entries' `quota_gib` plus
  `reserve_gib` exceed `ONYX_BULK_MAX_GIB`. So no grant can push another publisher into
  `bulk_node_full`. On the hosted node (24 GiB, 16 held by the first entry) that leaves room for
  a few new publishers; after that the answer is `no-capacity` until the disk grows.
- **Paused**: the node takes no manifest and no shard from that publisher and keeps serving. It
  still takes a catalog in which every leaf is unchanged or becomes a tombstone, so a paused
  publisher can take data down.
- A raise needs a signed `raise` request and is recorded in `decision.json`.

A node without the variable behaves as today (storage nodes, local rehearsals). No outside key
is registered on the hosted book before slice 2 is live there (THREATS PUBON-6).

### 11. `onyx-publish doctor`
A dry run that sends nothing. One line per check (`ok`, `missing` or `problem`) with the next
command to run: the key files (present, mode 0600, a spare exists); the id and the origin (every
rule of items 3 and 4); the keys document and the application (live at the origin, equal to the
local files, not expired); the sample (verifies, version 1, under 8 MiB, time left until stale);
the state in `publishers.json`; the book (`keyState`; id and origin free or bound to this pair);
gas (test ETH for one anchor; a warning if the address was funded from one with other history);
the node (answers `GET /v1/bulk`, and its book is the application's book).

### 12. Keys after approval, pausing, removing, appealing
Every request below is a signed `onyx-publish request` at the origin plus a pointer issue. The
reviewer's tool checks it as it checks an application.

- **A second key (rotation, or the spare)**: an application for the same id and origin with a
  `cosignature` from an active key of that id, and a keys document that lists both keys. Origin
  control alone gives nothing (fact 2). The guide tells every publisher to register its spare
  soon after approval.
- **Retire a key**: a `retire-key` request signed by that key or by another active key of the
  id. The reviewer calls `retireKey(id, at)`; what the key signed before `at` keeps counting.
- **The only key is lost**: there is no rotation. No one can prove the request, and the origin
  alone must not be enough. The data already published stays served. The publisher starts a new
  id on a new origin (the old pair is bound for good) and may publish the history again there.
- **A key is stolen**: one `key-stolen` request, signed by the stolen key, with the theft time,
  and a keys document at the origin that no longer lists that key. Both are needed: a thief has
  the key but not the origin; whoever took the origin has no key. If the request also carries a
  new key's application, cosigned by the stolen key, the tool registers the new key **first** and
  then calls `markCompromised`; after it, no cosignature of that key counts. The catalog steps of
  the guide's "A stolen key and catalogs" follow. A claim with only one of the two proofs leads
  to a pause on the node, never to a book call: `markCompromised` cannot be taken back.
- **The publisher takes data down**: a tombstone (`catalog-build --remove`, ADR-0059
  amendment 4). This is the first thing the reviewer asks for; it works while paused (item 10).
- **Pause**: `state: paused` in the limits file. Reversible; the book is not touched.
- **Stop serving one dataset** (personal or illegal data, a rights complaint, the publisher does
  not act): the node operator's existing step (the guide's "For the operator"). Per node.
- **Withdraw a publisher everywhere** (a confirmed, serious breach): `retireKey(id, at)` with
  `at` at or before the first offending report. Every report of that key from `at` on stops
  counting on every node and for every reader (fact 3). It is final, and it takes the
  publisher's good datasets from `at` on too: the book withdraws per key, not per dataset.
- **A suspended key** (an equivocation): the reviewer reads both reports, resolves the fact and
  calls `reinstateKey` when the cause is understood.
- **Re-apply**: fix the named reason and apply again; after a refusal with a new key, or after
  `blocks_signer_until` (item 9). After an expiry the same key and the same sample folder are
  used again, so version 1 keeps one root.
- **Appeal**: one new issue that names the decision; the reviewer answers once, in public. There
  is no second reviewer. A refused applicant can run its own book and node with the same tools.

### 13. What stays manual, what is automated, what comes later
- **Manual on the test network**: checklist lines 7 to 13, the decision to approve, pausing and
  retiring. Why: a registration cannot be undone, and "is this a person" and "may this be shared"
  have no mechanical answer (THREATS SUB-3, SUB-5).
- **One reviewer, one key**: the book's reviewer is immutable. If the key is lost or its holder
  is away, no application is answered, no stolen key is marked and no suspended key reinstated.
  Who stands in is the open question below. The page says so.
- **Automated now**: the application and later requests, `doctor`, the CI fetch and rehearsal,
  the record's check, the status file, the limits and their sum.
- **A later self-serve version** would need: a book whose reviewer is more than one key (ROADMAP
  H1); a withdrawal per dataset, which the book lacks; and something a bad publisher loses. The
  open economic question: can a stake or bond replace the review of names and rights? A bond
  prices spam and disk. It does not price harm to a third party whose data or name was used, and
  nobody is named yet who would judge a slash. This ADR does not answer it.

### 14. Privacy
- **What the application reveals**: an id, an origin, an address, one sentence about the data and
  a small sample. All of it becomes public through the book and the node anyway.
- **What the route reveals**: a public issue shows the account that opened it, and a domain's
  registration record can name its owner. The page says both. Slice 4's pointer needs no account.
- **What the applicant learns**: a hosted runner's address. The reviewer's machine and the node
  never connect to an applicant's host.
- **What the operator must not keep**: no client address, no e-mail, no account name in
  `deploy/publishers/`, no private notes about an applicant, nothing of a refused application
  beyond item 9's fields. CI artifacts of a refused application are deleted with the decision.

## Slices
Each slice ends with a fresh adversarial review. No slice changes a contract.

1. **The application and the doctor** (`crates/onyx-publish`, a module `onyx_bulk::application`,
   docs): `apply`, `request`, `verify-application`, `doctor`, the origin grammar, the issue
   template, docs/ops/publisher-review.md (the checklist), the guide rewritten. Tests in
   `application::`: `round_trips_and_the_signature_binds_every_field`,
   `another_book_or_chain_or_an_expired_one_is_refused`, `is_never_a_valid_book_report`,
   `unknown_fields_and_contact_text_are_refused`,
   `a_second_key_needs_a_cosignature_of_an_active_key`,
   `a_cosignature_of_a_retired_compromised_or_suspended_key_is_refused`,
   `a_refused_signer_is_blocked_until_its_report_is_stale`,
   `key_stolen_needs_the_key_and_the_changed_keys_document`; in `ids::`:
   `reserved_words_match_inside_segments`, `the_same_id_and_origin_is_not_a_look_alike`,
   `an_origin_that_is_a_prefix_of_another_is_refused`; in `onyx-publish::`:
   `doctor_names_each_missing_step_and_sends_nothing`,
   `verify_refuses_a_redirect_a_private_address_and_an_oversized_sample`,
   `a_sample_must_be_version_one_and_one_shard`.
2. **Limits per publisher** (`onyx_api::bulk_serve`, `onyx-node` config, docs/ops/config.md).
   Tests in `bulk_serve::tests::`: `a_publisher_gets_its_entry_and_one_without_an_entry_is_refused`,
   `a_paused_publisher_is_served_and_takes_no_upload`,
   `a_paused_publisher_may_still_post_a_tombstone_catalog`; in `config::tests::`:
   `grants_plus_the_reserve_over_the_total_stop_the_start`, `a_bad_publishers_file_stops_the_start`.
3. **The record, the reviewer's tool and CI** (built: "Build notes"; `deploy/publishers/` with the registered
   publishers as records, `scripts/check-publishers.py`, `book-reviewer approve`,
   `scripts/publisher-onboard-anvil.sh`, the workflow). Tests:
   `book_reviewer::refuses_to_send_when_the_application_fails`,
   `book_reviewer::writes_the_limit_before_it_registers`; the anvil script (apply, verify, approve
   with the relay, upload, `onyx-check --bulk`; a stale sample goes through catalog 0; a second
   version 1 suspends the key; a second key with and without a cosignature; a re-apply after a
   refusal: refused with the same key, taken with a new one); `check-publishers.py --self-test`
   (a changed application, a refused record that keeps the data text, e-mail-shaped text, an
   approval the book does not show, a late decision only warns); `scripts/check-workflows.py`:
   the workflow has no secret and no issue text in a command.
4. **The website and the pointer route** (`apps/web`, then `onyx_api`). e2e `tests/publish.mjs`:
   "the Add data page names both paths", "the publish page shows every step with its command",
   "a status lookup shows the state and the reason", "a refused application shows how to apply
   again"; `publishers::tests::a_pointer_keeps_only_an_origin_and_fetches_nothing`.

## Build notes

### Slice 1 (2026-11-17): the application and the doctor
Built: `onyx_bulk::ids` (the id rules and the one origin grammar), `onyx_bulk::application` (the
application, the later request, every offline check, the sample check), and in `onyx-publish`
the commands `apply`, `request`, `verify-application` and `doctor`; the issue template
`.github/ISSUE_TEMPLATE/publisher-application.md`; docs/ops/publisher-review.md; the first steps
of docs/ops/bulk-publish.md. `keygen` existed and is unchanged. Every named test of slice 1
exists under its name (the three `ids::` tests in `onyx_bulk::ids`, the three `onyx-publish::`
tests in `onboard::tests`).

What the build fixed that the text above leaves open:

- **Where the files are.** `https://<origin>/.well-known/onyx-publisher-application.json` next
  to the keys document, a later request at `.well-known/onyx-publisher-request.json`, the sample
  at `https://<origin>/<sample.path>` (`manifest.bin`, `report.bin`, `shards/<root>.bin`). The
  applicant keeps one folder, `site/`, that mirrors the origin; `verify-application --dir` reads
  the same tree, so the reviewer checks a saved copy offline with the same command.
- **The origin grammar** (item 4): a lowercase DNS host (`onyx_core::signed::is_origin_host`,
  unchanged: level-2 sources use it) and at most 4 path parts of lowercase letters, digits, `-`
  and `_`; no scheme, port, query or trailing slash. It lives in `onyx_bulk::ids`, not in
  `onyx-core`: only the bulk tools register an origin with a path. The registered origins
  (a host and one path part each) parse under it and do not overlap.
- **Shared hosts** (item 4): the tool refuses an origin on a short list of shared hosts. The
  list of shared hosts where a given-up name can never be taken again is empty: none was shown
  to promise that, so none is allowed yet.
- **A second key's sample** (items 6 and 12): the application still names a sample, the id's
  first dataset; its report may be signed by the cosigning key, and the one-hour rule does not
  apply. The same waiver holds for a re-apply whose expired record names the same root.
- **The request** (item 12): its own domain, `onyx-publisher-request-v1`, so a request is
  neither an application nor a book report. `withdraw` is checked for its signature only (the
  reviewer matches it to the pending application); `raise` needs an active key of the id;
  `retire-key` the key itself or another active key; `key-stolen` both proofs.
- **Refusal codes**: the tool's report uses item 9's reason codes where one fits and its own
  for what item 9 does not name (`malformed`, `bad-signature`, `wrong-network`, `expired`,
  `bad-time`, `keys-document`, `signer-blocked`).
- **`doctor`** prints a fourth word, `skipped`, for a check it did not run.

Not built in slice 1, said by the tools where it matters:

- **`--rpc`** on `verify-application` and `doctor`. The book's keys are handed in as a file
  (`--registered`: id, origin, signer, state per key); without it the report lists the book
  under `notChecked`. Reading `KeyRegistered` and `keyState` from a chain node moves to slice 3
  with the reviewer's tool, which needs the same read.
- **`doctor`'s gas and node lines** (they need the chain and the node) and its status line
  (`publishers.json` is written by slice 3): printed as `skipped`.
- **The record** (item 9): slice 3 writes `decision.json`. Slice 1 only reads the fields that
  block: `{"state": "refused", "signer", "blocks_signer_until"}` and `{"state": "expired",
  "signer", "sample_manifest_root"}`.

### Slice 1 after review APPLY-R1 (2026-11-17)
A fresh review of the built slice ("ready with changes": 2 High, 8 Medium, 5 Low; THREATS
"APPLY-R1") held the signature binding, the domain separation, the fetch guard and the
dependencies. Its findings changed the build as follows; where this differs from the text
above, this section is what the code does.

- **Reports and blocks (item 9).** `verify-application` names the signer, id and origin only
  once the signature is the signer's, and every report has `blocksSignerUntil`: a number only
  when the signature and the pointer's hash passed, else `null`. A refusal record takes its
  signer and block from the report alone. Otherwise anyone could copy a pending applicant's
  public file, break it, point at it and get that applicant's key blocked.
- **Exit 4.** A run that passed every check it ran but did not read the book (`--registered`
  with at least one key) or the records (`--decisions`) exits 4 with `"ok": false`. Exit 3 now
  also covers an origin that answers 429 or a 5xx, and bytes that `--save` could not keep.
- **The registered list** must hold every origin in the registered form; an entry that does
  not parse stops the tool instead of being skipped. The report echoes the list's count and
  Keccak.
- **Ids (item 3).** The reserved words and `onyx` are matched anywhere in the id after the
  separators are dropped and digits are read as the letters they stand for (`0` o, `1` and `i`
  l, `3` e, `4` a, `5` s, `7` t; `ks` as `x`), so `on-yx`, `0fficial` and `officials` are
  refused. The look-alike rule compares the same folded form with each word's plural `s`
  dropped. The price: an honest id that contains such a word (`cooperator`) is refused too.
- **Shared hosts (item 4).** The tool carries a list of shared hosts compiled by hand for Onyx
  (`crates/onyx-bulk/data/shared-hosts.txt`, 131 suffixes with its date and provenance;
  never fetched). An origin is refused when its host ends in, or contains as whole labels, one
  of them, or has a `blogspot` label. A host with an `xn--` label is refused. The review offered
  the Public Suffix List's private section instead; that list is under MPL-2.0 and the
  repository ships no copyleft material (`deny.toml`), so it is not vendored. The price: the
  hand list is not complete, a shared host missing from it passes the tool, and the reviewer
  reads the host (checklist line 7).
- **The keys document (items 6 and 12).** A first application's document lists exactly the
  signer; a further key's document lists the new key and keys the book holds for the id, each
  once. Every JSON file the tools read refuses a field that stands twice.
- **Requests (item 12).** A request carries a `sequence` (a request is taken once: none at or
  below the last one taken from the key) and, for `withdraw`, the Keccak of the application it
  withdraws; only that application's key may send it. The id and the origin of a request are
  in their grammar. `retire-key` takes a time from one hour before the request up to the
  request's expiry; an earlier time needs the second proof of a theft (a keys document that no
  longer lists the key), because `retireKey` with an early time withdraws as finally as
  `markCompromised`. `key-stolen` is refused for a key that is retired or compromised already.
  `onyx-publish verify-request` reads a request from a folder or the origin and checks it.
- **A theft with a move to a new key needs two fetches** (item 12 says "registers the new key
  first"): the cosigned application passes while the keys document lists both keys, the theft
  request only after the stolen key is taken out. docs/ops/publisher-review.md has the order.
- **`doctor`** takes a spare whose address `keygen` noted (`<key file>.address`) while the key
  file is offline, and takes an application made with `apply --reapply` on the applicant's
  word; the reviewer's tool still wants the expired record. `keys-doc --also-address` lists a
  further key by its address.

**Slice 2, 2026-11-17** (`crates/onyx-api/src/bulk_publishers.rs`, `onyx-node` config,
`scripts/deploy-api.sh`; operator text in docs/ops/run-api.md "Limits per publisher"). Built as
item 10 says, with these choices where the item was silent:

- **The file**: `{"reserve_gib": N, "publishers": [{"id", "quota_gib", "datasets", "state"}]}`.
  The entries are a list with an `id` each, not an object keyed by id: a JSON object with one key
  twice reads as one entry, and a second entry for an id must stop the start. Every field is
  required, no other is taken, numbers are whole GiB and whole datasets.
- **Read at start only.** A change needs a restart, and `onyx-node --check-config` checks a file
  first. So slice 3's tool writes the entry and the node is restarted before `registerKey`.
- **Codes**: 403 `bulk_publisher_not_listed` (the name item 10 gives) and 403
  `bulk_publisher_paused`. A publisher without an entry, and a paused publisher's new data, are
  answered with no chain read.
- **Paused** means "stores nothing new": no new version, no version in a withdrawn one's place,
  no new shard. A catalog is taken when every leaf is a tombstone or equals its leaf in the held
  catalog before it; a first catalog of tombstones over own-report versions counts. A links
  version (ADR-0059 amendment 6) is a manifest like any other, so a paused publisher removes a
  dataset but adds no successor link until it is open again.
- **A pause hides nothing** (review LIM-R1 M3, L2): a manifest or catalog for something the node
  already holds is read against the book as for any publisher. The same one again is 200, a held
  root is re-anchored under a new key, a second anchored manifest or catalog marks the fork, and
  catalogs the book withdrew are set aside (their replacement waits until the publisher is open).
- **A takedown is not held to the publisher's own limits** (LIM-R1 M1): a catalog that only
  removes (every leaf unchanged or a tombstone, at least one tombstone new) passes the dataset
  cap and the publisher's bytes; the node's total still holds.
- **What is held counts at start** (LIM-R1 M2): each publisher counts with the larger of its
  grant and its held bytes, a publisher without an entry with what it holds; a file that does
  not fit beside that stops the start and names them. The start logs each such publisher, and a
  file that lists nobody.
- **Deploy** (LIM-R1 H1, M4): `scripts/deploy-node-gate.sh` runs on the server before anything
  changes. An export without the file is refused when the server has one
  (`ONYX_API_LIMITS_OFF=1` to mean it), and the new binary's `--check-config` reads the new
  settings before the old node is stopped.
- **Not listed** refuses a takedown too: the runbook says pause an entry, never delete it.
- **Nothing is shown.** No route lists an entry; a refusal names the asking publisher's own
  limit only.
- **Storage nodes**: the setting is refused in storage mode (LIM-R1 L3): a storage node takes
  the data of its deals, whoever published it.
- Tests: the five named in slice 2, and `bulk_serve::tests::`
  `a_publisher_at_its_quota_does_not_affect_another`,
  `without_a_limits_file_every_publisher_has_the_nodes_one_quota`,
  `a_paused_publisher_removes_an_own_report_dataset_with_a_first_catalog`,
  `a_limits_file_with_unknown_fields_duplicates_or_bad_numbers_is_refused`; after review
  LIM-R1 the tests THREATS "LIM-R1" names.
- Not live: the hosted node gets the file with its next deploy (`<export>/publishers.json`).

### Slice 3 (2026-11-17): the chain read, the reviewer's tool, the record, CI, the rehearsal
Built: `onyx_api::bulk_registry` (the book's keys from a chain node), `--rpc` on
`verify-application`, `verify-request` and `doctor`, the command `book-reviewer` (a second
binary of the `onyx-publish` crate, which became a library with two commands), the record
`deploy/publishers/` with `scripts/check-publishers.py`, the status list
`apps/web/static/publishers.json`, `.github/workflows/publisher-application.yml` with its rules in
`scripts/check-workflows.py`, and `scripts/publisher-onboard-anvil.sh`. Where this differs from
the text above, this section is what the code does.

- **The chain read (items 3, 8).** `--rpc URL --chain-id N --book 0x… --from-block N
  [--logs-range N]`: every `KeyRegistered` log from `--from-block` to the node's finalized block
  (`decision_block`, the rule of every other bulk read; a node without a finalized block is not
  read), each log checked against `keyState` and `originOfSource` at that block. The range walk
  is the indexer's (`Spans`: a refused range is halved; a rate limit is waited out). What the
  list shows as free is read again from `keyOfSigner`, `originOfSource` and `sourceOfOrigin`:
  a scan that started after the book's first key ends with exit 3, never with a free name.
  `--rpc` beside `--registered` is refused. `--from-block` is required on every chain but a
  local one; the test network's book starts at block 52118336. **The price:** the public node
  answers 200 blocks a request and the chain makes about five blocks a second, so the book's
  whole history takes hours there. A reviewer needs a node that answers wide ranges; a cache of
  the scan is not built.
- **`doctor`** reads the book with `--rpc`, the key's balance against one anchor at the node's
  gas price (always `ok`: no test ETH is needed before approval), a node with `--node`, the
  status list with `--publishers`, and a folder copy of the origin with `--mirror`. Whether an
  address was funded from one with other history is not read: a chain node does not show it.
- **The tool's name and steps (item 8).** `book-reviewer approve --application <artifact>`, as
  the item says. The node reads its limits file only at its start (slice 2), so "one run"
  became two: the first writes the entry and stops with exit 6; after the node was restarted,
  the run with `--limits-live` sends `registerKey` and the relay and writes the record.
  `--limits-live` is the reviewer's word; no route shows a node's entries (changed after review
  S3-R1: the note below). `--dry-run` sends
  and writes nothing. The reviewer key pays the relay unless `--gas-key` names another.
  The reason of `registerKey` is `publisher application <keccak>`: a run repeated after a
  failure finds its own registration by it and goes on, and a key registered by any other
  decision stops the tool. The older `book-reviewer` example of `onyx-level2` (a bare
  `registerKey` through `cast`) stays for sources that are not bulk publishers.
- **Other commands.** `book-reviewer record` (received, refused, expired; the signer and its
  block only from the tool's own report), `request` (withdraw, raise, retire-key, key-stolen;
  each taken request is written down with its sequence, and `key-stolen` is sent only when the
  id has another active key at the finalized block, or with `--no-new-key`), `pause`, `resume`.
- **The record (item 9).** A registered publisher has `deploy/publishers/<id>/decision.json`, a
  list, oldest first, and `application.json` (`application-2.json` for a further key). **An
  application that is not approved is filed under `application-<16 hex of its Keccak>/`, not
  under its id**: item 9 keeps no id of a refusal, and a folder named by a refused id would
  publish exactly the name that was refused. So the status of such an application is looked up
  by its Keccak, which the applicant has. Field names are snake_case, as slice 1 reads them.
  A new state, `registered-before-record`, holds the three keys the book had before the record
  began (id, origin, key, keys document, transaction and block, read from the book): no
  application and no answers exist for them, and the record says nothing about who runs them.
- **The check (item 9)** is run by check-all next to the other checkers, not by check-docs. It
  verifies the application's signature in plain Python (no dependency), writes the status list
  with `--write`, compares a node's limits file with the record (`--limits`, `--max-gib`) and
  the record with the book (`--rpc`, three single-block log reads on the test network).
- **The CI job (item 7)** fetches and checks; it does not read the book (a hosted runner has no
  chain node and no secret to reach one), so its report ends with exit 4 at best, and it does
  not run the rehearsal on the applicant's sample. `verify-application --pointer FILE` reads
  the issue's two lines from a file. The job has not run on the hosted repository.
- **The rehearsal (item 8 step 6)** is `scripts/publisher-onboard-anvil.sh`, in check-all, about
  20 s once built. It rehearses the flow with a made-up applicant, not an applicant's own
  sample. Its steps: apply and doctor; verify with `--rpc`; a dry run; the limit, then
  `registerKey` and the relay; the record against the checker and the book; upload, rows,
  `onyx-check --bulk`; a refusal that blocks its key and a new key that is free; a second key
  with and without a cosignature; a withdraw taken once; a paused publisher's tombstone.
- Tests: the two named for slice 3 (`book_reviewer::` in `onyx-publish`), and the ones THREATS
  PUBON-19 names.

Not built in slice 3, against the list in "Slices":

- **On anvil**: a stale sample through catalog 0, and a second version 1 that suspends the key.
- **A raise, a retirement and a theft end to end on anvil**: `book-reviewer request` has unit
  coverage only for its record and its calldata; the withdraw is rehearsed.
- **The registrable domain** next to the shared-host verdict (THREATS APPLY-R1-M6).
- **A limits file in the repository**: the hosted node's file is not deployed yet, and the three
  earlier publishers' grants are not recorded, so `--limits` is run by the reviewer on the
  node's file, not by check-all.

### Slice 3 after review S3-R1 (2026-11-17)
A fresh review of the built slice ("ready with changes": no High, 6 Medium, 9 Low; THREATS
"S3-R1") changed the build as follows; where this differs from the note above, this is what
the code does.

- **`--limits-live` is gone.** `approve` takes `--node URL` and asks the node itself,
  `GET /v1/bulk?listed=<id>`, which answers `limits: {on, listed, publisher}` for that one id (a
  new field of the listing; no other entry and no number is shown). Limits off: refused before
  anything is written. No entry on the node yet: the entry is written to the file and the run
  stops with exit 6. The same command after the restart sends.
- **A closed application stays closed**: a file the record holds as refused, withdrawn or
  expired is refused by `verify-application` (`duplicate`) and by `approve`.
- **A run is taken up again only for an active key.** A key retired or compromised since
  stops the tool.
- **The scan starts at the book's deploy block**: pinned for the test network's book
  (`KNOWN_BOOKS`), proven by empty code one block earlier for any other. `--from-block` is no
  longer needed for the test network.
- **The scan is kept** (`--scan-cache`; by default under `ONYX_OPS_DIR` or
  `~/.local/state/onyx`, `book-scan/<chain>-<book>.json`, mode 0600) with the block it is valid
  to and its hash; `book-reviewer scan` fills it. The first scan of the test network's book
  through the public node still takes hours; every later one reads only the new blocks.
- **Requests**: a raise needs its named limit and never lowers one; a retirement and a theft
  are sent only by the book's reviewer; another key of the id is retired only with a keys
  document that no longer lists it (`onyx_bulk::application`).
- **The record's answers** are `yes` for the lines 7, 8, 9, 10, 11 and 13: no free text, and
  never line 12, which would say who runs a publisher. Item 9's "the checklist answers 7 to 13"
  is narrowed by this.
- **The status list** says `approved (record)` until a run with `--rpc` found the book's
  registration.
- **`--now`** is taken by `book-reviewer` on a local chain only.
- **Field names a secret scanner does not take for a credential.** The application's field for
  the keys document's Keccak is `signers_document_keccak` (the example in item 6 is renamed), a
  request names the key to retire as `retire`, a record entry as `ended_signer`: the public
  snapshot's gitleaks run reads a name with "key" in it and a hex value as an API key. No
  signed byte changed (the digests are over Borsh values, without names). The checker and the
  rehearsal run the scanner over the public files.
- **The rehearsal** has a tenth step (a raise, a theft with another key active, a theft of the
  last key refused, a retirement) and a node started without limits.

## What a newcomer sees on the website
- **Add data**: two cards. "Suggest a dataset: 5 minutes, no key, nothing is published." and
  "Publish your own data: about 40 minutes of your time, and a review of up to 5 days."
- **Publish page**: the ten steps of item 2, each with its command; what the review checks; "one
  person reviews, on a test network" first; what becomes public; the limits; the warnings about
  a lost key and a lost origin.
- **Status**: a box that takes a publisher id and shows its state, date, reason and transaction;
  under it the list of publishers.

## Consequences
- An outside person can build a verifiable request alone and see where it stands. The decision
  is still a person's.
- Taking over an origin no longer gives a key of a registered id. The price: a publisher that
  loses its only key loses its id.
- A new publisher cannot fill the node or push another into `bulk_node_full`.
- The repository gains a public record of every decision; of a refusal only a hash, a code and
  a blocked address.
- Still true: one reviewer key can refuse anyone; the book withdraws per key, never per dataset;
  a registration is permanent; nothing fetches a registered origin again, so a keys document
  changed later is noticed only by a reader who compares it with the `KeyRegistered` event; an
  approved publisher can anchor anything, and only its first dataset was read.

## What draft 2 changed
Review PUBON-R1's findings and how each was taken are in THREATS "PUBON-R1". Three differ from
the review's wording: H2 (a new dataset id does not help; item 9), M7 (a refused record keeps the
signer and a blocking time; item 9) and the order of a theft with a rotation (item 12).

## Open question for the operator
Who reviews when the reviewer is away, and who holds a copy of the one immutable reviewer key?
Until that is answered, a lost reviewer key means a new book and every publisher registering again.

## Alternatives
- **Reuse the proposal intake for applications.** Refused: one queue would mix "publishes
  nothing" with "asks for a signing key", and its journal is private.
- **Register on request, review later.** Refused: the book never forgets a registration.
- **A new book with open registration and a bond.** Not now: item 13's open question.
- **Prove control by a DNS record** (ADR-0028 allows one). Left out: the bulk tools read the
  well-known file only.
- **The node fetches the applicant's files.** Refused: it adds a fetch of any named host to the
  node (THREATS SUB-1); a hosted runner has nothing to protect.
