# Label set fixture provenance: test.onyx.entities.btc v1

Website: only the private preview build ships these rows (inside the recipe inputs);
`ONYX_PUBLIC=1 scripts/build-web.sh` ships none and refuses a build that carries any
(AUD-2-F1, docs/ops/deployment.md "Website builds").

Test fixture only. Named `test.*` (since loop 13), so `LabelSet::check_servable` refuses it. Rights review pending (PLAN.md question 8): the sources publish these addresses
for public verification but state no license. Do not serve these labels to customers until reviewed.

| Entity | Rows | Source | Evidence grade |
|---|---|---|---|
| entity:okx | 100 (43 active in blocks 921080-921082, 57 not) | OKX proof-of-reserves address files, e.g. https://static.okx.com/cdn/okx/por/chain/por_csv_2026070700_V3.zip, signatures checked by the entity indexer `scripts/verify-signed-addresses.py` (verdict `verified`) | First-party signed (PoR) |
| entity:bitwise-bitb | 20 | Bitwise BITB issuer wallet list, https://bitbetf.com/ (via the entity indexer `fixtures/labels/etf_custody.csv`) | Issuer published |

Selection: all OKX-verified addresses whose script appears in the fixture blocks, then the first 57
other OKX-verified addresses and the first 20 BITB addresses in source order. Confidence is a
fixture value (OKX 990,000 ppm, BITB 950,000 ppm), not a calibrated score.
Note: BITB addresses are custodied by Coinbase Custody; owner vs custodian attribution is an M2 topic.

# Label set fixture provenance: test.onyx.entities.btc v2

Test fixture only (`test.*`, never served; same rights note as v1: the sources publish these
addresses for public verification but state no license). 9,850 labels, 4 companies, built by
`python3 -I scripts/build-label-set-v2.py <the entity indexer>/fixtures/labels <out>`
(reproducible: same inputs, same bytes). Every v1 label is kept with identical kind and
confidence. Entities are registered in `fixture_registry_v2()` (`LABELS_V2_ENTITIES`); the v1
registry is unchanged.

Source files and their dates (from each file's own header and its modification time; LBL-V2-F4):

| Indexer file | Header date | File last modified |
|---|---|---|
| `exchange_por-verified.csv` | produced 2026-08-11 | 2026-08-11 |
| `exchange_por.csv` | fetched 2026-08-10 (OKX archive re-downloaded 2026-08-11) | 2026-08-11 |
| `etf_custody.csv` | fetched 2026-08-10 | 2026-08-17 |

| Entity | Kind | Rows | Source | Evidence class | Confidence (ppm) |
|---|---|---|---|---|---|
| entity:okx | Exchange | 9,443 | OKX proof-of-reserves files, 7 snapshots 2026-01..07, e.g. https://static.okx.com/cdn/okx/por/chain/por_csv_2026070700_V3.zip; indexer `exchange_por-verified.csv` | First-party signed: script hashes to the address and keys recovered from OKX's signatures meet the threshold (9,444/9,444 verified; 1 dropped as a filed address, below) | 990,000 |
| entity:bitmex | Exchange | 209 | BitMEX public PoRL bucket, https://s3-eu-west-1.amazonaws.com/public.bitmex.com/data/porl | Exchange published: 106 redeem scripts hash to the address (checked by the indexer); 103 output descriptors not expanded | 950,000 (106) / 900,000 (103) |
| entity:binance | Exchange | 81 | Binance proof-of-reserve wallet files, https://public.bnbstatic.com/static/proof-of-reserve/wallet_address_YYYYMMDD.zip (43 snapshots) | Exchange published, no signatures. The indexer states it re-downloaded the newest file (20260801) and matched it to the archive; Onyx did not repeat that check (no hash is recorded; LBL-V2R-F3). Its 55 exchange rows count as re-checked, the 26 older-only rows do not | 950,000 (55) / 900,000 (26) |
| entity:bitwise-bitb | Etf | 117 | Bitwise BITB wallet list, https://bitbetf.com/ (live + archive.org captures) | Issuer published. The 65 addresses live on the page have issuer-stated balances equal to on-chain balances; the 52 rotated-out addresses were not balance-checked. All 20 v1 BITB labels are among the 65 | 950,000 (65) / 900,000 (52) |

Confidence is a fixture convention, not a calibrated score: 950,000 only where a re-check
happened for that row (LBL-V2-F5), 900,000 for published rows nothing re-checked. The grade is
read from the indexer's per-row note with one strict pattern per source; a note in any other
format stops the build (LBL-V2R-F5; `--self-test`).

Excluded, with reasons:
- `filings_gov.csv` (and `filings_gov-build/`): it names natural persons. Hard limit: Onyx never labels individuals.
  No label is read from it. Its address column is used only as a deny list (LBL-V2R-F1): an
  address a government filing names is dropped even when a company signed or published it.
  One row dropped: `bc1qjzk63wwatq2f638psymq3qapn00aqgksyx56l3t8eg7zuuccwasq498khf`, OKX-signed
  in one snapshot (2026-02) and also on the OFAC SDN list (uid 58287, an entity listing, SDGT).
- `exchange_dune_thirdparty.csv`, `exchange_dune_binance_cold.csv`, `government_dune_thirdparty.csv`,
  `grayscale_dune_thirdparty.csv`, `owners_dune_thirdparty.csv`: third-party attribution;
  redistribution terms not established. Hard limit: no data whose license may forbid it.
- `unattributed_exchange_a.csv`: inferred from co-spend structure, no external claim of identity.
- `exchange_seeds_converted.csv`: mixed grades, 1,771 rows inferred; not first-party as a file.
- `exchange_por_cbbtc*.csv` (Coinbase cbBTC), `exchange_por_wbtc.csv` (wBTC/BitGo): wrapped-token
  reserves, a separate product and custody question; not reviewed for this set.
- `okx-snapshot/`: the raw OKX archive behind `exchange_por-verified.csv` (already included).
- `mining_pool_payouts.csv` (dropped after review, LBL-V2-F1): a coinbase payout shows where
  coins came from, not who controls the address (the indexer's own ruling). Many rows are
  individual miners' payouts (KnCMiner paid 104 addresses in one block; sampled F2Pool rows are
  single-use 1 BTC payouts; one LuBian row is a seized address in a forfeiture filing), and the
  hard limit forbids labelling individuals. Pools return only with a method that proves pool
  ownership (THREATS LBL-V2-F1, PLAN).
- 5 Binance rows with `entity_type=exchange_custodian` (held by the custodian Ceffu): custody
  addresses are not exchange addresses.
- Other spot BTC ETFs: none publishes its addresses (indexer `etf_custody.csv` header).

# Indexer bridge (ADR-0031): its undated output never ships

`scripts/build-label-set-v3.py` (the ADR-0031 bridge) builds v2's rows plus every indexer label
file on its `CLEARED` list, and that list is empty, so its output equals v2; it is never written
as a fixture. The v3 fixture is the dated roster set below (ROADMAP P2a), built from the issuers'
own snapshots, not from the indexer's label files. Indexer label files considered and excluded
(ADR-0031 item 1 and Amendment 1; the same exclusions hold for the roster set):

| Indexer file | Why it is not imported |
|---|---|
| `exchange_por_wbtc.csv` (wBTC reserves, fetched from wbtc.network's API) | The site's terms of service forbid reproducing or distributing its materials without written permission and forbid automated access (IDX-R1-F1) |
| `exchange_por_cbbtc.csv`, `exchange_por_cbbtc_2026-08-16.csv` (cbBTC reserves) | Coinbase's terms for the page are unread (it refuses our fetches); the rows came from a 2025-10-17 Wayback capture and a 2026-08-16 snapshot relayed by a person. Blocked on rights review (IDX-R1-F2) |
| cluster expansions, `inferred`, `unattributed_*` | Heuristic attribution (graded weak, 24.3% unresolved false merges) could label a person |
| `*_dune_thirdparty.csv` | Third-party attribution, redistribution terms not established |
| `mining_pool_payouts.csv` | Payout addresses are often individual miners (LBL-V2-F1) |
| `exchange_seeds_converted.csv` | Mt. Gox rows are relayed by a wiki; the rest are inferred |
| `filings_gov.csv` | Names natural persons; its addresses stay v2's deny list only |

An earlier draft of this section stated per-file cbBTC row counts that did not match the file
(IDX-R1-F3); with cbBTC excluded the counts are moot and were removed.

# Label set fixture provenance: test.onyx.entities.btc v3 (dated, issuer rosters)

ROADMAP P2a (absorbs D10), ADR-0037 Amendment 1. Test fixture only (`test.*`, never served;
`check_servable` refuses it). Rights: the issuers publish these rosters for public verification
and state no licence (THREATS L8-5, the same status as v1 and v2); OKX's terms (8.1) and BITB's
examiner terms restrict distribution, as recorded for v2. Served only under the rights rules: the
private preview shows the per-entity figures; a public build with `ONYX_RIGHTS=fallback` ships no
label-derived total (`history/rosters-week.json` is refused by path, ADR-0035).

18,444 memberships of 16,555 addresses, 11 entities, 206 dated snapshots, published at block
971,207 (the end of the committed header chain). Content hash
`0x38fdef8d7afe64352f35b0dbb2ede8c4122030ce2ec627f720343264e76a64cd`. Rebuild, byte for byte:

```text
python3 -I scripts/build-rosters.py listings /tmp/listings.json
cargo run --release -p onyx-source-btc --example build-label-set-v3 -- /tmp/listings.json fixtures/labels/test.onyx.entities.btc.v3.json
```

(`labels_v3::v3_rebuilds_byte_for_byte_from_the_committed_rosters` runs both and compares.) The
committed snapshot files (or, where the raw bytes hold personal data or are too large, deterministic extracts) are under `fixtures/rosters/<source>/` with a `snapshots.json` per source
(issuer URL, issuer date, fetch date, block height and how it was found, sha256 and keccak-256 of
every raw snapshot, the committed file's sha256, and every recorded but skipped snapshot with its
hash). They were imported read-only from the indexer's own runs of the issuers' files
(`scripts/build-rosters.py import`); `scripts/build-rosters.py fetch <source> <dir>` downloads
them again from the issuer (User-Agent `onyx-dev`) and checks every sha256.

How a row becomes a membership (ADR-0037 Amendment 1): a listing supports the address for 8,928
blocks (62 days); a range runs from the snapshot that lists it to the block before the first later
snapshot that omits it, or until its last listing is 8,928 blocks old, and stays open when still
alive at 971,207. Each membership cites its first and last snapshot record in its evidence (the
Merkle leaf commits the URL, dates, height and raw hashes). Grades: `signed` (990,000 ppm) only
where the issuer's published Bitcoin message signature verifies in Onyx
(`onyx_source_btc::signed_roster`, a Rust port of the indexer's `verify-signed-addresses.py`
rule); everything else the issuer listed is `published` (900,000 ppm).

| Source id | Issuer | Snapshots imported | Recorded, not imported | Committed form | Height basis | Grade | Addresses | Memberships | Indexer run |
|---|---|---|---|---|---|---|---|---|---|
| `binance.por` | Binance (`entity:binance`) | 43 (2023-02-01 to 2026-08-01) | 0 | raw HotCold CSV member, gzip | issuer | published | 81 | 84 (0 open) | `por/binance-history/20260822T092000Z` |
| `bitwise.bitb` | Bitwise BITB (`entity:bitwise-bitb`) | 1 (2026-08-24 to 2026-08-24) | 0 | extract: active wallets from the page JSON | bound | published | 65 | 65 (65 open) | `staging/bitb/20260824T082600Z` |
| `bitfinex.wallets` | Bitfinex (`entity:bitfinex`) | 2 (2022-11-11 to 2026-08-24) | 1 | raw wallets.txt | fetch-tip, indexer-mapped | published | 3 | 6 (3 open) | `por/bitfinex/20260824T200500Z` |
| `bitmex.por` | BitMEX (`entity:bitmex`) | 61 (2026-01-01 to 2026-08-20) | 304 | extract: height and the address section, gzip (no custodial section) | issuer | published | 531 | 760 (61 open) | `por/bitmex-history/20260822T013611Z` |
| `bybit.por` | Bybit (`entity:bybit`) | 17 (2024-09-04 to 2026-07-22) | 0 | extract: addresses from the PDF (pdftotext -layout) | indexer-mapped | published | 28 | 53 (0 open) | `por/bybit-history/20260822T111100Z` |
| `coinex.por` | CoinEx (`entity:coinex`) | 20 (2023-12-26 to 2026-07-21) | 0 | extract: the worksheet cells as CSV, gzip (no workbook metadata) | issuer | signed and published | 54 | 107 (0 open) | `staging/coinex/20260824T193131Z` |
| `cryptocom.wallets` | Crypto.com (`entity:crypto-com`) | 1 (2022-11-11 to 2022-11-11) | 0 | extract: addresses from the page | indexer-mapped | published | 6 | 6 (0 open) | `staging/cryptocom/20260824T211500Z-refresh` |
| `deribit.por` | Deribit (`entity:deribit`) | 1 (2026-01-26 to 2026-01-26) | 0 | extract: addresses from the article JSON | fetch-tip | published | 9 | 9 (9 open) | `por/deribit/20260822T141328Z` |
| `htx.por` | HTX (`entity:htx`) | 45 (2022-12-01 to 2026-08-01) | 0 | raw CSV, gzip | issuer | signed and published | 17 | 30 (0 open) | `por/htx/20260824T230226Z-reclassified` |
| `mexc.por` | MEXC (`entity:mexc`) | 8 (2026-01-09 to 2026-08-10) | 7 | raw CSV, gzip | indexer-mapped | published | 6318 | 6700 (0 open) | `staging/mexc/20260825T053500Z-renormalized` |
| `okx.por` | OKX (`entity:okx`) | 7 (2026-01-17 to 2026-07-07) | 19 | extract: the Bitcoin rows of each zip (address, message, signatures, script), one file | issuer | signed | 9443 | 10624 (0 open) | `por/okx-history/20260822T043948Z` |

SHA-256 of every imported raw snapshot (as the issuer served it; keccak-256 and the URL are in each source's `snapshots.json`):

`binance.por` (public.bnbstatic.com):

- 2023-02-01, block 774512: `d275c98e69efb304ea174de79308ae8623a0f7ea043bf7723746071964db13c0`
- 2023-03-01, block 778721: `9aa6c34eced4b5978d895ceb4ba9610438398a32964ebb3ac78aa6cdbe8962d7`
- 2023-04-01, block 783395: `64ce95e33d5000e2a30b7dcb281c590de92ee3f3611456792eef2295c4532ae8`
- 2023-05-01, block 787704: `9a4b8b528d7a55d9d3636bd3d92cd7dfd5e9991d5bdb2de03f6cc11323523b1b`
- 2023-06-01, block 792316: `028429a4f92021378c212ff58061e7b8435d281e3b680870cb2812abe275d296`
- 2023-07-01, block 796629: `0f8e130882d640e90640e57c08f0139f94f33a0c25e127189c72240a4d87de7c`
- 2023-08-01, block 801130: `c43381da0295d375bc476095421e59a2817158e6b072be75e943ee16126eb70f`
- 2023-09-01, block 805651: `297ca175fec3b3783bb1f6d58fdfb461d57ec741e9f479fc17d3654d8f6bbcf9`
- 2023-10-01, block 810077: `5ca0b4f860fbf320ab7fb118eef6f0bc154344e7ebf07bff93c60342d1d261a2`
- 2023-11-01, block 814748: `5fd27c077f47cf8752555ac4ab2be5d3e4a58db174b990b0634b62a08f6eccc8`
- 2023-12-01, block 819186: `e7612499e688f635a43137db7bee7fbe3fe4fb5a7b2740ea26ceef50f6644687`
- 2024-01-01, block 823629: `34ec26f4911c19f2c8af9007e0eb6b7e54ab69ac30864dc97f6e17210ef44e28`
- 2024-02-01, block 828307: `ffb558b82f87bc5d9238526d75f86650f3c37125f4abc317f2dc2f6780507508`
- 2024-03-01, block 832602: `1a2b44045f5e28e749389a716217cb4ceb01d60d8b0024574a9df00b65b07a2d`
- 2024-04-01, block 837164: `a713a13db0ad805d1d296889dcadf864dd4f6afca14556d7e1e8a0da837d0e01`
- 2024-05-01, block 841571: `cc71ae4357ac9676d326775ee2f5a93711ad2a77098e422d00bb245997f57bd2`
- 2024-06-01, block 845981: `24b4af6eb0efea44c4cb9f620779c36dcc686b0f8aaf1cbd3a1b351422109187`
- 2024-07-01, block 850160: `71cae7a501fff02eaf433b68da3159ccd241eccecedf0108f3e6102dd41143e1`
- 2024-08-01, block 854872: `50903686558276bfd24afb8163d369aebf66e0f431fc75b4b59e94a851043778`
- 2024-09-01, block 859302: `4aececd0c00f2114f03e7a3c47477f995dc12b804e079a383032a5ce8f1e37af`
- 2024-10-01, block 863565: `eaf55a3885fbe648a9b50a52299a8251f87f8f0bf98d175427bfb769f4daf493`
- 2024-11-01, block 868326: `8fa0a138c51f3f8e693cc10f70d05510c81e6b7ba98b9b4eee3ffd2aea06bc9e`
- 2024-12-01, block 872689: `d56df0e607728bcc31813c5d3ac0271a08702b4e5a62bce72a991916965f699e`
- 2025-01-01, block 877258: `50f565013d8d83a3663b91ea2021794e37759d217f8f24947a9e043595e1eff8`
- 2025-02-01, block 881692: `10c05f0e17c901e7f9d13380ce89944a5154a23dd4fd5afe69fc3629d45ea186`
- 2025-03-01, block 885783: `dc6602b5d0111761d2532ef89a5d1131c57be61b94f223e294418475c0c9503b`
- 2025-04-01, block 890324: `5c45b311b3eb6e095ad11385be9b945ecc79d2e62eff79b24acdc48352f1ccbc`
- 2025-05-01, block 894668: `5a32edfb8a3cce1a0b61143ee49728b73b21cea1557ddeae4c4e07998bb88b1f`
- 2025-06-01, block 899296: `d1e05acb8d79e4f3f0477a513e2798118951542602d9e87329b627116b90077f`
- 2025-07-01, block 903454: `592c7daf0d179d1b0ed9171fc01f4a062e51121b699365787650e49aef909602`
- 2025-08-01, block 908028: `eba4d16d1f88283ace38d7d3a47df0ed75d85a651cd21a8c12c2aec8e01b3719`
- 2025-09-01, block 912616: `7114059bc97c1b37a9ded7974be327cc13ec31faa7951e1dc07b61b958c73e1c`
- 2025-10-01, block 917148: `96efd6860b602ce69fd67c903f3046814954f593285814d3834e3cd9bc9bec62`
- 2025-11-01, block 921682: `6b76e83d7d072d0bdb7bf2d73efad3af678e97c2a7bbd7ad07b8c123f286418b`
- 2025-12-01, block 925938: `d20166e367771f054a9822dc05330c55a5b6243460ab86a85dbb83ecb3ce2491`
- 2026-01-01, block 930340: `b97c2dbf08bfbef4f6d1c4e5ce2a1a1749db52781b4c071084682eaa640f390d`
- 2026-02-01, block 934541: `0c6edcbffd6e56e940de3761ebd7c4b413e086c2089731ff2449679e488eadf2`
- 2026-03-01, block 938780: `532db44095ec1879fffde76079e7e04615890b687ab248a44d7f3fcde04d0fd2`
- 2026-04-01, block 943129: `49acfb18280942e9ea03d6ad1785c06cdf52b33493b075778e4d92e4b242fb27`
- 2026-05-01, block 947348: `3ccdcbbc26afc4d7ffbab528385feb2416159504491db5a53ecad5b060d0fb55`
- 2026-06-01, block 951913: `a2aa224141083534f3be881e95f5117f8e55aee944468225f0ae08c1393f7f9c`
- 2026-07-01, block 956140: `59990af7baf43a654bbff0c6f2834d1e82fbc4215696d0f0891f101931c2b8e1`
- 2026-08-01, block 962079: `aa4201834bfe2a1a7997567f87e897858bb6c1d1a5208c5271dd0ec3f12045b9`

`bitwise.bitb` (bitbetf.com):

- 2026-08-24, block 963898: `75ebc18617bd546d3790917006ef4029cd94d9b91c3b9cdc56070e7a2739a972`

`bitfinex.wallets` (raw.githubusercontent.com):

- 2022-11-11, block 762737: `e68a1f3f8bbd1df8cf8643b588b94fa4e743b94a9a6a3675524944539d153727`
- 2026-08-24, block 963899: `1ffb55547342927abf5a6bc5486b8842342f90e9b598489cdf65199fbaa55a55`

`bitmex.por` (s3-eu-west-1.amazonaws.com):

- 2026-01-01-930405, block 930405: `0f05ebb284fc1348f7128580d8204e2bd0f60a3eb92a5c74adfe0a175a905c87`
- 2026-01-06-931122, block 931122: `f84430398bd5c7f8cf65d982009b9be5e178b9de6c765391bb19d51b10518855`
- 2026-01-08-931397, block 931397: `5088382a437783a83f1fcfa647a54b5ca4198560b8512199f1032e3582f8d5ef`
- 2026-01-13-932101, block 932101: `eb25a4d2b7e462ba3082f7cdb9f3f4c29c93088ad4b29ca7e85460554de6d69e`
- 2026-01-15-932364, block 932364: `200233a82674bd28b3f9a4377c81b272cabab7d83ad36b3f5cc0ae381f649529`
- 2026-01-20-933074, block 933074: `a2db5f8d3c2612fbe99f60a96c8f8cdfc506512f5ff08b8ff944c1599357d47f`
- 2026-01-22-933353, block 933353: `886a8c216a415e334326c2a9ad89b0d003cd334a1856e10f6ad80af4052315e1`
- 2026-01-27-933957, block 933957: `91f68dba0ba7c06f96d3f2cc5216a9b769ab76b8fcf2a7e619685c72577ef5a9`
- 2026-01-29-934203, block 934203: `2cfc9732445becd4f0b3cdde1d8facfd26e7678b45bd8fd7996ba0654a9985b5`
- 2026-02-03-934831, block 934831: `923971095cdbcb840bd6432730a1b65d6c70a8a2b883edee5a3fba9d5d6f59ea`
- 2026-02-05-935107, block 935107: `bb1dacb4034e2890c0e1eacae7834bedde67fcf91c20051cb4b55ed4c4bb54fb`
- 2026-02-10-935890, block 935890: `9b4026114be812620b8d7ce69961a59982067b8c86fdc3d0dad55efca13d1299`
- 2026-02-12-936205, block 936205: `8857e10874b38135984e8f571eb3b3fb1bc4e97b6b0257cebb424d1f306133f5`
- 2026-02-17-937059, block 937059: `f3c8f8c2a0b64d90b2992dddb73786d5e538029aa0b62259f591939442546674`
- 2026-02-19-937387, block 937387: `2908538001e051d8307de763bc1c4589bf7654c4c443fc326f10fbacdd9a61b4`
- 2026-02-24-938104, block 938104: `9ec8f6626e6d8b95bd9adda5e2a3e6b8cf8fcfe36c7046e4d5c4f18a4d4f5167`
- 2026-02-26-938419, block 938419: `2ab67055852f8c488ccf66310cd5fa0e8173bcd823cfa7495a33bf5621bd3eb0`
- 2026-03-03-939113, block 939113: `99e99586061139f899725c2e90304ceea78f8e2fd42e983328611d51be467bf8`
- 2026-03-05-939406, block 939406: `a5515dd7305527afbb3e31f635c82436901c5a8a20279d5f81876e73cbe86903`
- 2026-03-10-940104, block 940104: `a1005cb322820e5eab029c455c709b167272b692f7042d140aa2574baff56e0c`
- 2026-03-12-940383, block 940383: `30fffe6680fe9e74c5bdad683d55dcc8105af10caa4195f3ffd11d6469a2269b`
- 2026-03-17-940995, block 940995: `225d4cbfe9344ac87d9e9c588ec30e4dfa17103fca974662f7d7543ffe152184`
- 2026-03-19-941271, block 941271: `c91b1dbf88e2ddb81d09857386a12c51b0a081300d896e1b3fa622ee18b709d1`
- 2026-03-24-941986, block 941986: `8789db16220c0840f6d8ec337077db8260ebed44b0202fe2fc214bc59a775b72`
- 2026-03-31-943047, block 943047: `b8bbfcc6291a0d0f57ec090e3c81c380ba006d8df358991411505dc0fecf3274`
- 2026-04-02-943340, block 943340: `d9bc994e3cab281cac2b015a3e498f95fbf2cb6b6ec17c9d5129da87b0398f47`
- 2026-04-07-944031, block 944031: `11528bb5cad0b7933f2fa58bb73dc9fa5f2a67ca686297039dcd12390e3d4860`
- 2026-04-09-944320, block 944320: `7a27ef9ba4ce83af788a6cf00c7de797891d23575395de331e10cff7bd345a55`
- 2026-04-14-945014, block 945014: `50a9cd79ff2c6f1c3ac72f9939b1389d4282a6f580d40a0bbe4a5fdcef58ce9c`
- 2026-04-16-945310, block 945310: `499b4414edd47a7016db07d1d8b9447521a2d054db1c203f14b2ada9c535abf1`
- 2026-04-21-946029, block 946029: `01835b0dc6888073dae4aeadd546615afcdad355b353596337e57ce5ce14a3c0`
- 2026-04-23-946290, block 946290: `46646ef5046364b70ec54abcb38eb66a5ec21748fd0b313efdd01db26a927087`
- 2026-04-28-947009, block 947009: `e6d159da3c4131fa1babdb08bfaf71c058be1037a79de390960e2c0ba406f14a`
- 2026-04-30-947270, block 947270: `8747ec70945ddf0e1f4ef1c3cd1a45fe1cfd8c7836a4e2c3a3a5e9b080602f45`
- 2026-05-05-948008, block 948008: `dae7c2e5ea433a7cda7c0ceadbe00bc246e2785940a39371404d9975fa67f182`
- 2026-05-07-948301, block 948301: `563a7f18e41a29a8966a812a59981535305f61b2fa48d86082ba635f40472544`
- 2026-05-12-949059, block 949059: `58c25c23923b18b1b240af9f84a5fbb433aa56c02e5f06c1d2bba56f2965d56f`
- 2026-05-14-949346, block 949346: `c08f74f9f135ff05990291d09c9b0406b8cd63f3a56ea468f28bfc5705913420`
- 2026-05-19-950062, block 950062: `da6407d8ffe29dea36c01a4299ed08953f93ce6cd3aa1a0ff6975690ff3cf431`
- 2026-05-21-950355, block 950355: `a98638adcfb0bc9017f90c337c4759ea0bb34af794d6d84ae351cda008708a6c`
- 2026-05-26-951112, block 951112: `7761de03670542ba15d085d99389f0db54f42c13cde5993e8047887887790473`
- 2026-05-28-951397, block 951397: `35781b29de9722c6fa89fe7a5e33417844876593ded5720c2d6c2151b51ca849`
- 2026-06-02-952097, block 952097: `9cd257a8e2dd89cc07f732984177270aacf651b066c0dbef6db5f77367f023a6`
- 2026-06-04-952320, block 952320: `69115ba80972ebaf4040d679e693ebeab74e350011322aeeefb9db9916beacb1`
- 2026-06-09-952958, block 952958: `29f6b83b3e0a5bb1cb842615892dde4dc9baa4ce316b80178e2e07c8f0d13906`
- 2026-06-11-953228, block 953228: `6a2d8d56fcd98736c5c99d2ed7d5e5e09839fa0ffbf5caca82273a86bf9ab1dc`
- 2026-06-16-953926, block 953926: `99e04f5270fc25f5178ccda9b01374ed80b2b24914af333a4ca786bfb0c90f64`
- 2026-06-18-954221, block 954221: `31a1bdd549d0734a5fe31a1e4fb62dee7293001a3ae447af0cc7ac208e854da5`
- 2026-06-23-954985, block 954985: `6707d6a1d1ed94b537b4381b2f7d9bba1e6790179a20aba9772e7ef7b883af39`
- 2026-06-30-956056, block 956056: `c209a9e74e45cce1a2aa21a43ee3be8b3c8a697f62c03aecd41014b01298f561`
- 2026-07-02-956345, block 956345: `4ad30cb719380ec670fa17601afc8a57faa6c1fdc4eda77f123e4d93b561ff78`
- 2026-07-07-957021, block 957021: `1a3fefd7a66de961bccade3d0434a77b02400db788d388a207cab24d36c3e977`
- 2026-07-09-957288, block 957288: `fe4202c0be92824b941ba1707faf2f75db8536a447e847cea7076150d421045c`
- 2026-07-14-957985, block 957985: `492ebd3e203800ed8dfe2908cccf9a5f56efe990ef9f23ab477c2867928646c3`
- 2026-07-16-958274, block 958274: `c18ed0e84a1a96057959848ee87af8f32a32295768887c37238d8867a5eb625f`
- 2026-07-21-958990, block 958990: `735d9d520c8651016f11eafe0adcdbd272ca0380c2ee89f728835704cf6381cb`
- 2026-08-11-961992, block 961992: `abff1542f3c9b223a0d4389eb381388d95707560f2d81789eeb842b2eed1fb25`
- 2026-08-13-962271, block 962271: `ea2c27f57ffa84f0d7c79a7f5227af6c3c17f536c2da1d71be7ece6fa5a295aa`
- 2026-08-14-962360, block 962360: `b6d2654a27d46a73e446c3b98a5837b5e788c4c1bc38f41ddb40f7d239cf9507`
- 2026-08-18-963019, block 963019: `cff5817c44454cd4e08b99544b466ea89cbbf7daf9d00f76d3b909d6e6a2e053`
- 2026-08-20-963287, block 963287: `aa5371a9239260769c98c3de031b9f48fed80bc0ca3cb2ff78da721855b66779`

`bybit.por` (www.bybit.com):

- 2024-09-04, block 859854: `cdc231cf95992a802b1cce1c25e4bd59b40630b92f0fbb6a5190243ff82266e5`
- 2024-12-12, block 874436: `3af81d484ed7e1d0959efc26ece192a6bf237bd6276827452e613f639b1b1c03`
- 2025-01-16, block 879520: `22a6154ff5a3371b459fbb5ac3b6e98170e590c6e72811f0a789ed8e92574068`
- 2025-02-23, block 885085: `91ffec837d4c526a9598edf4a957e2be34ba5b8e8e16e62ce856a41378aa5abd`
- 2025-03-27, block 889694: `503ba89f7285a3fa513102ebac379adc134b9cb1e977c1f8c029d77a9c6d2c7b`
- 2025-04-23, block 893673: `131ca377fdf041f9d1537d8e61487966a66c287e50df33a4a76dbfd6c7927e22`
- 2025-06-19, block 901835: `21af464f8191a30dee17371685fb10cb627a3d0e0c3db571c5c02ff936259210`
- 2025-07-24, block 906912: `df47bd7ef1099c6d574b27ec89d22376d31cb1e279255c679c86963f3648a2d7`
- 2025-11-19, block 924239: `ac290fa6ca1b03c5dadd09c42c6cf25dabb3e60e96df13e0f9c0aa99e326079a`
- 2025-12-17, block 928237: `ecd5bc25db3bbba46ea2675112d40b20f81f5241678ea7c0c95fb30e1323ed1f`
- 2026-01-27, block 933925: `a490eea84afce78815a748c9deba68797d56bc675240d8a38578ee5becfa8ca6`
- 2026-02-26, block 938378: `9d1a41597b2f2dad62306d1d3e398e38989b572527c104ec9b997e3222297dab`
- 2026-03-18, block 941086: `0a25b267493d618d5724b76b8c2e83116a7f12a141d18f9ec6c41960d026622e`
- 2026-04-22, block 946132: `f1fca867555a7fbadb01a62f651825b82731c696fe861eb8b50f4af19e31bafe`
- 2026-05-27, block 951216: `f1f3de378b5acfec08b73a38f58cf01632d2c6a20fc073f8277ef6c3bed08ed9`
- 2026-06-24, block 955120: `8f1774095b653dee00840789e765777abf9f537b7606e441b1a3dea348d6c9c8`
- 2026-07-22, block 959104: `d2eee6ecc21f0dfe4798f0fe550e5332aec395699c3d0d54da4628f06a7f6676`

`coinex.por` (file.coinexstatic.com):

- 2023-12-26, block 822997: `08a9235fbc08d1d331f73d05df02bb112ff2faeefea498785cc3802e66dba8e0`
- 2024-06-20, block 848717: `5517558585c2c60485bf95e4f6f2c71f99842e28fa43d343976b0780bb5a6e10`
- 2024-11-13, block 870111: `706bb78dbc76a2011b8411b8b4390220cdf31e5e7c99f3586dc34ff2b50ed34d`
- 2025-01-21, block 880191: `82a6407f28b91c967cc728a0da06a76ca7ba0aa6eccdce01709d167c61fd9fc6`
- 2025-02-25, block 885228: `8b94c8f7050b0c8cf20168c740dc7f5bba3d422aa6cb333308747d5634c0085f`
- 2025-03-31, block 890222: `4619be7950a594783d9f949c822eac769e7fbfd7b82bad657774706904a19125`
- 2025-04-29, block 894429: `7dbeadf7c23e6cc22284e73ed6634f0d42420d62d9df8730e34050bb442f85d2`
- 2025-06-04, block 899743: `15998b1643a49692497be42f42174811a97527fa9a38e1a018750a4129c50a44`
- 2025-07-08, block 904565: `e6640debbcfb3b0d0ab4c659c771242b5eef816a0a0c213b15b5c3dd1af3207e`
- 2025-08-13, block 909813: `4481b128ba6a32a2e739d200d43db7a6f0b4854964f438506805e06fc4acf6a2`
- 2025-09-08, block 913702: `c81c231ac97b82f167280c0dc7087f535beb8a1d3abe8485d270b99a9bbc032c`
- 2025-10-10, block 918411: `fc3192de609aa28e3c647cc009d1e550f3dd28da9fda88249dc8ad57ce3f57ff`
- 2025-11-13, block 923408: `80c3f4e925ca105f13feb7977aa9aaf920d321a148d345ff5a2d5c54e06bbb9e`
- 2025-12-17, block 928244: `2dfe63146cb3687b4ac666ed50214f4e2645d4912d61c221f613842696705412`
- 2026-01-22, block 933330: `ecabc0c3816135fa6ce2631baa233402b4b5b8422c238499a8631209a77307dc`
- 2026-02-24, block 938085: `126a34c49ff1218004c7b6df7843ac75a77f20debcf4de350ff54b4a9317e9ae`
- 2026-04-09, block 944275: `d563cdcbf2142e2be598e2672f402e82146ccf49aa7ab49cd335068b95711811`
- 2026-05-08, block 948394: `c2205a4d9f1c8a2af4067e71300f0b23b40ed0b81f65f676b68ba1200c304ad5`
- 2026-06-11, block 953210: `09e436ab7890d19d210939f2851b41443930dbd53624a58dbcf3fcbf31390d13`
- 2026-07-21, block 958982: `556f954b673b0ca153006073aea61323ffe965b2f8501c8ae5437ea304b0ad4a`

`cryptocom.wallets` (crypto.com):

- 2022-11-11, block 762776: `acc47945bf60156e90839e5ad99aa0d6a3b5c5e0d26c18c9f91dd94e86102cf2`

`deribit.por` (support.deribit.com):

- 2026-08-22, block 963597: `da985802e58cfec73ba0693ecd0a53df166cfadf6d7e88944d83101c3710ea31`

`htx.por` (raw.githubusercontent.com):

- 2022-12-01, block 765333: `ce4fb5e0719504a97bd9adc235f4035400cdf9b1439019a1873da1929a1fdce1`
- 2023-02-01, block 774468: `607ec441d100dbef0fad022fddce2b5911ce6136df56707031d4bbfcfc5c79f4`
- 2023-03-01, block 778675: `bdd93197f2a5fa89e047de0f874d761d72aa26b685b953feb4415a555a5bb900`
- 2023-04-01, block 783345: `7915a8e6903dd8cf231f6145a3bcb471e3318d56b062fb9bdfa2dd3603605cc3`
- 2023-05-01, block 787658: `eb535a13c9bf5f61e002e97125c99827cc6e795633eee4ca7b435e39d21cf4ef`
- 2023-06-01, block 792272: `5edf1e28262d513771b26d016f9c2b9adf1ec08cf3b22e8b7a1f0738c4751edd`
- 2023-07-01, block 796582: `6aa595b59e981e23027fedcff24cb33f174bdc5ee42cd47691c8f610f5d688b5`
- 2023-08-01, block 801090: `162ce48c0c8310cd5a5b30f769ecdff83d7245370678c55b1a9f142ef5f2f3ca`
- 2023-09-01, block 805602: `827fa990d8bc44a0fd94f23b64db8986e42f693464f5dc9153116b88c79ac79e`
- 2023-10-01, block 810035: `5c209c88aa8656c5261b1ed3afb07bf360a2a218fa6574a968fafba58be7047f`
- 2023-11-01, block 814701: `f71c382b8e9508a2e200c8a3e9a842d17c450c2b2ef0491b5e6f245c856fd77e`
- 2023-12-01, block 819136: `fc6694dedc8c289ebe309ebd65ed61440f1930a6c59cb9e86da02afafcb1d463`
- 2024-01-01, block 823738: `c7c14d24c30f7c5a8534dbc8f3712061b089397c4128ff9d004f1fb10e94db45`
- 2024-02-01, block 828255: `abc42bc16ad18f6c4308b5b2ac0298881aaf987fb2e74439d08930dc0c4c3ea0`
- 2024-03-01, block 832555: `d737b164846f0443aae70e3bd8f43f0295d2fb6a96176fa0deb680ab2e96950c`
- 2024-04-01, block 837115: `07982f048d15ee97ee4a3f458ec93b6f4bc5685d5f59400b110ce4e6530f7358`
- 2024-05-01, block 841532: `5fd1638bf619fa2c214c66bd524dc0dc103beb674f86a34578fab7a296bfe2fe`
- 2024-06-01, block 845930: `2e93e698454a9f8dc55da15b212b16722d3df767004ef66da681c52962502c9f`
- 2024-07-01, block 850119: `5e59c1a3745d45610f157ed4c9656d02cc8b7638796fc3200a36cecc4c56fd36`
- 2024-08-01, block 854822: `c2ccf262a7492f40224ccb6a59a3f709edd221a22cf4bfcd4c1d983ca347f8cc`
- 2024-09-01, block 859248: `585dbda60ed31e430777bc5964239d2ad63d3ad8a3b9400db2127e941d3704d6`
- 2024-10-01, block 863517: `3a381b6a7b4d13c0fd77ed453a31835a8ca9626e4189f5eddae98271d6bb2aa2`
- 2024-11-01, block 868266: `268011c2862202aaca9c75656b94677ea5a7f89b77b32db9fb15303c92158831`
- 2024-12-01, block 872639: `d6b51005428a7f70f9c76963c23276985c01f87d4c4b84f1cb0f07ebe79ce600`
- 2025-01-01, block 877221: `64c82f1f711a1f69bd86681eddfb3f32045e89c9a9e40c0affb21b1014032a40`
- 2025-02-01, block 881659: `a0a0c365a3819ac3887628a30bad62956b9670c2de334dcb7ebd0fe0483728e1`
- 2025-03-01, block 885737: `356a84968093af897508fc9476f6827ff2a2645b5ed47670df812721e1c8a6a4`
- 2025-04-01, block 890266: `d78f817b5080bb54a05417c5cf26d4eaf583bd227f3a45716ef40cc0f2c5795a`
- 2025-05-01, block 894626: `a26de12e793d458b915a308f4e14c23b6ae4210ceb2948a5e4c93d36d0011475`
- 2025-06-01, block 899243: `f0f5e41cddcdfe5314f01cca65f26e037babc9c47e65e72f27a44c9c8fce67e1`
- 2025-07-01, block 903412: `42bf3fd0c585cc1828030d99b9c8d06cf97b448a85743a39fbedf64aee3de25d`
- 2025-08-01, block 907972: `0d2e5d6a4a080ee18559b02c5a62ea7f6214cdba9f254e84db598c1786a215f6`
- 2025-09-01, block 912570: `c8d1b76520421c9eb8010f44839d3e64505087ef3ecf6199216297c20b081bd0`
- 2025-09-10, block 913920: `152196834ea8bc6e070c30732f73053a5e7f47c64832cd66989072b29d45872b`
- 2025-10-01, block 917089: `4fd93f1b07843a9ef655c2ae3118e1cc3695023918a4db242669ee04c18d7f55`
- 2025-11-01, block 921627: `225a30b3528da264eccde76e58dddf1d40e03dd982d570ff30799280476ceab0`
- 2025-12-01, block 925888: `a753c0f768c73b221fd808df0ea3c87bb9c999f5237c6c86873de9eee6e98a15`
- 2026-01-01, block 930288: `73de4f6ec4f707b8d3af939e8782151e968e90f924de10e58e41e4e89fa71149`
- 2026-02-01, block 934496: `21be8ca6124432cf98d6ad4f9f9c8a9b3747ca21cc64f245e0c9915d0df2ede4`
- 2026-03-01, block 938735: `6bb988eae52ca0a588a446b2f57e8d654f817d67bd050ca7fa38b4ef3b3193fe`
- 2026-04-01, block 943083: `b953ca0d51e16e90ff74d25180e2ce59073a11744fdf17141923ac3a29c52076`
- 2026-05-01, block 947308: `15eeef6261187e3635ec58f77ecc8840e4e4f34783438870b3aa3b85ff114ced`
- 2026-06-01, block 951871: `b3c9c9d0dce8aaee5ae6db8a67e3d7d37fa122c6d4126c8e3fbcc2eae2e48cb0`
- 2026-07-01, block 956087: `2c42070cf4437a650ea3e076ad4187da7ffa74d9d8f3b99d5d34677a8eefe71a`
- 2026-08-01, block 960427: `c3bc72f76d29d5f539c88888e00c0dea27f2cdfa5edc9bfcbe91864e3896d24b`

`mexc.por` (por.mocortech.com):

- 2026-01-09, block 931425: `c6d84174cac2023d903d7c9f475ec51f01f6ca72065ff4a3bfa3ea4f4e738637`
- 2026-02-10, block 935766: `edd02abf5da82abde57baf50ddf7a61cc781648e15350964cf2bb31d1c22e20c`
- 2026-03-10, block 940012: `1cf47a8cc612ce6644ba24ed48c10c000e235d452654c30f5a084504f63cfdef`
- 2026-04-10, block 944355: `a6d0c404174060f820146ded57d448d93304344cca8b4ff6de857a1166704789`
- 2026-05-10, block 948634: `468b7d6b3a5835174a874521c46bd1a75a0132da9f08f88e624a6b62ccb9fe6f`
- 2026-06-10, block 952990: `82107a0e83d067d0def200195f9654cb058d026db2882997e81e485e253d2461`
- 2026-07-10, block 957322: `752c4323897638dd3cedcde095c5ecc25bd8022078579224b05ae2249036ae00`
- 2026-08-10, block 961747: `2f0465f32809c69199e91ed9238d2c6f308fc64c37f983361e407506e7ed1f83`

`okx.por` (static.okx.com):

- 2026-01-17, block 932619: `2b6fec477182a3d960c48f6d2fc6824f298d6894b559dcbb9c405389333ecd22`
- 2026-02-04, block 934973: `36ee17bdb56aee71a4c58edd6d1aea49027fde6672b9049efc57f41bd75457dd`
- 2026-03-03, block 939123: `c0dec756aace721fe8a483d530a1cb88e3b38d4806fec94f274712a977639174`
- 2026-04-20, block 945799: `fb55af0ad9bd7a2c70d72c0e42b8cb358eab412fa336d5a68e1a2c509cd719b0`
- 2026-05-07, block 948185: `bf4eb2b7de9c88587166a4ead2e2ee1617381877875b00f2a775462bb9681009`
- 2026-06-19, block 954269: `1fd7764c9beafcf34141eaffd0352d1653040bb0cd30a1d8d1c2cd9ae408d139`
- 2026-07-07, block 956934: `cd007043bc9dc81acdf4457a98cdb8d4afd3821023702352824d30d550354dd2`

Signature results (Onyx's own verification; a signature counts only over a message that names
the issuer): OKX 26,214 of 26,214 rows signed; HTX 127 signed ("huobi"), 88 published (their
signatures verify but over "King will be back!", which names no issuer), 69 published (no
signature in the file); CoinEx 100 signed, 38 published (cold multisig wallets:
CoinEx publishes owner-key signatures but no redeem script that binds them to the address), 18
published because their signatures do not verify (newer hot-wallet proofs; the indexer reports
the same). BitMEX: every published redeem script hashes to its address (0 rows dropped); output
descriptors are not expanded.

Heights (basis column): `issuer`, the block height the snapshot itself states (OKX, BitMEX,
Binance, HTX, CoinEx); `indexer-mapped`, the first block after the snapshot time by the indexer's
header-timestamp mapping (MEXC's exact API times; Bybit's audit time, or the end of the audit date
at UTC-12 where the report states no time; Bitfinex's first wallet-file commit; Crypto.com's
disclosure date, first block of the next UTC day), block times kept in `snapshots.json`;
`fetch-tip`, the indexer's tip when it fetched a current roster, plus one (Deribit, Bitfinex
current); `bound`, a block mined after the fetch that bounds it (BITB). Not yet checked against
headers by Onyx (THREATS ROS-3).

Left out, with reasons:
- wBTC/BitGo (`exchange_por_wbtc.csv`, wbtc.network): its terms forbid reproducing or
  distributing its materials and automated access (IDX-R1-F1). Hard limit.
- Coinbase cbBTC: the page's terms are unread and the rows came via a Wayback capture and a
  person's relay (IDX-R1-F2, D4). Not an issuer snapshot Onyx can reproduce.
- OKX, BitMEX and MEXC snapshots before 2026-01-01 (19, 304 and 7 snapshots; up to 110,775,
  336,705 and 28,848 addresses each): recorded by hash in `snapshots.json`, not imported (size).
  Version 4 cannot add them under this name (no back-dating, ADR-0037 item 4).
- BitMEX's `custodial:` section (customer deposit custody) and Binance rows held by a third-party
  custodian (Ceffu): custody, not exchange addresses.
- Bitfinex's 2022-12-12 wallet-file commit: no block height mapped for it (its Bitcoin wallets
  equal the current file's).
- BITB's 52 rotated-out wallets (in v2): their Wayback captures' bytes were not kept, so no
  issuer snapshot can be hashed; v3 holds the 65 wallets of the 2026-08-24 page.
- One OKX address a government filing names (OFAC SDN, an entity listing): dropped by the
  deny list `fixtures/rosters/deny.txt` (addresses only; built from the indexer's
  `filings_gov.csv` address column, whose names are never read). `deny.basis.json` records the
  filings file's sha256 (fetched 2026-08-10) and the digest of the 16,556 roster addresses the
  list was checked against; `build-rosters.py listings` refuses any other roster content until
  `deny` is re-run (review P2a-R2-1).
- No third-party, inferred or clustered row is read (no Dune, Arkham, WalletExplorer, Blockpath,
  CoinGlass, TimeChainIndex, GraphSense, bitcoin.it, the unsourced IBIT/GBTC file, or any customer
  export). Raw pages and PDFs are not committed, only their address extracts, so no page text
  (staff or auditor names) ships; no commit metadata is copied. Every label names a company or
  a fund; no individual (THREATS ROS-1).
