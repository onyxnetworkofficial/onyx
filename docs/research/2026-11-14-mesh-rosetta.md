# Research: Coinbase Mesh (formerly Rosetta) and Onyx (2026-11-14)

Sources, read on 2026-11-14: the `mesh-specifications` README and its `api.yaml` (the endpoint
list), fetched from github.com/coinbase/mesh-specifications; search results that point to the
Coinbase developer docs (docs.cdp.coinbase.com/mesh). Those docs pages were not opened, so their
claims below are marked "(docs, not opened)". "(Onyx proposal)" marks our own suggestions.

## Question
Mesh was an earlier try at one interface for many blockchains. What can Onyx take from it?

## Short answer
- Mesh is one API that every chain team implements on its own node. Onyx is one API on top of
  many sources, with proofs. They solve different halves of the same problem.
- Mesh trusts the node you call. It has no proofs, no signatures and no way to check two nodes
  against each other. Onyx adds exactly that layer.
- Two things are worth taking: (1) a Mesh source adapter, so that one adapter reads every chain
  that has a Mesh node (Onyx proposal, ROADMAP F14); (2) a conformance tool for Onyx adapters,
  like `mesh-cli` (Onyx proposal).

## What Mesh is
- "An open-source specification and set of tools that makes integrating with blockchains
  simpler", written in OpenAPI 3.0 (README).
- Two parts (api.yaml):
  - **Data API** (read): `/network/list`, `/network/status`, `/network/options`, `/block`,
    `/block/transaction`, `/mempool`, `/mempool/transaction`, `/account/balance`,
    `/account/all-balances`, `/account/coins`, `/call`.
  - **Construction API** (write): `/construction/derive`, `preprocess`, `metadata`, `payloads`,
    `combine`, `parse`, `hash`, `submit`. It builds and signs transactions offline and keeps no
    state (docs, not opened).
  - Optional indexer endpoints: `/events/blocks`, `/search/transactions`.
- Every value is described the same way on every chain: a network id, a block id (index and
  hash), transactions made of typed operations, and amounts with a currency and its decimals.
- `mesh-cli` tests an implementation; its `check:data` replays blocks and checks that the
  balances it computes from operations equal the node's `/account/balance` (README; check details
  from the docs, not opened).
- Reference implementations exist for Bitcoin and Ethereum; community ones for other chains are
  listed in `mesh-ecosystem` (README).

## Mesh and Onyx side by side

| | Mesh | Onyx |
|---|---|---|
| Who runs it | each chain's team or an exchange, on its own node | any node operator; data from many sources |
| What it serves | raw chain state: blocks, transactions, balances | derived data: metrics, datasets, events, with history |
| Trust | the caller trusts the node it calls | every answer carries a proof; `onyx-check` verifies it against a signature or a root on chain |
| Many chains | one spec, one implementation per chain | one API; source adapters per chain or source |
| Writing | Construction API (build and submit transactions) | none for chains; Onyx only reads |
| Tests | `mesh-cli check:data`, `check:construction` | unit and property tests per adapter; no shared conformance tool |

## What Onyx can take (Onyx proposals)
1. **A Mesh source adapter (ROADMAP F14).** One adapter that reads the Data API covers every
   chain with a Mesh node. Rules, as for every source:
   - read at least two Mesh nodes from independent operators and refuse a value they disagree on
     (one Mesh node is one trusted party);
   - read only final blocks (the depth per chain from `/network/status` is not a finality rule;
     each chain needs its own);
   - entities only, never individual accounts (THREATS and the source rules in ROADMAP Phase F);
   - a rights record first, like every Phase F source.
2. **A conformance tool for adapters.** Like `check:data`: replay a window of history through an
   adapter and compare its totals with an independent total (for example the supply a chain's own
   node reports). Onyx's adapters each have their own tests today; one shared check would catch a
   new adapter that is wrong in the same way.
3. **One value model.** Mesh's amount (value, currency symbol, decimals) matches how Onyx bulk
   datasets keep decimals exact (`onyx-bulk` decimal columns). A Mesh adapter can map an amount to
   a decimal column at the currency's decimals with no rounding.

## What not to take
- The Construction API: Onyx does not write to chains for users.
- Mesh's trust model: a single node's answer is what Onyx exists to replace.

## Open questions
- Which Mesh implementations are still kept up to date. The ecosystem list was not checked.
- Whether two independent public Mesh endpoints exist for any chain, or whether Onyx node
  operators must run their own Mesh nodes (more disk and work per chain).
