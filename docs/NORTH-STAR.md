# North star

**Onyx is the place where any fact can be checked by anyone.** Anyone can add data. Other people
check it, by the strongest method the data allows. Every value carries its proof level and the
evidence behind it, and a buyer, an app, an AI agent or a market can verify it without trusting
Onyx.

This file is the master list: what the partners asked for, what makes Onyx new, and every item
that gets it there, in order. The project owner asked (2026-10-25) for everything to be built
toward this; the loop works it top down and keeps this file current. Rules that never bend:
testnets only until contracts are reviewed; no data whose licence forbids redistribution; labels
name organizations, never people; claims about other projects link a source; everything public is
anonymous.

## 1. What the partners asked for

From the 20 October call and the partners' written questions (the private partner notes hold the
details).

| They asked | What Onyx does about it | Items |
|---|---|---|
| Lowest friction for contributors and for buyers | One "Add data" flow that picks the proof level for you; an API every agent can read | NS1, NS8 |
| Stake to submit; lose the stake if wrong | Staked claims, decided by observation, then a stake vote, then a drawn panel (testnet) | built (ADR-0042), NS11 |
| Attack cost like proof of stake | Attack cost computed and shown; observers override votes | built, NS11 |
| No reward for re-submitting what everyone agrees on | Duplicates earn nothing | built |
| Futures on future values | Parimutuel futures that settle only on recomputed or re-observed facts | built (testnet), NS11 |
| Give an API key without Onyx seeing it | A real TLSNotary proof works; the browser prover is next | NS6 |
| Who pays validators when nothing is disputed | Open: needs the token decision | NS12 |
| Spam and rate-limited sources | Per-source fetch budget built; validate only after a buyer pays | built, NS10 |
| Storage tiers | Hot, warm, cold (cold = root on chain, recompute on request) | NS13 |
| Salted rows so a buyer cannot guess other rows | Built and served one row at a time | built |
| Buyer privacy | Prepaid balances with batched settlement (contract built) | NS9 |
| First user: API and agents | OpenAPI file and agent toolkit (MCP server, `onyx-check`) built | NS8 |
| Estimates must never pollute checked totals | Own namespace and level, method and error shown | NS14 |
| Token: subsidy creates junk; wash queries | Buyers pay USDC; a token only after real usage; emission per query below fee minus miner share | NS12 |
| Most non-crypto data is level 2 | Make level 2 the strongest it can be: independent observers, live sources, many domains | NS2, NS3, NS4 |

## 2. What makes Onyx new

Each pillar says what exists elsewhere (with a source) and what Onyx adds.

1. **A proof level on every value, for any data.** Data APIs sell numbers; oracles sell answers
   decided by vote or by a few signers. Onyx labels every value with how it was checked
   (recomputed, re-observed, source-signed, claimed, or scored later) and ships the evidence.
   A buyer picks the lowest level it accepts.
2. **Settle by recompute, not by vote, wherever the data allows.** Prediction markets resolve
   disputes by token vote (Polymarket uses UMA's optimistic oracle,
   [docs](https://docs.polymarket.com/concepts/resolution)). Onyx reruns a pinned program on
   pinned inputs, and anyone can repeat it in a browser.
3. **Check it yourself.** The website runs the network's own Rust checks in the browser; the API
   ships a Merkle proof and a verify bundle with every row.
4. **Values climb the ladder.** A claim enters at level 4 on day one and rises when stronger proof
   arrives (a later observation, a source signature, a TLS proof); the level history is public.
5. **Markets on data itself.** Stake on whether a value is right; bet on a future value; settle only
   on recomputed or re-observed facts.
6. **Keys without sharing.** A contributor proves "this exact answer came from this website" with
   TLSNotary, without showing Onyx the key.
7. **Rights travel with the data.** Every source carries a rights record whose hash is in every
   proof; restricted rows are salted and sold one at a time; data nobody may share never enters.
8. **Agents first.** Every route is described for machines (OpenAPI), payable per call (x402), and
   checkable by the agent itself.
9. **Contributors own the commons.** Paid per accepted contribution and per use; old data becomes
   free.
10. **Track records.** Sources and forecasters are scored later against checked facts, with a proper
    scoring rule; the score is itself provable.

## 3. The build list

Status: `todo`, `doing`, `done`, `parked (why)`. Size is a rough time for the loop.

| # | Item | Pillar | Size | Status |
|---|---|---|---|---|
| NS1 | **Website for any data.** Rewrite Home, How it works and the API page so they speak about any data (public records, APIs, filings, sensors, own data, forecasts); on-chain data is one example. Company totals appear only as one sample dataset | 1 | 1 day | done 2026-10-25 (How it works: "One network for any data", six kinds with their level; Data page "Browse data", Bitcoin as the live sample; README tagline) |
| NS2 | **Live observers for any published date** (pre-public loop PP9) | 1 | 4 h | done 2026-10-25 (rehearsed live for 2026-10-22) |
| NS3 | **Two datasets outside finance, level 2:** earthquakes from the USGS feed and weather observations from the U.S. National Weather Service (both U.S. government works; rights record and terms review first, as for every source) | 1 | 2 days | done 2026-10-26 (ADR-0027 amendment 17: the daily rule and the shared runner `onyx-level2`; `adapters/source-usgs` M5+ quakes per day and `adapters/source-ncei` Denver TMAX, both rehearsed on recordings and live. Weather comes from NOAA NCEI GHCN-Daily: the NWS API keeps only about 7 days. Both on the data page, checked in the browser and sealed in public builds. Next: serve them on the hosted API, NS17) |
| NS4 | **Independent observers:** observer processes on separate machines and operators; a signed observer registry | 1, 2 | 2 days + an operator | todo |
| NS5 | **The ladder in the product:** every value shows its level and evidence; a value's level rises when stronger proof arrives, with the history public (API field, site badge, events) | 4 | 2 days | done 2026-10-27, slice 1 (ADR-0048: `proofLevel` and `ladder` on every value answer, forged levels and falls refused, badges and history on the site); slice 2 done 2026-10-28 (amendment 2: cross-lane form checked per lane, contradictions, level-3 ladders, levels bound to the caller's registry; serving cross-lane: ROADMAP C68) |
| NS6 | **Browser TLS prover:** a contributor proves a keyed API answer in the browser (TLSNotary), the key never leaving the page | 6 | 3 days | parked 2026-10-28 (upstream tlsn still pre-release alpha.15; browser path is a Chrome extension; unblock steps in docs/north-star-loop.md NL11) |
| NS7 | **Go public, anonymous** (PP13): the public repository, the site at onyx.cryptagraph.com, contacts through GitHub | all | 1 day | doing |
| NS8 | **Agent toolkit:** a small client library and an MCP server built from the OpenAPI file, so AI agents can find, pay for and verify Onyx data | 8 | 1 day | done 2026-10-26 (ADR-0047: `onyx-check` runs the Rust client check with the root read on the agent's own node; `apps/agent` client and MCP server; `scripts/agent-smoke.sh` in check-all. Next: `onyx-check` kinds for level-2 observations, signed facts and track records) |
| NS9 | **Prepaid balances in the node** (PP10) | 7 | 1 day | done 2026-10-25 (HTTP, chain pass rehearsed on anvil, `ONYX_PREPAID`; a node rehearsal open in ROADMAP S7) |
| NS10 | **Validate on first paid query:** proposals are free; observers run when a buyer pays or the submitter pays a small fee (stops spam without a gate) | 9 | 1 day | done 2026-10-27, slice 1 (ADR-0049: a level-2 subject no round settled yet is offered for a prepaid hold; one round per fact, one hold per buyer; charged after the checked round is stored and served, released in full otherwise; `/v1/tickets`, `ONYX_OBSERVE`; not deployed; left in ROADMAP S9) |
| NS11 | **Markets live on testnet:** deploy stakes, panel and futures to Base Sepolia; the Markets page reads them | 5 | 2 days | done 2026-10-27 (contracts live on Base Sepolia, ADR-0042 amendment 2: futures settle from registry level-2 facts through `OnyxObservedFacts`; market 1 on Treasury debt for 2026-10-27 open with four test bets, settles after the 2026-10-29 round; the Markets page reads the chain through the reader's RPC; the site is not deployed yet) |
| NS12 | **Token economics** with the partners' numbers: emission per paid query below fee minus miner share, validator pay for undisputed claims, seat limits on the panel | 9 | partners first | parked (needs the partners' decision on Monday 2026-10-26) |
| NS13 | **Storage tiers:** hot, warm, cold (root only, recompute on request) | 3 | 2 days | doing 2026-10-28 (slice 1, ADR-0052: `onyx_coordinator::tiers`; the node's use of it is ROADMAP S11) |
| NS14 | **Estimate layer rule:** estimates in their own namespace and level, method and error shown, never in checked totals | 1 | 1 day | done 2026-10-28, slice 1 (ADR-0050: `estimate.` metrics at level `E`, `/v1/estimates`, refused by every checked entry point, shown apart on the data page; slice 2 is ROADMAP C67) |
| NS15 | **Bounty board:** coarse categories of wanted data, never exact queries | 9 | 1 day | done 2026-10-28, slice 1 (ADR-0051: closed-list wants from prepaid accounts, 5 accounts per shown category, buckets, a weekly cut; `/v1/wants`; the Add data page's panel) |
| NS16 | **Track records served:** source and forecaster scores on the API and the site | 10 | 2 days | doing 2026-10-28: slice 1 (ADR-0036 amendment 7) and slice 2 (amendment 8: the NWS forecast as a labelled testnet forecaster, through the intake on Base Sepolia, live panel on the data page); grades come from December |
| NS17 | **Hosted API node** next to the site: the live query API on testnet | 3, 8 | 1 day | done 2026-10-27 (live at https://onyx.cryptagraph.com/v1/: `scripts/deploy-api.sh` run with the owner's OK; `/v1/health` 200 on Base Sepolia (chain 84532, settlement 0xe4be…ae8f), 2 servable shards, payments off and no key on the server, Caddy's admin endpoint closed, other `/v1/` paths and POST 404, the site's live daily check 11/11. Label data answers "withheld" until the rights are cleared. Level-2 daily values: ROADMAP B6) |
| NS18 | **Pooled private data** (encrypted contributions computed in a protected environment) | 7 | research | parked (after paid demand) |

Order: NS7, NS1, NS2, NS8, NS3, NS5, NS9, NS10, NS17, NS11, NS6, NS4, NS13, NS14, NS15, NS16;
NS12 when the partners decide; NS18 later.

## 4. Only the project owner can

1. Send the label-rights letters (drafts exist).
2. Create the neutral GitHub organization for the public repository.
3. Decide the token question with the partners (NS12).
4. Find a second operator for independent observers (NS4).
5. Approve anything that costs money.

## 5. Log (newest first)

- 2026-10-28: NS6 parked (NL11): `tlsn` is still `v0.1.0-alpha.15` (pre-release), `tlsn-js` is
  archived and the browser path is the `tlsn-extension` Chrome extension with a Node verifier
  server; an in-page build is unproven. Next step when picked up: a labelled experiment that
  proves the Treasury target from a page against a local verifier.
- 2026-10-28: NS13 slice 1 (NL10, ADR-0052): row batches move hot, warm and cold in
  `onyx_coordinator::tiers`; a move writes and reads back before it drops; a shard goes cold only
  over chain input and only after a rebuild matched the chain root; a recompute is stored only on a
  match; recomputes need a paid hold and are capped. The node does not use it yet (ROADMAP S11).
- 2026-10-28: the join kit (north-star loop NL16, toward NS4): an operator outside Onyx joins a
  level-2 round from docs/join.md with `scripts/join.sh`; rehearsed in a fresh container against a
  local test chain (drawn, committed, sealed reveal on chain, agreed). NS4 stays open: no outside
  operator has joined a live round yet, and bonds wait for NS12.
- 2026-10-28: NS5 slice 2 (ADR-0048 amendment 2, ROADMAP C64): cross-lane ladders defined and
  checked per lane, contradictions shown without moving a level, level-3 answers carry their
  ladder, `onyx-check` binds levels to the caller's registry, clients and the site remember the
  ladders they accepted, unchecked badges say "stated". Serving the cross-lane form waits for a
  fact with two lanes (C68). Not deployed.
- 2026-10-28: NS15 slice 1 (ADR-0051): the bounty board. A want is four values from closed lists
  (kind, domain, region, cadence), signed for the week by an open prepaid deposit; the node keeps
  salted account hashes and shows a category only from 5 accounts, as a bucket, changing weekly.
  The Add data page asks for the board on request. Posting needs a prepaid contract (closed on the
  hosted node today).
- 2026-10-28: NS16 slice 2 (ADR-0036 amendment 8): the NWS forecast of the Denver high is a labelled
  testnet forecaster (NOAA grades NOAA; an Onyx test key relays it). Six days were stated through
  the track-record intake on Base Sepolia; each stays sealed until its day ends and is graded by
  the NCEI record from December. The data page checks the node's record in the browser and says
  "too few to score" until 30 days are graded. Served after the next deploy.
- 2026-10-28: NS16 slice 1, then parked: daily level-2 metrics get a close rule and are resolved
  only by rounds the client's node shows recorded at level 2; a publication under these rules is
  served and checks. The first forecaster, the NWS forecast of the Denver high, fails the
  self-grading rule (NOAA also runs the NCEI record): an owner decision. Not deployed.
- 2026-10-28: NS14 slice 1 (ADR-0050): estimates under `estimate.` at level `E`, never a proof
  level, served only at `/v1/estimates` with method and error band, refused as an input by
  shards, level-2 feeds, recipe sources, label sets and track resolutions; the data page shows
  the first one apart (mean daily Bitcoin fee of the checked window's whole days, 95% interval,
  made in the reader's browser). Not deployed yet.
- 2026-10-28: release `2f01280` live (owner's "Deploy"): the six level-2 series at level 2, intake
  on, the 443 cap loaded; the site's Markets page reads market 1 and both insurance products from
  Base Sepolia. `/mcp` waits for Node on the host (owner's OK).
- 2026-10-28: release prep (north-star loop lane A): NS11 markets open only on guarded futures
  with `closeBy` from the source; the Markets page also shows the two test insurance products
  (NL18), read from Base Sepolia and checked; the web server's deploy turns on proposals, the MCP
  connector at `/mcp` and the 443 connection cap; `scripts/quickstart.sh` verified a hosted value
  off and on chain. `ONYX_WEB=1 scripts/check-all.sh` passed. Not deployed yet.
- 2026-10-27: NL17 (ROADMAP S13): four more level-2 series from the same reviewed sources:
  Denver precipitation, New York Central Park high temperature and precipitation, and M2.5+
  earthquakes within 300 km of Los Angeles; live rounds on Base Sepolia (docs/ops/demo-testnet.md).
- 2026-10-27: THREATS NS5-8 closed, the owner's choice "free" (ADR-0048 amendment 1): values of
  free public sources (Treasury, USGS, NCEI) are free to read on the site and the API, unsealed
  with their level history; the SEC holdings stay sealed.
- 2026-10-27: NS10 slice 1 done (ADR-0049, north-star loop NL8): proposals stay free and no
  round runs without money behind it. A request for a level-2 subject no round settled yet is a
  402 with an `observe` offer; a prepaid voucher is held (never counted or redeemed), the request
  gets a ticket, and the node's worker runs the round through the operator's command; the checked
  round is stored and served, then the hold is charged; a failed, forged, late or wrong-day round
  releases it in full. Not deployed (the owner's one deploy at the end).
- 2026-10-27: NS11 done (not deployed): the Markets page shows the live Base Sepolia markets above
  the practice markets, read from the chain in the reader's browser and checked by the Rust
  verifier; market 1 read live at 1280 and 390 px (125 tONYX, trading open).
- 2026-10-27: NS11 contracts live: tONYX, stakes, the drawn panel and the futures deployed on Base
  Sepolia (8 transactions, 0.000351 test ETH); market 1 on the Treasury's debt for record date
  2026-10-27 opened before the Treasury publishes it, with four test bets (125 tONYX). It settles
  from the level-2 round of 2026-10-29 (`OnyxObservedFacts.prove`, ADR-0042 amendment 2). The
  Markets page reading the chain is next (it waits for the NL2 site deploy).
- 2026-10-27: NS5 slice 1 done (ADR-0048): every value answer states its level, and row, metric
  and observation answers carry a ladder of rises with evidence and chain record; the client
  refuses a forged level, an unchecked rise and a fall; the data page shows level badges and the
  checked level history of the Base Sepolia values. Cross-lane rises are ROADMAP C64.
- 2026-10-27: B6 live: the hosted API serves both level-2 values (earthquakes 2026-07-19 = 5,
  Denver high 2026-09-27 = 28.3 °C) and the site links them (owner's OK). The client check passed
  against the hosted API and the chain; the live site check passed (16 checks, 5 for B6).
- 2026-10-27: ROADMAP B6 rounds done: one earthquake day and one temperature day ran live on
  Base Sepolia (7 observer processes, 5 drawn, all agreed, receipts and values on chain, 29
  transactions). A local node served both and a client checked them on chain. The hosted API
  serves them after the owner's OK to redeploy; the data page links them.
- 2026-10-27: NS17 done: the API is live at https://onyx.cryptagraph.com/v1/ (owner's OK; health
  200 on Base Sepolia, 2 servable shards, payments off, Caddy admin closed, site checks 11/11).
- 2026-10-27: NS17 built: a new Base Sepolia deployment passed a full run (145 transactions; the
  validators' vote window was too short on the real network and is now longer). The API node,
  its install script and the site's web config are ready; installing them waits for the owner.
  The earthquake and temperature values are served by the API code and checked by a client (tests).
- 2026-10-26: NS8 done: AI agents can ask, pay (x402 offer) and check Onyx answers through an MCP
  server; `onyx-check` refuses a changed row, another row and a root that is not final on the
  agent's own node. Markets is one dashboard at `/markets` (live, 78 browser checks).
- 2026-10-26: PP9, PP10 (NS9) and PP11 done: live observers, prepaid vouchers end to end, the
  observers' value on receipts and a date on every dataset card. The site has the Onyx icon.
- 2026-10-25: NS7 dry run passed: the public snapshot (1419 files, no names, secrets or e-mail)
  passes check-all on its own. Left for the project owner: the GitHub organization name and the go.
- 2026-10-25: NS1 done; public site build passes (1156 browser checks). NS7: GitHub-only contacts
  written (SECURITY.md, CONTRIBUTING.md with DCO sign-off, CODE_OF_CONDUCT.md, label-removal
  "Contact"); cryptagraph.com and www redirect to onyx.cryptagraph.com.
- 2026-10-25: file created from the partner notes, the decision log (docs/TRACKER.md section 1),
  the product plan and the roadmap. The project owner asked for everything to be built toward it.
