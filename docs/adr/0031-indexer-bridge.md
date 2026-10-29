# ADR-0031: Indexer bridge (label set v3 from the entity indexer)

## Status
Accepted, 2026-10-11 (the loop decides; the owner: "work on adding the entity indexer to
everything"). Amended 2026-10-11 after review IDX-R1 (see Amendment 1): no v3 fixture ships.
Implemented by `scripts/build-label-set-v3.py` (the bridge converter: v2 rows plus cleared
sources, none yet), `onyx_source_btc::label_kind` (`Custodian` accepted) and
`tests/labels_bridge.rs`. Phase 2+ is planned in `docs/ops/indexer-integration.md`.

## Context
The owner's the entity indexer (read-only, `~/projects/the entity indexer`) attributes Bitcoin
addresses to entities. It grades claims by how they are known (`EVIDENCE.md`: proved, strong,
weak, never run) and labels rows by evidence class (`signed`, `published`, `published_relayed`,
`chain_attested`, `filing`, `inferred`, `*_thirdparty`). Its on-chain attribution beyond the
seeds comes from common-input clustering, which it grades **weak**: 24.3% of attributed scripts
sit in unresolved ownership contests, nine entities move more than 2% with input order, and the
full-space clustering route is refuted (`EVIDENCE.md`). Onyx's hard limits: companies only,
never individuals, and no data whose license may forbid redistribution.

## Decision
1. **Import seeds, not clusters.** v3 takes only rows whose source is the company itself:
   | Indexer class | Source | Onyx |
   |---|---|---|
   | `signed`, verdict `verified` (OKX PoR) | `exchange_por-verified.csv` | imported; first-party signed, 990,000 ppm |
   | `published` by the exchange (BitMEX, Binance) | `exchange_por.csv` | imported (as v2); 950,000 / 900,000 ppm |
   | `published` by the ETF issuer (Bitwise BITB) | `etf_custody.csv` | imported (as v2); 950,000 / 900,000 ppm |
   | `published` custodial reserves (wBTC by BitGo) | `exchange_por_wbtc.csv` (custodian's own API) | **excluded** (Amendment 1): wbtc.network's terms forbid redistribution and automated access |
   | `published` / `published_relayed` custodial reserves (cbBTC by Coinbase) | `exchange_por_cbbtc*.csv` | **excluded until its terms are read** (Amendment 1): rows via a Wayback capture and a person's relay |
   | cluster expansions (H1-H5, any grade) | the indexer's store, `out/` | **excluded**: weak grade, measured false merges; a false merge can label a person's address |
   | `inferred` | `exchange_seeds_converted.csv`, `unattributed_*` | **excluded**: no external claim of identity |
   | `*_thirdparty` (Dune and other aggregators) | `*_dune_thirdparty.csv` | **excluded**: redistribution terms not established |
   | `chain_attested` mining pools | `mining_pool_payouts.csv` | **excluded** (LBL-V2-F1): payout addresses are often individual miners |
   | `filing` | `filings_gov.csv` | **excluded as labels** (names natural persons); its addresses stay a deny list |
   | Mt. Gox `published` rows | `exchange_seeds_converted.csv` | **excluded**: relayed by a wiki, not published by the company |
2. **Proof levels (on main today, ADR-0027).** Signed PoR rows are level 3 (source-proven);
   published lists are claims (level 4 on main, "a claim, bond planned"). A shard's level is the
   weakest of its inputs, so every v3 shard is level 4 (`label_set_proof_level`), exactly like v2.
   When level 4 returns from its parked branch with `Unstaked`, unsigned published lists move
   there (ADR-0029 amendment 2); v3 needs no change beyond that mapping.
3. **Custodian, not owner.** wBTC and cbBTC addresses are custody reserves of wrapped-token
   products, not the custodian's own holdings. The entity ids say so
   (`entity:wbtc-bitgo`, `entity:coinbase-cbbtc`) and the kind is `Custodian`.
4. **Guards.** The converter stops on any entity outside the company allowlist, any unknown note
   format, any row from another source URL or evidence class, and any address labelled for two
   companies; filed addresses are dropped (v2's deny list). v1 and v2 stay byte-identical.
5. **Fuel.** Label decoding is the fixed part of each run (SER-1). The first v3 draft was 10,302
   labels; with Amendment 1 no v3 fixture ships, so every run uses v2's 9,850 labels (AUD-2-F12).
   A future v3 re-plans shards with the preflight, which stays the authority (each shard <= 50%
   of its fuel).

## Consequences
- With wBTC and cbBTC excluded, no indexer source beyond v2 is cleared, so v3 would equal v2:
  no v3 fixture ships, and the site, the history and the API keep running label set v2.
- The bridge stays: the converter (`CLEARED` and `EXCLUDED` lists with reasons; it stops if an
  excluded source is ever listed as cleared), the `Custodian` kind, and this ADR's import rules.
- The indexer's richest numbers (cluster balances, filings, ETF parity) stay outside Onyx until
  they can enter with a proof level and clean rights (Phase 2 plan).
- Test labels only: v2's sources publish addresses for public verification and state no license
  that forbids it; `test.*` sets are never served to customers (R-LBL-FIX-1).

## Amendment 1 (2026-10-11, review IDX-R1)
1. **wBTC dropped (IDX-R1-F1).** wbtc.network's terms of service say materials may not be
   reproduced, distributed or republished without prior written permission, and they forbid
   automated access. The 231 wBTC rows came from that site's API, so importing them breaches the
   hard limit. They are excluded, and so is any Phase 2 plan to re-observe that site.
2. **cbBTC held back (IDX-R1-F2).** Coinbase's terms for the cbBTC reserve page could not be read
   (the page refuses our fetches); the rows came from a Wayback capture and a person's relay. By
   the conservative rule they stay out until the terms are read and recorded ("blocked on rights
   review").
3. **No v3 fixture.** v3 = v2 rows; shipping an identical set under a new name would only move
   every pin for no data. The bridge converter stays, ready for the first cleared source.
