# Threat sweep L9 (2026-10-14)

Branch `threat-sweep` from main 4fe53de, origin/main 44bab39 (L4 mutation testing) merged in.
ROADMAP L9: every open Medium/High row in THREATS.md fixed, accepted with a reopen condition, or
deferred to a named open item, and a whole-system adversarial review of main. Not an external audit.

## Result

- 90 open or partial rows of severity Medium or higher (plus the unrated base threats) triaged
  against current code: 37 already closed by later work (each now names its test), 38 deferred to
  a named, unticked ROADMAP item, 12 accepted with a reason and a reopen condition, 3 fixed here
  (R-LBL-FIX-1 on two rows, T-KEY-1). 10 High rows marked TESTED/Guarded: every cited test exists
  and asserts the claim (all closed).
- 135 rows that named no test or no live milestone now do; 4 TODOs assigned to L9 by L3 and L2-R2
  (Low/Info) and 3 by L4 (MUT-1, MUT-2, MUT-5, Medium test-coverage gaps) moved to new items E6,
  E7, E8.
- New check: `scripts/check-threats.py` (run by `scripts/check-docs.sh`) enforces the CLAUDE.md rule.
- Adversarial pass: 19 new rows (L9S-1..9, L9A-1..10): 0 Critical, 0 High, 0 Medium for a testnet
  launch; 7 rise to Medium on mainnet. 5 fixed here (L9A-4, L9A-6, L9A-7, L9S-9, and L9A-2's docs).
- Nothing blocks a public testnet launch that is not already an owner decision or the TLS front
  (list below). Mainnet stays blocked by Phase H (H1-H7).

## Method

1. Parsed every table row of THREATS.md (Python, `cells()` as in check-threats.py), bucketed
   severity (Critical/Blocking, High incl. Med-High, Medium incl. Low-Med, Low, Info, unrated base
   rows) and status by its first word (Closed: FIXED, CLOSED, SUPERSEDED, TESTED, MITIGATED,
   Guarded, DOCUMENTED or a test name; Partial: PARTIAL, PARTLY, NARROWED; Open: OPEN, TODO,
   BLOCKED, ESCALATED, Handed, a bare milestone; plus DEFERRED and ACCEPTED). Status-update tables
   ("Status after loop 38") are not counted.
2. Seven read-only triage passes (fresh subagents, grep and read only, no builds) took the rows in
   batches: still true on current code, evidence (every cited test confirmed by grep), decision
   (CLOSED / FIX / ACCEPT / DEFER) and launch impact. The lead applied the results and wrote the
   fixes.
3. Two read-only adversarial passes over whole flows (not line review), one per attacker per
   flow: cheating miner, lazy validator, griefing challenger, data thief, malicious RPC, malicious
   operator. Flows: settlement round (miner, validator, coordinator, challenger, payouts and
   revenue); level-2 draw lifecycle; paid API (x402 precheck, facilitator and self-settle, settle
   check, receipts and intents, retry keys, gate, contributor reads, pricing); chain indexer;
   reserves serving check; website public build.

## Counts before and after

Before (origin/main 44bab39, 999 rows):

| Severity | Closed | Partial | Open | Deferred | Accepted | Total |
|---|---|---|---|---|---|---|
| Critical | 14 | 0 | 0 | 0 | 0 | 14 |
| High | 102 | 11 | 8 | 0 | 0 | 121 |
| Medium | 290 | 19 | 34 | 0 | 2 | 345 |
| Low | 351 | 7 | 31 | 0 | 15 | 404 |
| Info | 64 | 1 | 8 | 0 | 8 | 81 |
| Unrated | 23 | 1 | 10 | 0 | 0 | 34 |
| All | 844 | 39 | 91 | 0 | 25 | 999 |

After (this branch, 1,017 rows: 999 + 19 new, less one status-update duplicate the first count included):

| Severity | Closed | Partial | Open | Deferred | Accepted | Total |
|---|---|---|---|---|---|---|
| Critical | 14 | 0 | 0 | 0 | 0 | 14 |
| High | 110 | 4 | 1 | 2 | 4 | 121 |
| Medium | 301 | 15 | 15 | 6 | 8 | 345 |
| Low | 343 | 21 | 19 | 8 | 28 | 419 |
| Info | 55 | 7 | 4 | 2 | 17 | 85 |
| Unrated | 28 | 3 | 0 | 0 | 2 | 33 |
| All | 851 | 50 | 39 | 18 | 59 | 1017 |

Open and partial rows of Medium or higher: 72 before (19 High, 53 Medium) and 11 unrated base
threats; after: 43 open, partial or deferred (7 High, 36 Medium) and 0 base. Every one of them now
names an open ROADMAP item (or Needs the project owner), which check-docs enforces. The Medium count rose from
30 in the first draft to 36 after review: the stricter "verified test" rule found 6 Medium rows
marked FIXED whose fix no test exercises (R-HOST-COMPILE-1, L9-F4, W7, M6-L36-F2, M8-L57-N5,
AUD-1-F2); they are now PARTIAL with a TODO E1 test. Rows whose
status word stayed OPEN or PARTIAL but gained a `TODO <item>` count as open above, not deferred.

## Fixes made here

| Row | Fix | Test / check |
|---|---|---|
| R-LBL-FIX-1 (Medium) | With `ONYX_INDEXER=off` (the default) listed shards never ran the label-set evidence check, so a non-test set that is missing or cites unresolvable evidence was served. `onyx_api::indexer::check_label_set` (split out of `check_held`) now runs on every listed shard at onyx-node start | `catalog_file::tests::a_listed_shards_label_set_must_be_held_and_evidenced`; on the real start path `app::tests::the_start_refuses_a_listed_shard_whose_label_set_is_not_held` (fails when the call is removed: checked) |
| T-KEY-1 | No secret scan existed. `scripts/check-secrets.sh` in check-all, case-insensitive: tracked `.env*`, `*.env`, `.envrc`, `*.key`, `*.pem`, `.onyx-testnet/` (not `.env.example`); 64-hex keys after `*_KEY`, `private_key`, `privateKey`, `PK`, `SECRET` or `--private-key`; long `*SECRET`/`api_key` values (base64 too); 12-24 word mnemonics; PEM private keys. Public anvil placeholders allowlisted; prints file:line only; tree, not history | `scripts/check-secrets.sh --self-test`: 10 planted forms and 5 env/key file names must each fail, placeholders pass |
| L9A-4 (Info) | A gate answering fewer clearances than leaves would have let an export carry uncleared rows; `gate::refresh` now refuses the whole shard | `tests::a_gate_answering_fewer_rows_than_leaves_clears_nothing` |
| L9A-6 (Low) | `ONYX_PUBLIC=true` built the private preview with exit 0; build-web.sh refuses anything but 0 or 1 | check-all asserts `ONYX_PUBLIC=true scripts/build-web.sh` fails |
| L9A-7 (Low) | read-indexer-sepolia.py wrote `--rpc` verbatim into a shipped file; now scheme://host[:port] | `scripts/read-indexer-sepolia.py --self-test` (3 URLs) in check-all |
| L9A-2 (Low) | run-api.md said a self-settle revert gives the unit back; it does not | docs; code fix TODO E1 |
| L9S-9 (Info) | README called the empty `onyx-sim` the adversarial simulation | docs-only |
| OPS-7 (Low) | Base-image digest pins were unchecked | `scripts/check-boundaries.sh` fails on an unpinned `FROM` (negative control: exit 1) |
| L2-R3-2 (Info) | ADR-0022 Am. 1 items 4 and 7 said "one fee per member per shard" | docs-only; L2-R3-1 stated there |
| T-ENC-1, R-CLAIM-1 | T-ENC-1 cited a test removed in loop 5 | `check-threats.py` now fails on a cited test that does not exist |
| (index) | DECISIONS.md lacked ADRs 0020-0032 | check-docs fails on an ADR missing from the index |
| (IDs) | IDX-1..7 used twice, R-REG-1 twice | renamed CIDX-1..7 (chain indexer) and R-REG-2, noted in THREATS |

`scripts/check-threats.py` rules (tightened after review): (1) every row is backed by a VERIFIED
test, an ACCEPTED reason (4+ words, in the status or the mitigation column), a `docs-only:` path that
exists, or an open status / TODO naming an open milestone. A citation is verified only when it names
a test function (Rust `#[test]`/`#[tokio::test]`/proptest case, Solidity `test*`/`invariant_*`)
inside the crate, file, module or type its path names, or an e2e title that exists verbatim in
apps/web/tests, or a check script (`scripts/x.sh`) that names the row's ID and that check-all or
check-docs runs. Citations after "TODO" are planned tests and do not count. (2) an open row (OPEN,
TODO, PARTIAL, PARTLY, DEFERRED, BLOCKED, ESCALATED, NOT FIXED, Handed) names an unticked ROADMAP item,
an unticked PLAN milestone or "Needs the project owner" (finished milestones such as M4 fail as stale; a dated
"L9 2026-10-14" note does not count); (3) unique IDs outside status-update tables; (4) every
backticked test path outside a TODO names a `fn`/`function`/`mod` in the code. Rows that predate
the rule and have none of these may be listed in `scripts/threats-legacy.txt`, which may only shrink
(an ID added after origin/main's copy fails; a listed row that gains a verified test must leave).
After review, 142 rows failed the stricter rule; every one was resolved by citing its real test
(90), a docs-only path (17), an ACCEPTED reason (2), the check script that names it (L9A-6, L9A-7,
L2-R1-1, L2-R1-4, OPS-7) or PARTIAL with a TODO E1 test (27), so the legacy list is empty.
`check-threats.py --self-test` (run by check-docs) plants 13 bad rows (`totally_fake_crate::main`,
`Ledger::anything_at_all`, `ONYX_PUBLIC`, `nonexistent_test_name`, a non-test fn, a fake e2e title,
bare ACCEPTED, `docs-only: nowhere/...`, a stale TODO M4, no test, a duplicate ID, a test named only
after TODO, a guard script that does not name the row) that must each fail, 5 good rows (one backed only by its guard script) that must
pass, and a legacy-list addition that must fail.

## Decisions table (Medium and higher, plus the unrated base threats)

"Still true" is the triage pass's answer for current main. CLOSED rows cite their first test;
the full evidence is in each THREATS row's "L9 2026-10-14" note.

| ID | Sev | Still true | Decision | Where / why |
|---|---|---|---|---|
| T-COPY-1 | Base | no | CLOSED | covered by `commitment::commit_hash_binds_context` |
| T-COPY-2 | Base | no | CLOSED | covered by `OnyxSettlement.t.sol::test_VerdictRevealMustMatchCommit` |
| T-LAZY-1 | Base | no | CLOSED | covered by `validate::settle_reruns_disputes_and_lazy_actors_lose` |
| T-SAMP-1 | Base | partly | CLOSED | covered by `OnyxSettlement.t.sol::test_DrawnSeedIsTagAndBlockhash` |
| T-GRIEF-1 | Base | no | CLOSED | covered by `dispute::a_frivolous_challenge_with_silent_sybils_and_appeals_loses_money` |
| T-DRAW-1 | Base | no | CLOSED | covered by `golden::golden_output_is_byte_identical_everywhere` |
| T-LBL-1 | Base | no | CLOSED | covered by `score::tests::overconfident_scores_worse_than_calibrated` |
| T-LBL-2 | Base | no | CLOSED | covered by `attest::tests::non_canonical_statements_rejected` |
| T-PAY-1 | Base | no | CLOSED | covered by `paid_row_needs_an_exact_settled_payment` |
| T-PAY-2 | Base | no | CLOSED | covered by `paid_row_needs_an_exact_settled_payment` |
| T-FIX-1 | Base | partly | ACCEPT | reopen if an API-fetched fixture (not own-node data) ever feeds a finalized, sold or on-chain root |
| T-ENGINE-1 | Base | yes | ACCEPT | reopen if before mainnet; any wasmi version change; a RustSec advisory against wasmi 2.0.0 (check-deps fails); or a published wasmi 2.x audit to read |
| T-KEY-1 | Base | partly | FIX | fixed here: `scripts/check-secrets.sh` in check-all, with a planted-key self-test |
| R-DRAW-1 | High | no | CLOSED | covered by `commitment::every_field_is_bound` |
| R-COMMIT-1 | High | no | CLOSED | covered by `OnyxSettlement.t.sol::test_UnrevealedCommitForfeitsItsBond` |
| R-CANARY-1 | High | partly | DEFER | H2 |
| R-DA-1 | High | partly | DEFER | H7 |
| R-PRIV-1 | High | partly | ACCEPT | reopen if a metric, feed or recipe with an Address subject is proposed, or registry registration opens beyond the operator's review |
| R-SETTLE-1 | High | partly | ACCEPT | reopen if the first market or integrator contract that decodes row bytes on chain (ship a strict Solidity row decoder with shared vectors first) |
| R-CHAL-1 | Medium | no | CLOSED | covered by `OnyxDisputes.t.sol::test_ClaimRevealIsBoundToTheChallenger` |
| R-GRIEF-2 | Medium | no | CLOSED | covered by `dispute::config_rules` |
| R-SEED-1 | Medium | partly | CLOSED | covered by `OnyxSettlement.t.sol::test_DrawnSeedIsTagAndBlockhash` |
| R-PAY-3 | Medium | no | CLOSED | covered by `export_is_priced_per_row_and_capped` |
| R-REMOVE-1 | Medium | partly | ACCEPT | reopen if the first real removal request, or any automated CAS deletion or garbage collection is built (it must then refuse to delete a label set pinned by a root insi... |
| R-DRAW-2 | Medium | no | CLOSED | covered by `dispute::config_rules` |
| R-SUBJ-1 | Medium | no | CLOSED | covered by `claim::subjects_must_be_registered_and_canonical` |
| R-CHAL-2 | Medium | no | CLOSED | covered by `OnyxDisputes.t.sol::test_ClaimRevealIsBoundToTheChallenger` |
| R-CANARY-2 | Medium | partly | CLOSED | covered by `validate::settle_reruns_disputes_and_lazy_actors_lose` |
| R-DA-2 | Medium | no | DEFER | H7 |
| R-CACHE-1 | Medium | no | CLOSED | covered by `labels::attestation_tests::cache_reuses_signature_verdicts_across_shards` |
| R-DRAW-3 | High | partly | DEFER | H3 |
| R-DA-3 | High | yes | DEFER | H7 |
| R-CHAL-3 | High | no | CLOSED | covered by `OnyxDisputes.t.sol::test_L3_F2_UnrevealedClaimWaitsOnlyTheGrace` |
| R-REG-1 | High | no | CLOSED | covered by `registry::root_tracks_every_change` |
| R-SHARD-2 | Medium | no | CLOSED | covered by `commitment::shard_spec_checked_against_registry` |
| R-CANARY-3 | Medium | yes | DEFER | H2 |
| R-FIX-2 | High | partly | CLOSED | covered by `source-btc::header_checks` |
| R-SUBJ-3 | Medium | partly | ACCEPT | reopen if any instrument namespace is registered for a shipped registry (F4 ETF trusts, F6 series): add per-instrument registration first |
| R-REG-4 | Medium | partly | DEFER | G5 |
| R-LBL-FIX-1 | Medium | partly | FIX | fixed here: listed shards get the label-set check at start (`catalog_file::tests::a_listed_shards_label_set_must_be_held_and_evidenced`) |
| R-FIX-3 | High | partly | ACCEPT | reopen if Validators share one node, mirror or RPC provider, or a shard is pinned to a non-node (API) fixture, or any reward depends on a single node's prevout index |
| R-HOST-COMPILE-1 | Medium | no | PARTIAL | reviewed again: no test exercises the fix; TODO E1 |
| R-CLAIM-1 | Medium | partly | FIX | E1 |
| R-HOST-COMPILE-2 | Medium | no | CLOSED | covered by `host::fast_path_runs_only_allowlisted_recipes` |
| L10-F2 | Medium | no | CLOSED | covered by `host::fast_path_runs_only_allowlisted_recipes` |
| L12-F4 | Medium | no | CLOSED | covered by `bip322::registry_vectors_cover_types_and_refusals` |
| R-SCORE-1 | High | partly | DEFER | C1 |
| R-SCORE-2 | Medium | partly | DEFER | C1 |
| L13-F5 | Medium | no | CLOSED | covered by `labels::attestation_tests::evidence_and_attestation_caps` |
| L14-F4 | Medium | partly | DEFER | C1 |
| R-M3-2 | Medium | no | CLOSED | covered by `validate::settle_reruns_disputes_and_lazy_actors_lose` |
| R-M3-6 | Medium | no | CLOSED | covered by `OnyxSettlementRegression.t.sol::test_F4_RewardFollowsCommitOrder` |
| L18-F5 | Medium | no | CLOSED | covered by `cas::tests::honest_outputs_fit_the_row_cap` |
| L18-F8 | Medium | partly | DEFER | C1 |
| R-M4-2 | High | no | CLOSED | covered by `validate::settle_reruns_disputes_and_lazy_actors_lose` |
| R-M4-3 | Medium | no | CLOSED | covered by `OnyxSettlementVectors.t.sol::test_VerdictCommitHashesMatchRust` |
| R-M4-6 | Medium | yes | DEFER | H2 |
| R-M4-7 | Medium | yes | DEFER | H2 |
| L22-F3 | High | no | CLOSED | covered by `onyx_coordinator::validate::tests::canary_list_must_be_strictly_sorted` |
| L22-F4 | High | no | CLOSED | covered by `validate::settle_reruns_disputes_and_lazy_actors_lose` |
| L22-F5 | High | no | CLOSED | covered by `validate::settle_reruns_disputes_and_lazy_actors_lose` |
| L22-F6 | High | no | CLOSED | covered by `validate::settle_reruns_disputes_and_lazy_actors_lose` |
| L22-F7 | Medium | no | CLOSED | covered by `validate::settle_reruns_disputes_and_lazy_actors_lose` |
| L22-F9 | Medium | no | CLOSED | covered by `validate::settle_reruns_disputes_and_lazy_actors_lose` |
| L23-F2 | High | no | CLOSED | covered by `round::validation_rules_are_fixed_at_open` |
| L23-F6 | Medium | no | CLOSED | covered by `validate::settle_reruns_disputes_and_lazy_actors_lose` |
| L23-F7 | Medium | no | CLOSED | covered by `validate::lying_source_leaves_shard_pending_and_known_miners_are_never_canaries` |
| L23-F8 | Medium | no | CLOSED | covered by `validate::tests::concurrent_settlements_never_slash_past_the_bond` |
| L23-F9 | Medium | no | CLOSED | covered by `OnyxSettlement.t.sol::test_SilentAssignedValidatorIsSlashedOnce` |
| L24-F3 | High | no | CLOSED | covered by `validation_rules_are_fixed_at_open` |
| L24-F6 | Medium | partly | DEFER | H3 |
| L25-F3 | High | no | CLOSED | covered by `OnyxSettlement.t.sol::test_DrawnSeedIsTagAndBlockhash` |
| W3 | High | no | CLOSED | covered by e2e "api: sample proof checks against the pinned root" |
| W4 | High | no | CLOSED | covered by e2e "overview: demo market says level 4, not level 1" |
| W5 | Medium | no | CLOSED | covered by e2e "explorer: live headers pass" |
| W6 | Medium | no | CLOSED | docs-only fix |
| L28-F2 | Medium | no | CLOSED | covered by `OnyxSettlementRegression.t.sol::test_F3_ImpossibleVerdictWindowRefused` |
| L28-F3 | Medium | no | CLOSED | covered by `OnyxSettlement.t.sol::test_ValidatorSetIsTheWholeRegistrySnapshot` |
| L29-R1 | Medium | yes | DEFER | H3 |
| L29-R2 | Medium | no | CLOSED | covered by `OnyxDisputesVectors.t.sol::test_PanelDrawMatchesRust` |
| L29-F7 | Medium | partly | DEFER | H3 |
| M6-G1 | Medium | yes | DEFER | H2 |
| M6-L37-F1 | Medium | no | CLOSED | docs-only fix |
| M7-6 | Medium | partly | DEFER | H5 |
| M7-7 | Medium | partly | DEFER | L11 |
| M7-L42-F1 | Medium | no | CLOSED | covered by `tests::a_failed_or_timed_out_settle_keeps_the_unit_and_stores_no_success` |
| M6-D6 | Medium | yes | DEFER | H2 |
| LBL-V2-F2 | Medium | no | CLOSED | covered by `labels_v2::v2_names_only_allowlisted_companies` |
| LBL-V2-F3 | Medium | no | CLOSED | covered by e2e "labels: OKX labels are signed, level 3" |
| LBL-V2R-F2 | Medium | no | CLOSED | covered by e2e "labels: OKX labels are signed, level 3" |
| REV-1 | Medium | partly | DEFER | H4 |
| REV-12 | Medium | yes | DEFER | H4 |
| L2-F2 | High | partly | DEFER | C4 |
| L2-F6 | Medium | partly | ACCEPT | reopen if A level-2 feed is added whose source can personalise or geo-vary its answer, observers share one network path, or a money market accepts level 2 |
| L2-F9 | High | no | CLOSED | covered by `proof::tests::spec_rules` |
| L2R-F9 | Medium | no | CLOSED | docs-only fix |
| L2R2-F2 | Medium | partly | DEFER | L7: registry pin in the demo export, verified by the Docker smoke run |
| L2-SRC-1 | Medium | partly | DEFER | B2 |
| L2-SRC-3 | Medium | no | CLOSED | covered by `fiscaldata::tests::anything_but_one_matching_record_is_unavailable` |
| REG-1 | Medium | yes | DEFER | H1 |
| REG-R1-F4 | Medium | no | CLOSED | docs-only fix |
| OPS-2 | High | no | CLOSED | covered by `manifest::tests::chains_follow_the_environment` |
| OPS-L3-F4 | High | no | CLOSED | covered by `tests::a_long_lived_authorization_is_refused_before_verify` |
| SER-R1-F2 | Medium | no | CLOSED | covered by `series_props::preflight_refuses_a_shard_over_half_its_fuel` |
| L3-F1 | High | no | CLOSED | covered by `signed::tests::every_claim_field_is_bound_to_the_payload` |
| L3-F2 | High | no | CLOSED | covered by `signed::tests::a_forged_signature_is_refused` |
| L3-F3 | High | partly | DEFER | H6 |
| L3-F8 | Medium | partly | ACCEPT | reopen if Before the first real signed source is registered: write the review checklist and a periodic keys-document re-fetch first. |
| HIST-1 | Medium | yes | DEFER | D6 |
| L2API-R1-F2 | Medium | no | CLOSED | covered by `level2_review2::the_price_clock_is_the_window_end_whatever_the_round_config` |
| IDX-1 | High | no | CLOSED | covered by `labels_bridge::bridge_converter_refuses_uncleared_sources` |
| IDX-R1-F1 | High | no | CLOSED | covered by `labels_bridge::bridge_converter_refuses_uncleared_sources` |
| IDX-R1-F2 | Medium | no | DEFER | D4 |
| IDX-R1-F3 | Medium | no | CLOSED | docs-only fix |
| AUD-1-F1 | Medium | partly | ACCEPT | reopen if Actions billing restored (run CI once and confirm green), or outside contributors open PRs (then CI must gate merges). |
| SEC-1 | High | no | CLOSED | covered by `onyx_source_edgar::tests::only_the_canonical_feed_is_read` |
| SEC-R1-F2 | Medium | no | CLOSED | covered by e2e "data: SEC card says level 2 from filings, a company-wide crypto-asset total, as tagged, not on chain, and a rehearsal" |
| POOL-1 | High | partly | CLOSED | covered by e2e "pools: the note says the table is a claim, not proof of ownership" |
| POOL-5 | High | no | CLOSED | covered by `btc_pool_blocks::tests::unmatched_coinbase_text_is_never_output` |
| POOL-6 | High | no | CLOSED | covered by `golden_pools::a_tampered_proof_is_refused` |
| POR-1 | High | no | ACCEPT | reopen if Written permission from Chainlink (and Coinbase for cbBTC) or an openly licensed issuer attestation appears, or any code starts reading a Chainlink feed. |
| POR-2 | Medium | no | ACCEPT | reopen if POR-1 is unblocked and a PoR metric is designed (its wording must say 'declared by the issuer via the oracle'). |
| L2RND-R4-F1 | Base | no | CLOSED | covered by e2e "data: level-2 card says the browser checks the recorded draw for consistency only" |
| SIG-R1-F1 | Medium | partly | DEFER | C4 |
| REVLOG-1 | Medium | yes | DEFER | C4 |
| REVLOG-R1-M1 | Medium | yes | DEFER | C4 |
| CIDX-1 | High | no | CLOSED | covered by `indexer::tests::an_event_whose_root_the_contract_does_not_hold_is_not_served` |
| IDX-2 | Medium | no | CLOSED | covered by e2e "labels: published lists are claims, level 4" |
| IDX-3 | Medium | no | CLOSED | covered by `no_v3_fixture_ships_while_no_source_beyond_v2_is_cleared` |
| IDX-4 | Medium | partly | ACCEPT | reopen if a deployment serves without the client's own-node root check, or the node provider is not one the operator trusts as it trusts ChainRoots |
| IDX-R5-3 | Medium | partly | CLOSED | covered by `tests::the_client_checks_roots_on_its_own_node_by_default` |
| RES-1 | Medium | partly | DEFER | D2 |
| RES-2 | Medium | partly | DEFER | D1 |
| RES-4 | Medium | partly | DEFER | D1 |
| RES-10 | Medium | yes | DEFER | D3 |
| L8-H1 | Medium | yes | DEFER | L10 |
| L8-H2 | Medium | yes | DEFER | L10 |
| L8-4 | Medium | yes | DEFER | L10 |
| L8-5 | Medium | yes | DEFER | L10 |

## Accepted, and what reopens each

| Row | Sev | Why accepted | Reopen when |
|---|---|---|---|
| T-FIX-1 | Base | Mempool.space fixture is test and golden data only; live data comes from the operator's node, checked from raw bytes | an API-fetched fixture feeds a finalized, sold or on-chain root |
| T-ENGINE-1 | Base | wasmi pinned `=2.0.0`, golden roots identical on both engines, advisories gated | before mainnet (H5), any wasmi change, an advisory, or a published 2.x audit |
| R-PRIV-1 | High | Subjects and label entities must be registered; governance registers organisations only | a metric with an Address subject, or registration opened beyond the operator's review |
| R-SETTLE-1 | High | Onyx binds (round, shard) and the binding hash on chain; no market decodes row bytes on chain yet | the first market contract that decodes rows (ship a strict Solidity decoder first) |
| R-FIX-3 | High | Node data checked from raw bytes (difficulty pin, BIP34, prevouts in range, fee, subsidy, 21M) | validators share one node or RPC, or a reward rests on one node's prevout index |
| POR-1, POR-2 | High, Medium | Not built: the oracle's licence forbids redistribution (hard limit) | written permission or an openly licensed attestation, or any code reads the feed |
| R-REMOVE-1 | Medium | Rule and process written; old blobs kept restricted until dependent windows close | the first real removal, or any automated CAS deletion |
| R-SUBJ-3 | Medium | No instrument namespace is registered, so every Instrument subject is refused | an instrument namespace is registered (F4, F6) |
| L2-F6 | Medium | Observers fetch independently over HTTPS; one spoofer is outvoted and slashed | a personalising source, a shared network path, or a money market taking level 2 |
| L3-F8 | Medium | Origin rules enforced; residual is reviewer judgement, no real signed source exists | before the first real signed source (checklist + keys re-fetch) |
| AUD-1-F1 | Medium | CI mirrors check-all; billing is the owner's (Needs the project owner); every loop runs check-all locally | billing restored, or outside contributors open PRs |
| L9A-3 | Info | Library only: no partial contributor discount is wired | onyx-node wires a discount below 100% |

Low and Info rows accepted by the triage passes (R-RIGHTS-3, R-TEST-2, L26-F4, L28-F1, M6-D2,
POOL-3, REVLOG-2, CIDX-4, CIDX-6, L2C-1, L20-F5 and others) carry their reason and reopen condition
in their THREATS row.

## New findings (adversarial pass)

Severity is "testnet public launch / mainnet". Full rows: THREATS.md "Threat sweep L9".

| ID | Sev | Finding | Decision |
|---|---|---|---|
| L9S-1 | Low / Medium | OnyxRevenue registers canary reveals as miners (revenue share, contributor pass) | TODO H4 (contract) |
| L9S-2 | Low / Medium | Coordinator can list a newcomer's predictable correct reveal as a canary | TODO H7 (contract) |
| L9S-3 | Low / Low-Med | Self-challenge moves a shard's validator revenue cut to the miners | TODO H4 (contract) |
| L9S-4 | Low / Medium | Slash capped at one stake across rounds; zero-stake member still earns verdict fees | TODO H3 (contract) |
| L9S-5 | Low / Low | `panelSize` 1 is a valid dispute config | TODO H3 (contract + vectors) |
| L9S-6 | Low / Low | One coordinator bond backs any number of concurrent rounds | TODO H3 (contract) |
| L9S-7 | Low (testnet) | Free-mint sybil validators join later rounds of a live run | TODO E1 (driver), before the next live run |
| L9S-8 | Low / Medium | No client floor on level-2 pool size vs draw size | TODO C4 |
| L9S-9 | Info | README overstated `onyx-sim` | FIXED |
| L9A-1 | Low | Identical retry on metrics/observations latest can 409 after a newer shard lands | TODO E1 |
| L9A-2 | Low / Low-Med | "Nothing was sent" self-settle errors keep the unit and fill the payer's intent cap | docs fixed; TODO E1 |
| L9A-3 | Info | Partial contributor discount breaks the identical retry | ACCEPTED (not wired) |
| L9A-4 | Info | Short gate clearance list could export uncleared rows (latent) | FIXED |
| L9A-5 | Low | Registry level never re-read after admission | TODO E1 |
| L9A-6 | Low | `ONYX_PUBLIC=true` silently built the preview | FIXED |
| L9A-7 | Low | RPC URL with a key would ship in the run JSON | FIXED |
| L9A-8 | Low | Public guard misses e-mails and profile links; privacy scan is manual | TODO L10 |
| L9A-9 | Info | Public verifier still pins level-2/EDGAR response keccaks | TODO E4 |
| L9A-10 | Low / Medium | A removal claim strips a company-signed address with no proof | Needs the project owner, TODO L10 |

Coverage gaps noted by the passes, not raised as rows: no miner or validator client talks to a
chain (only the demo driver does), so a lying RPC aimed at miners or validators is untested;
round 2 of the indexer run page is matched by transaction only.

## What blocks a launch (input for docs/ops/launch-checklist.md, L10)

Public testnet launch (website and paid API on Base Sepolia):
1. Role contact for label removal in docs/ops/label-removal.md (L8-4, Needs the project owner).
2. Label-set rights for OKX, BITB, BitMEX and Binance: both builds ship totals derived from them
   and the API sells an OKX reserve row; OKX ToS 8.1 and BITB's examiner terms restrict
   distribution (L8-5, Needs the project owner).
3. TLS in front of onyx-node; a public `ONYX_LISTEN` is never plain HTTP (M7-7, L11).
4. Only an `ONYX_PUBLIC=1` build goes public (now refused for any other value, L9A-6), and the
   public guard also refuses e-mails and profile links (L9A-8).
5. Removal-policy decision for company-signed addresses (L9A-10, Needs the project owner).

Making the repository public (not needed for the site or API): rewrite or keep private the
history with excluded data (L8-H1, L8-H2, L8-H3, Needs the project owner), and run a history secret scan
first (T-KEY-1 covers the tree only).

Mainnet (upgrades.md mainnet gate: "no open high items", outside audit, multisig keys):
1. H1 owner and role keys: multisig or timelock owners, KMS coordinator, lost-key plan (REG-1, L2C-5, K-1..K-3).
2. H2 unbiasable randomness and private assignments (R-M4-6, R-M4-7, M6-G1, M6-D6, R-CANARY-1).
3. H3 dispute and round economics (L29-R1, L29-F7, L24-F6, R-DRAW-3, M6-D5, M6-D7, L9S-4..6).
4. H4 payment attribution and pins (REV-1, REV-12, L2C-2, L9S-1, L9S-3).
5. H5 outside security audit of contracts and x402, solc bump first, wasmi audit status (M7-6, DEP-1, K-8, T-ENGINE-1).
6. H6 level-3 unwind before any real signed source is served (L3-F3).
7. H7 open participation: availability challenges, unlinkable and signed canaries (R-DA-1, R-DA-3, R-CANARY-3, L9S-2).
8. Level 2 sells no money-market data until C4 closes REVLOG-1, SIG-R1-F1, L2-F2 and L9S-8
   (`proof::tests::money_markets_take_levels_one_and_three_only` holds that line today).

High rows still not closed: R-CANARY-1, R-DA-1, R-DA-3, R-DRAW-3 (partial or open, H2/H3/H7),
L2-F2 (C4, B2, B6), L3-F3 and R-SCORE-1 (deferred: H6/C2, C1); accepted with reasons: R-PRIV-1,
R-SETTLE-1, R-FIX-3, POR-1. Under the mainnet gate every one of these needs a decision.

## Commands and checks

Final run on the merged tree (origin/main 92ff43f merged in, review fixes applied), 2026-10-15:

| Command | Exit | Notes |
|---|---|---|
| `ONYX_WEB=1 scripts/check-all.sh` (systemd-run MemoryMax=12G, own `CARGO_TARGET_DIR`; exit code captured to a file) | 0 | ALL CHECKS PASSED: fmt, clippy, workspace tests, boundaries, docs, secrets, deps, forge (462 passed, 0 failed), storage layout, gas snapshot, website build and E2E PASSED |
| `scripts/check-threats.py` (in check-docs) | 0 | "threats ok: 1017 rows, open milestones 55, legacy rows without a verified test 0" |
| `scripts/check-threats.py --self-test` | 0 | 13 planted rows fail, 5 good rows pass, legacy list only shrinks |
| `scripts/check-secrets.sh --self-test`, `scripts/check-secrets.sh` | 0, 0 | 10 planted forms and 5 env/key file names fail, placeholders pass; 606 tracked files clean |
| `scripts/read-indexer-sepolia.py --self-test` | 0 | 3 URLs |
| `cargo test -p onyx-node --lib app::tests` with the start-path call removed | 1 (expected) | the new test catches the deletion; call restored |

Contracts were not changed (forge runs inside check-all). Builds used `CARGO_TARGET_DIR=<worktree>/target`
under `systemd-run --user --scope -p MemoryMax=12G`.
