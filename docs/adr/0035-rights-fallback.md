# ADR-0035: Rights fallback: ship without label-derived totals until label-set rights are cleared

## Status
Accepted, 2026-10-16 (branch `rights-fallback`, ROADMAP L14). Prepared, not switched on: the
default of both switches keeps today's behaviour. Launch-checklist gate LC-1 names the two ways
to pass it.

## Context
THREATS L8-5 and launch gate LC-1: both website builds ship company totals derived from the
entity label sets (OKX, Binance, BitMEX, Bitwise BITB): per-block and per-week flows, the
reserve history, the reserve cross-check figures, the run records' OKX figures. The paid API
sells an OKX reserve row. The public build already ships no label row itself (AUD-2-F1), but a
total computed from a list is still a use of that list. OKX's terms (ToS 8.1) and the examiner
terms bitbetf.com links restrict redistribution (docs/ops/data-sources.md section 4). Only
The project owner can clear the rights (write to OKX and Bitwise). If no answer has come by launch, the
launch must not wait on it and must not ship the totals.

What is not label-derived and stays: Bitcoin block data (headers, transaction and output
counts), mining-pool block shares (the MIT-licensed bitcoin-data/mining-pools list, chain data),
the U.S. Treasury and SEC EDGAR level-2 rounds, roots and commitments (hashes; nothing can be
read back from them), the label set's own hash and count, and the run records' transactions.

## Decision
1. **One switch name, two places: `ONYX_RIGHTS=cleared|fallback`.**
   - Website: `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh`. `fallback` without
     `ONYX_PUBLIC=1` is refused (exit 2): the private preview keeps every total, by the task's
     rule that nothing leaves the preview. Any value other than `cleared` or `fallback` is
     refused, as `ONYX_PUBLIC` is (THREATS L9A-6); check-all asserts both refusals.
   - Node: `ONYX_RIGHTS=fallback` in onyx-node's environment.
   - **Default `cleared` in both.** Today's public build and today's node are unchanged
     (the ROADMAP item says "prepare", and the default public build is a VERIFIED gate,
     LC-5). The safety comes from the checklist instead: LC-1 cannot be ticked without either
     the project owner's written clearance or a fallback build and node (section "Consequences"). A
     separate flag rather than "fallback whenever `ONYX_PUBLIC=1`" because the day the rights
     are cleared the public build must ship the totals with no code change, and because a
     second meaning hidden in `ONYX_PUBLIC` would be easy to miss in a deploy command.
2. **Website, fallback build.** `build-web.sh` empties the run and live rows in `demo.json`
   (company flow leaves), drops the per-block rows, the series rows, the label-set spread and
   the recount lines, marks it `rights: "fallback"`; after the app build it deletes
   `history/week/` and `history/reserves-week.json` and sets the run records' OKX figures to
   `null` (any `<amount> BTC` in a step line becomes "[company total held back]"). The app reads
   `VITE_ONYX_RIGHTS`, which Vite replaces with a literal, so the reserve figures in
   `src/lib/reserves.ts` are folded out of the bundle. Each page says plainly why where a total
   would be: a banner on every page ("Company totals are held back until data rights are
   cleared."), a held-back section on Data and Reserves, the home card 2, the Explorer and API
   notes, "Held back" on the Run page. The in-browser checks over company rows (run rows, live
   rows, row proof) give way to one over the mining-pool history (5 checks, was 7).
3. **Guard.** `public_build::a_fallback_build_carries_no_label_derived_totals` runs on every
   fallback build and fails it on: a refused path; any satoshi amount (7+ digits), BTC amount
   (8 decimals, grouped or not) or row leaf (as bytes) taken from the committed label-derived
   outputs (`history/week`, `reserves-week.json`, `fixtures/reserves/cross-checks.json`, the
   run records' OKX fields) and, when present, from the private export, in every encoding the
   public guard follows (hex, base64, escapes, gzip, zlib, Brotli); any JSON field that carries a
   company total (`net_sats`, `balance_sats`, `okx_btc`, ..., or an `entity` naming a label-set
   entity); a `demo.json` not marked `rights: "fallback"` or still carrying rows.
   `the_fallback_guard_refuses_label_derived_totals` plants each form and checks the real
   committed files are refused and the kept ones (pools, level 2, indexer read) pass.
4. **Node.** With `ONYX_RIGHTS=fallback`, every request for a label-derived shard (proofs,
   export, metrics, settlement) is refused with **403 `label_rights_withheld`** before any 402,
   so nothing is charged. A shard is label-derived when its binding pins a label set (non-zero
   `label_set_hash`), when `KNOWN_GATES` claims it (the reserve gate), or when its metric is an
   entity metric by name: three independent signals, so a misnamed catalog entry still fails
   closed. This removes the OKX reserve sale. Shards without a label set (mining pools) and
   level-2 observations are served as before. `/v1/health` reports the mode.
   403, not 451: 451 `restricted_data`/`forbidden_data` already means the *source's* registered
   rights record forbids it; this is the operator's own decision about a derived product, which a
   client should not read as a registry fact.

## Consequences
- Launch has two paths (LC-1): rights cleared in writing (then `cleared`, the default), or a
  fallback build published and the node run with `ONYX_RIGHTS=fallback`. The rollback table
  and LC-6 must name the mode of the build that is live.
- A fallback site shows fewer of Onyx's strongest demonstrations (the reserve match to the
  satoshi, the flow charts). Those stay in the private preview.
- The guard knows the totals that are committed or exported today. A new label-derived output
  must add its file to `fallback_totals` (the JSON field rule and the path rule still catch the
  obvious shapes); the reviewer of any new page data checks this list.
- Roots and shard ids of label-derived shards stay on the site and on chain. They are hashes;
  nothing of a total can be read back from them.
- Reversal: set `ONYX_RIGHTS=cleared` (or unset it) and rebuild; no data migration.

## Amendment 1 (2026-10-16, review L14-R1): no default for a public build or a testnet node
Decision 1's "default `cleared` in both" is replaced, for the two places that can go public:
- `ONYX_PUBLIC=1 scripts/build-web.sh` refuses (exit 2) when `ONYX_RIGHTS` is unset or empty;
  the private preview takes no value and keeps every total.
- onyx-node with `ONYX_ENV=testnet` refuses to start when `ONYX_RIGHTS` is unset or empty;
  `ONYX_ENV=dev` (a local chain) keeps `cleared`. The `--export-node` files of a testnet run
  say `ONYX_RIGHTS=fallback`.

Why: the rights are not cleared today, so `cleared` is a claim, and a default makes the claim
for an operator who never thought about it. The checklist (LC-1) only catches that before
launch; a deploy command copied later would ship the totals with exit 0. Requiring the word
keeps the original goal (the day rights clear, `ONYX_RIGHTS=cleared` and no code change) and
fails closed. Defaulting to `fallback` instead was rejected: it would silently change what
the verified public build (LC-5) ships and hide the choice the same way.

Also (THREATS L14-R1-1): Svelte keeps a dead branch's static text in the bundle, so the
fallback bundle carried the cleared Reserves page's prose with a label-derived share. The
build now marks Svelte's template calls pure (apps/web/vite.config.ts), and the guard's rule 4
refuses any figure next to a label-set company's name, whatever its value.
