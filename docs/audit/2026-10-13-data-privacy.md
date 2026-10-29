# Data and privacy audit, 2026-10-13 (ROADMAP L8)

Branch `audit-data` from main `6665104`. Scope: every dataset in the repository and in both
website builds (private preview, `ONYX_PUBLIC=1`), checked against the CLAUDE.md hard limits:
labels name entities only; no data whose licence forbids redistribution; no invented numbers;
sources linked. Not legal advice.

Result: **no individual is named in the tree or in either build**; **no excluded data** (wBTC,
cbBTC, Chainlink PoR, third-party lists, OKX PoR rows outside label set v2) **in the tree or in
either build**. Fixed: 3 (a gap in the public-build guard, the pool list's MIT notice not shipped
with the site, one excluded pool named in an ADR). Open for the project owner: 5 (3 past commits on `main`,
the removal contact, the label-set rights question). THREATS L8-F1..F3, L8-H1..H3, L8-4, L8-5.

## Tools

| Tool | Version |
|---|---|
| Python | 3.14.4 (`scripts/audit-privacy-scan.py`, new) |
| rustc / cargo | 1.98.0 (guard tests in `adapters/source-btc/tests/public_build.rs`, `pool_review.rs`) |
| pnpm / node | 10.34.5 / v22.23.2 (website build and e2e) |
| git | 2.53.0 |
| Reference data (local, read only) | The entity indexer `4da0223` (2026-09-27): `fixtures/labels/*.csv`, `okx-snapshot/`, `fixtures/groundtruth/pool-list.json` (SHA-256 `ddc505c8...a5ef0e`, equal to the pin in fixtures/pools/PROVENANCE.md) |

The scan needs the indexer's files because the names and addresses it looks for (people, excluded
lists) are, by design, not in this repository.

## Inventory

"Shipped" = in the website build. "Committed" = in git (the repository is private:
`gh repo view the private repository` reports `PRIVATE`). Decisions: OK, remove, or the project owner (needs his
rights review).

| Dataset | Source and link | Licence / terms (quoted, or "none published") | Committed | Shipped | Decision |
|---|---|---|---|---|---|
| `fixtures/btc/blocks-921080-921082.json` | Bitcoin mainnet via mempool.space Esplora API, tip cross-checked on blockstream.info (fixtures/btc/README.md) | Public chain data (`onyx:public-chain-data`); the API's own terms are not recorded in the repo | yes | preview only (inside recipe inputs, with label rows) | OK: consensus data, reproducible from our own node |
| `fixtures/btc/coinbase-970019-970029.json` | Our Bitcoin Core node (raw blocks), `examples/export-pools.rs proofs` | Public chain data | yes | no | OK: range chosen so every coinbase carries only a pool tag (POOL-R1-F2, scanned) |
| `fixtures-live/` (day, node, tail, week: 3,058 files, 8.0 GB) | Our Bitcoin Core node, `scripts/fetch-btc-node.sh` | Public chain data | **no (gitignored, local only)** | no (exports derived from it ship: history, reserves, live run) | OK; raw coinbases there carry miners' text and are never committed (scans below) |
| `fixtures/labels/test.onyx.entities.btc.v1.json` (120) and `.v2.json` (9,850: OKX 9,443, BitMEX 209, BITB 117, Binance 81) | OKX PoR files (https://www.okx.com/proof-of-reserves), BitMEX PoRL (https://s3-eu-west-1.amazonaws.com/public.bitmex.com/data/porl), Binance PoR wallet files (https://public.bnbstatic.com/static/proof-of-reserve/), Bitwise BITB list (https://bitbetf.com/), via the entity indexer (fixtures/labels/PROVENANCE.md) | **None published**: addresses published for public verification, no licence stated | yes | preview: all rows (recipe inputs); public: none (guarded) | **The project owner** (PLAN question 8; THREATS L8-5). Entities only: every row's `entity` is one of 4 companies, kinds Exchange/Etf, no free text |
| Label set v3 | `scripts/build-label-set-v3.py`, `CLEARED` list empty | n/a | no fixture (removed in `260bdc5`; see L8-H2) | no | OK |
| `fixtures/reserves/scan-971199.json`, `scan-971202.json`, `flows-971200-971202.jsonl`, `base-969838.json` | Our node (`scantxoutset` over the v2 addresses; OKX PoR addresses outside v2 dropped before commit) | Public chain data over the v2 addresses | yes | no (deploy/reserve-gate.json points the API at them) | OK, rights follow v2 (L8-5). Scanned: 7,028 / 7,025 / 69 addresses, all label-set addresses |
| `fixtures/reserves/cross-checks.json` | OKX PoR file `por_csv_2026090800_V1.zip`, BITB page (docs/ops/indexer-integration.md "Live checks") | None published | yes | totals only, in `history/reserves-week.json` | OK: totals and counts only; no address of the PoR file outside v2 anywhere (scans) |
| `apps/web/static/history/week/*.json`, `index.json` | `export-history` over fixtures-live/week with label set v2 | Derived: per-entity flows over public chain data | yes | both builds | OK (entity totals, Merkle leaves; no addresses: scan); rights question as L8-5 |
| `apps/web/static/history/reserves-week.json` | `export-reserves` | Derived per-entity balances | yes | both builds | OK (as above) |
| `apps/web/static/history/pools-week.json` | `export-pool` over fixtures-live/week with the pool allowlist | MIT (list); chain data | yes | both builds | OK; MIT notice now ships (L8-F2) |
| `fixtures/pools/pool-allowlist.json` (127 pools) + `LICENSE-mining-pools.txt` | https://github.com/bitcoin-data/mining-pools branch `generated`, `pool-list.json` | MIT: "The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software." | yes (upstream file not committed: it names people) | names and links in both builds; notice now in `data/` | OK. 22 upstream ids excluded by id and reason only |
| `apps/web/static/level2/debt-to-penny.json`, `adapters/source-fiscaldata/fixtures/` | https://fiscaldata.treasury.gov/ (Debt to the Penny API) | "The data on this site is available to copy, adapt, redistribute, or otherwise use for non-commercial and commercial purposes." (https://fiscaldata.treasury.gov/about-us/) | yes | preview: values; public: sealed commits only | OK |
| `apps/web/static/level2/sec-holdings.json`, `adapters/source-edgar/fixtures/` | https://data.sec.gov/api/xbrl/companyconcept/ (Strategy, Riot) | "Information presented on sec.gov is considered public information and may be copied or further distributed by users of the web site without the SEC's permission." (https://www.sec.gov/privacy) | yes | preview: values; public: sealed | OK. User-Agent sends no personal e-mail (SEC-4). One cited press URL carries an executive's name in its slug (PROVENANCE.md source link, not a label) |
| `apps/web/static/run/*.json` | Our runs: local anvil, Base Sepolia (`scripts/build-testnet-run.py`, `read-indexer-sepolia.py`) | Our data; public testnet chain data | yes | both builds | OK (testnet addresses of our own actor keys, tx hashes) |
| `fixtures/bip322/*.json` | https://github.com/bitcoin/bips/tree/master/bip-0322 | CC0-1.0 | yes | no (our own proof vectors ship) | OK |
| `fixtures/eip191`, `merkle`, `commit`, `claims`, `registry`, `revenue`, `settlement`, `api`, `ops`, `golden`; `adapters/source-btc/src/proof_vectors.rs`; `adapters/source-page/fixtures`, `adapters/source-signed/fixtures`; `apps/web/tests/fixtures` | Generated by Onyx (test keys from public labels; "Synthetic example ... Not real data") | Our data | yes | `revenue/*.json` ship in both builds; demo.json carries proof vectors and a test-key attestation | OK |
| `recipes/out/*.wasm` | `scripts/build-recipes.sh` (our code) | Repository licence | yes | `recipe.wasm` both builds | OK |
| `apps/web/static/fonts/*.woff2` | IBM Plex, Instrument Serif | SIL OFL 1.1; `OFL-*.txt` ship next to them | yes | both builds | OK |
| `apps/web/static/og.jpg` | Rendered from our own home page (`apps/web/tests/og.mjs`) | Our data | yes | both builds | OK |
| `apps/demo/site/data/*` (demo.json, recipe.wasm, input-full/half.b64.txt) | `examples/export-demo.rs` | Derived (blocks + label set v1) | no (gitignored) | preview: all; public: no inputs, demo.json `public: true` | OK under L8-5 |
| Excluded indexer sources: wBTC list, cbBTC lists, Chainlink cbBTC PoR, `*_dune_thirdparty`, `filings_gov`, `mining_pool_payouts`, `unattributed_*`, `exchange_seeds_converted`, a vendor/TimeChainIndex balances | see fixtures/labels/PROVENANCE.md, RESEARCH.md "On-chain proof-of-reserve feeds" and "Live data sources" | wBTC: terms forbid reproduction/distribution (IDX-R1-F1); Chainlink Terms v6.0 s.3: may not "resell, lease, lend, share, distribute or otherwise permit any third party to use" the Services (POR-1); cbBTC: unread (IDX-R1-F2); others: not established, or name people | **not in the tree** (2 past commits: L8-H1, L8-H2) | no (scans) | OK as excluded; history: the project owner |

## Known decisions re-verified

| Decision | Check | Result |
|---|---|---|
| wBTC excluded | `git grep -i wbtc` outside docs: only the converter's exclusion list and a test; address scan of tree and both builds against `exchange_por_wbtc.csv` | 0 addresses in the tree or builds; 231 in history (`01ebd95`, L8-H2) |
| cbBTC pending | same, against both cbBTC CSVs | 0 in tree and builds; 221 in history (`01ebd95`, L8-H2) |
| Chainlink PoR blocked | `git grep -n -i -E "chainlink|latestRoundData|getRoundData|aggregator" -- ':!*.md'` | no code reads a feed; `origin/evm-por` is fully merged and holds the decision only |
| OKX PoR file / BITB list: only totals quoted | address scan against `exchange_por.csv`, `okx-snapshot/` and `etf_custody.csv` rows outside v2 | 0 addresses outside v2 anywhere; `cross-checks.json` and `reserves-week.json` carry totals and counts |

## Scans

All from the repo root. The audit script prints a name hit's source class only, never the name.

```
python3 -I scripts/audit-privacy-scan.py $PWD <indexer> git                        # 522 tracked files
python3 -I scripts/audit-privacy-scan.py $PWD <indexer> history                    # 3,199 blobs reachable from any ref
ONYX_PUBLIC=1 scripts/build-web.sh                                                 # then:
python3 -I scripts/audit-privacy-scan.py $PWD <indexer> apps/web/build apps/web/static/data   # public: 92 files
scripts/build-web.sh                                                               # then the same scan: preview, 96 files
git ls-files -z | xargs -0 grep -IoE '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' | sort | uniq -c
```

Reference sets: 9,850 label-set addresses, 180 pool-allowlist addresses, 7,388 addresses from
excluded or out-of-v2 sources, 131 name needles (68 filings_gov entity strings, 16 third-party
"owners", 16 payout pool names not on the allowlist, 31 names and tags of the 22 excluded pool ids).

| Target | Person names | E-mail / social | Handle phrase | Excluded addresses | Other Bitcoin addresses |
|---|---|---|---|---|---|
| Tracked files | 0. Hits only on company names that also occur in the indexer lists (BitMEX, Braiins, Innopolis, LuBian, Pega Pool, SigmaPool, Ultimus Pool; an OFAC entity in THREATS); 1 excluded pool's name (id 115, not a person) in ADR-0032, fixed (L8-F3) | 0 personal: `@*.example` test strings, `legal@chain.link` (a published role address); "x.com" hits are `okx.com` | 0 | 1: the OFAC-listed entity address that v2 drops (`bc1qjzk6...`, an entity listing, SDGT), cited in PROVENANCE.md and `labels_v2.rs` as the deny-list case | proof vectors, BIP-322 vectors, recipe test literals |
| Git history | 0 names; id 118's tag (an operator handle) in ADR-0032 at `9111db7` (L8-H3) | 0 | 1 blob: an invented test handle in an old `pool_review.rs` | `c12ed3a`: 912 payout addresses (L8-H1); `01ebd95`: 231 wBTC + 221 cbBTC (L8-H2) | |
| Public build (`ONYX_PUBLIC=1`) | 0 (hits are BitMEX and pool companies) | 0 | 0 (`pool_review` built-site scan, exit 0) | 0 | 11, all in `data/demo.json`: proof vectors and the demo attestation (test keys) |
| Private preview (96 files) | 0 (hits are BitMEX and six pool companies) | 0 | 0 (`pool_review` built-site scan, exit 0) | 0 | 11 in `demo.json` (test keys, as public). The recipe inputs carry the 9,850 (full) and 5,129 (half) label-set addresses by design (L8-5) and nothing else once decoded; 17 address-shaped runs in the raw base64 text are base64 noise |

The guard tests the build script already ran (`public_build::a_public_build_carries_no_label_rows`,
`pool_review::the_built_site_carries_no_persons_handle`, `att_r1_attempts::a_public_build_ships_no_level2_value_or_result_hash`)
passed in both builds.

## Gap found and closed (L8-F1)

The public guard knew only addresses of committed label sets. An address from a list that is
never committed (2,398 BTC addresses of OKX's PoR file are outside v2; the excluded wBTC and cbBTC
lists; a person's address) would have passed. `public_build.rs` now also refuses every
checksum-valid Bitcoin address (any network) that is not on `known_addresses` (the registry's
proof vectors, the BIP-322 vectors, the demo attestation's test key, the reviewed pool list), in
every form the guard already decodes (text, base64, hex, escapes, `data:` URIs, gzip, zlib,
Brotli). Test: `the_guard_refuses_an_unlisted_address_and_passes_known_ones` (bech32, base58,
base64-wrapped, `\xXX`-escaped, testnet; known ones and hashes pass). The current public build
passes it.

## Fixes

| Id | Change |
|---|---|
| L8-F1 | `adapters/source-btc/tests/public_build.rs`: unlisted-address rule and test; docs/ops/deployment.md |
| L8-F2 | `scripts/build-web.sh` ships `data/LICENSE-mining-pools.txt`; linked from the pools section (`apps/web/src/routes/data/+page.svelte`); allowlisted; e2e check |
| L8-F3 | docs/adr/0032-mining-pool-block-share.md names excluded id 115 by id only |
| Process | docs/ops/label-removal.md (new), legal-data-rights.md, data-retention.md, docs/spec/labels.md R-IDX-5 exception, docs/ops/README.md |
| Tooling | `scripts/audit-privacy-scan.py` (new) |

## Open items (Needs the project owner)

1. **History with excluded or personal-adjacent data** (L8-H1..H3): commits `c12ed3a` (912
   payout addresses, likely some individuals'), `01ebd95` (wBTC and cbBTC lists), `9111db7` (an
   operator-handle tag). All on `main`, pushed to the private remote. No rewrite done. Decide
   before the repository or any mirror goes public: rewrite and force-push, or keep private.
2. **Removal contact** (L8-4): docs/ops/label-removal.md has a marked placeholder for the role
   mailbox; the only channel today (an issue in a private repo) is closed to outsiders.
3. **Label-set rights** (L8-5, PLAN question 8): OKX, BitMEX, Binance and BITB publish their
   addresses without a licence; the preview ships the rows, both builds ship totals derived from them.
4. **Response times** in label-removal.md are proposals.
5. The indexer's `EVIDENCE.md` (named in docs/ops/indexer-integration.md row 6) is the indexer's
   file, not Onyx's; Onyx has no EVIDENCE.md. Nothing to change.

## Checks

Run on the final tree (`CARGO_TARGET_DIR=<worktree>/target`, under `systemd-run --user --scope -p MemoryMax=8G`):

| Command | Exit | Notes |
|---|---|---|
| `ONYX_PUBLIC=1 scripts/build-web.sh` | 0 | 3 guard tests pass (label rows and unlisted addresses, coinbase handles, sealed level-2); e2e passed, incl. "pools: the MIT notice of the pool list ships and is linked" |
| `scripts/audit-privacy-scan.py` on the public build | 0 | table above |
| `scripts/check-docs.sh` | 0 | 12 specs, 33 ADRs |
| `ONYX_WEB=1 scripts/check-all.sh` | 0 | fmt, clippy, workspace tests (incl. the 3 `public_build` tests and the repo-wide `pool_review` scan), boundaries, docs, forge (447 passed, 1 skipped), preview build and e2e |
