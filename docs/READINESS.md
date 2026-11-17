# Production readiness tracker

One list of what stands between the testnet prototype and a public beta on testnet, then real use.
The loop works this list from the top; every change is logged in [TRACKER.md](TRACKER.md).
State words: TODO, DOING, DONE (with the date), BLOCKED (says on what), OPERATOR (a step only the
operator can do).

Last update: 2026-11-16.

## A. Must have for a public beta on testnet

| # | Item | State | Notes |
|---|---|---|---|
| A1 | Alerts reach a person (a failed daily job, a failed or stale backup, the node down) | OPERATOR | Today every alert is only a log line. Needs the operator's choice of channel |
| A2 | The daily refresh retries by itself after an outage | OPERATOR | 2026-11-16: an 8-hour network outage failed the anchor step; it stayed failed until a run by hand. 2026-11-16: the script side is done and tested (a failed network step is exit 5 and the next start finishes the same catalog); the unit's restart-on-5 lines wait for the operator's install |
| A3 | Daily backup of the API node switched on | OPERATOR | Built and drilled 2026-11-16 (`scripts/backup-node.sh`, `scripts/restore-drill.sh`); units in `deploy/systemd/user`; until installed, one run by hand a day |
| A4 | A warning before a publisher's storage quota fills | DONE 2026-11-16 | The refresh prints the quota each day (10.43 of 16 GiB, 65 %), warns from 80 % and lists no new dataset from 95 % |
| A5 | The 14 datasets refused every day move to new ids with successor links | DOING | Tools built 2026-11-16 (ROADMAP S23); day 1 (2026-11-17) lists 14 new ids; day 2 moves 8 with links. 6 stay as they are: their source recomputed its history and dropped its first 1 to 40 days, so a move would lose days |
| A6 | Build review of the successor reader check and publisher tool (SUCC-B7), fixes, deploy | DONE 2026-11-16 | Deployed `501d0434`; a new `onyx-check` release is still to cut (A21) |
| A7 | Correction market 1 recheck on Base Sepolia (window 2026-11-17 to 2026-11-20) | DONE 2026-11-17 | The recheck round agreed on 18463, the original value; market 1 resolved, "Yes" won; 16 transactions |
| A8 | A newcomer test: a fresh reader uses the website, the API, the AI connector and the publish guide, and lists what confuses | DONE 2026-11-16 | Every area "works with friction"; publishing stops being self-serve at step 2. Its findings are A13 to A17 and B1 |
| A13 | The AI connector's dataset search looks in the bulk datasets | DONE 2026-11-16 (live 2026-11-16, node, site and AI connector at `501d0434`) | `onyx_find_dataset` also asks `GET /v1/bulk?q=` (common names made short: bitcoin to btc), and each match carries the exact call that reads its newest rows. Needs the connector deployed |
| A14 | A "get data in 3 commands" box at the top of the API page; links to the source repository and the AI connector in the footer | DONE 2026-11-16 (live 2026-11-16, node, site and AI connector at `501d0434`) | The box also shows how to check an answer with `onyx-check` (an oldest-first example the released binary accepts) and where to get it. Needs the site deployed, with the node (its third command uses `order=desc`) |
| A15 | Rows route: newest first, proofs optional, dates accepted for day keys, keys explained | DONE 2026-11-16 (live 2026-11-16, node, site and AI connector at `501d0434`) | `order=desc`; `from` and `to` take UTC dates for a key that reads as unix seconds (named as a time, its signed range between 1973 and 2100); the answer's `key` names the column and, then, the unit. `onyx-check --order desc` proves a newest-first page 0 starts at the newest row (review NEWC-R1, THREATS NEWC-R1-M1 and M3); the released onyx-check does not check newest-first pages until the next release. Proofs stay on every answer (an answer without them could not be told from a checked one); the size is documented instead, and the AI connector has a compact form. Needs the node deployed |
| A16 | AI connector answers sized for an AI (about 10 rows, no proofs by default); upload and relay tools hidden on the hosted server | DONE 2026-11-16 (live 2026-11-16, node, site and AI connector at `501d0434`) | `onyx_get_bulk_rows`: 10 rows by default, keys and values with a proof summary, `full` for the whole proof; the hosted server lists 29 read tools (41 locally). 2 rows of one column: 1.5 kB. Needs the connector deployed |
| A17 | Publisher guide gives the node and book addresses, one contact route and the real clone URL; `onyx-check` easy to find | DONE 2026-11-16 (live 2026-11-16, node, site and AI connector at `501d0434`) | docs/ops/bulk-publish.md and docs/agents.md: both addresses, a GitHub issue as the one contact route, a "Check an answer yourself" section. Reaches readers with the next public source snapshot |
| A18 | Stale datasets: about 2,700 of 9,047 end before November | BLOCKED | Cause found 2026-11-16: the first publisher's own source files stopped: most Bitcoin files in September, every Solana-chain token in July, three more tokens in September. The exporter and the node publish what the source holds. Needs the publisher's source to resume; A20 makes the age visible meanwhile |
| A20 | Each dataset shows how fresh it is (newest day, and a "not updated since" mark) in the list, the dataset page, the API listing and the AI connector | DONE 2026-11-16 (live 2026-11-17, node and site at `c7d86526`) | So a reader never takes stale data for current. The node says `newest` and `newest_age_days` and takes `fresh=30d` and `sort=newest`; the list and the open dataset say "No data after" (review FRESH-R1 fixed: a newest key dated in the future is said so and never counts as fresh); the connector marks `stale` and lists fresh matches first; the API page's example is a dataset with data from this month |
| A21 | A new release of the tools (`onyx-check` with `--successor`, `--order`; `onyx-publish` with moves) so readers can check newest-first pages and successor links | TODO | The quick-start check examples use oldest-first pages until then |
| A19 | The home page says first what a data user gets | TODO | Today it speaks to contributors first |
| A9 | Usage counts (per route and dataset, counts only, nothing about visitors) | TODO | No access log is kept, by design; counters inside the node |
| A10 | The record's daily job and the node backup run on one machine with no standby (2026-11-16: 13 hours lost to a crash and a network outage) | OPERATOR | A second machine or a hosted runner costs money |
| A11 | Test ETH for the daily anchors lasts about 3 months | OPERATOR | A faucet needs a sign-up |
| A12 | The health watch on the server (catalog age, storage nodes, gas) installed | OPERATOR | Staged since 2026-11-09; a one-time install |

## B. Before outside publishers

| # | Item | State | Notes |
|---|---|---|---|
| B1 | A guided sign-up for a new publisher (key registration, first dataset, first catalog) | TODO | Tools exist; not self-serve |
| B2 | The proposal review queue has a reviewer tool and checklist in use | TODO | ROADMAP S5 |
| B3 | Storage network: live deals again, deals on catalog-anchored datasets (THREATS CAT-2) | BLOCKED | Needs test ETH and a new OnyxStorage |
| B4 | Installers opened on real machines (ROADMAP S20) | OPERATOR | |

## C. Before real money

| # | Item | State | Notes |
|---|---|---|---|
| C1 | An outside security audit | OPERATOR | Costs money |
| C2 | Independent operators on other people's machines | OPERATOR | |
| C3 | A legal and rights review of the data licences | OPERATOR | |
| C4 | Phase H hardening (ROADMAP "hardening before mainnet") | TODO | |

## Done

| Date | Item |
|---|---|
| 2026-11-16 | Successor links on the API node (deployed `b69f2482`), reader check and publisher tool pushed (`5546d3e6`) |
| 2026-11-16 | Backup and restore drill for the API node (`85bc9ff1`), drilled against the live node |
| 2026-11-16 | Held-back trailing days: 28 datasets refused every day export again |
| 2026-11-15 | Early-contributor record backfilled (19 days), daily timer on |
| 2026-11-15 | Refresh failures raise an alert and exit non-zero |
