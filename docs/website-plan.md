# Website plan

Tracker for the website (apps/web, published privately at
<private link>). ROADMAP.md Phase W points here.
The loop works one item at a time on its own website lane, ticks it here, adds a line
to the log, and republishes the private preview build.

Rules: real data only (no invented numbers); the public build (`ONYX_PUBLIC=1`) never ships
label rows or addresses; every change keeps both builds' browser tests green and adds tests
for what it changes; measure before and after (sizes, timings, contrast ratios) and record
the numbers here; check desktop and phone (390 px) screenshots before publishing.

Publishing the preview: copy `apps/web/build` to a staging folder, then rewrite the
`"/app/immutable/` paths in its `index.html` to `"./app/immutable/` before publishing. SvelteKit
writes absolute paths into the fallback page, and the artifact host serves the files under a
sub-path, so absolute paths load nothing and the page is blank (version 76, 2026-10-20; fixed in 77).

## Items

- [x] W1 Baseline audit: page list, bundle and data sizes per page, first-load bytes and
      time (local, throttled), Lighthouse-style scores if a free tool is available
      (e.g. `lighthouse` via npx), contrast ratios of the palette, keyboard walk-through;
      record numbers in docs/audit/website-baseline.md (2026-10-14, see log)
- [x] W2 Phone layout: every page at 360/390/430 px; fix wrapping (run picker, tables,
      charts, nav), no horizontal scroll, tap targets >= 44 px (2026-10-14, see log)
- [x] W3 Accessibility: WCAG AA contrast in both themes, visible focus, full keyboard use,
      landmarks and headings, alt text and aria labels for charts and icons, reduced motion
      (2026-10-14, see log; dark only by decision, so one theme)
- [x] W4 Speed: lazy-load heavy data (the 3.4 MB demo inputs, history week files) only on
      the pages that need them; split chunks; compress JSON where possible; target and
      record first-load size for the home page (2026-10-15, see log; home target <= 250 KiB, met)
      - [x] W9 follow-up (2026-10-17, docs/audit/website-final.md): Run CLS 0 -> 0.033 in all three
        builds (shifted node `div[data-testid=net-picker]`, the run picker) and private Labels CLS
        0 -> 0.014 (`div.setslot`). Fixed 2026-10-17 (branch web-perf-fix): Run's heading shows the
        run's date from its file name and the badges row keeps a pill's height, so the date and the
        status pill arriving no longer move the picker; Labels' decoding placeholder carries the
        panel heading, so the section opener (drawn once a section has an h2) no longer appears
        late and moves the slot 28 px. Layout-shift entries (PerformanceObserver in Playwright,
        412x823, unthrottled like Lighthouse's trace): Run 0.0325 / 0.0327 / 0.0325 -> none in the
        private / cleared / fallback builds; private Labels 0.0136 -> none. Lighthouse on the base
        commit 9462150, private build (3 of 3 runs, load 5.9-8.1): Run CLS 0.0325, Labels 0.0136.
        Review 2026-10-17: e2e now holds Run's latest file and Labels' input back and checks, in every
        build, that the picker and the slot do not move when they arrive (both fail on 9462150), that
        nothing reads as loaded meanwhile (no pill, no chain id), and that once a run loads its
        heading shows the date the file records, not the one in its name.
      - [ ] W9 follow-up, LCP: Data 3.1 -> 3.4 s (private, cleared; 3.2 s fallback) and public Labels
        2.8 -> 3.0 s. Traced 2026-10-17: both LCP elements (Data's `p.slabmeta` block range, public
        Labels' `p.muted` note) are drawn from data/demo.json, which was requested only after the
        page's scripts ran (149 ms Data, 425 ms Labels; fallback build, one unthrottled run). app.html now preloads
        it on the pages that read it at mount (Home, Data, Labels, Explorer, API; not Run, Reserves or
        the not-found page): requested at 16 ms, one request (fetch reuses the preload; e2e checks it). Lighthouse not re-measured as a median: one quiet moment gave one
        private run of Data (LCP 3,069 ms, load 8.21 -> 9.35; base 3,404 / 3,408 / 3,416 ms at load
        6.3-7.8), then the load stayed at 11-29 for 30 minutes. Re-measure Data and Labels LCP in
        all three builds (median of 3, load < 10).
      - [ ] Not new, still open from W1: public Explorer's throttled browser LCP 11.5-11.7 s in the
        cleared build (W1 11.9 s), 2.0 s in the fallback build, so the late element is likely a
        label-derived one (not traced).
- [x] W5 Home page clarity: what Onyx is in one sentence, three proof points with live
      links (live runs, reserves, verify in your browser), one clear next step; keep it honest
      (testnet only, private preview) (2026-10-15, see log; includes the home page's part of W10)
- [x] W6 Chart consistency: one chart style for Flows, Reserves, Pools, History (axes,
      units, colours, tooltips, empty and loading states), tested in both themes
      (2026-10-15, see log; dark only by the W3 decision, so one theme)
- [x] W7 Run page: add the 2026-10-14 run (ROADMAP A2b) and make the run picker scale to
      many runs (2026-10-14, see log)
- [x] W8 Copy pass: plain language, consistent terms (round, shard, root, proof level),
      no overclaims; every claim links to its proof or source
      First fix (the project owner 2026-10-15 asked "why does it say demo data?"): the Data page tags the
      Treasury and SEC values "demo data", but the numbers are real (each observer fetched
      them from fiscaldata.treasury.gov and data.sec.gov over HTTPS). Only the round was a
      rehearsal (observers simulated on one machine, local test chain). Replace the tag with
      words that say exactly that, e.g. "real value · test round", and match the paragraph text.
      - [x] Done 2026-10-15 (web-w6, see log): tag "real value · test round" on the Treasury
        and both SEC values; the copy names the source and the fetch date read from each
        round's window (2026-10-11 UTC) and says only the round was a local rehearsal.
      - [x] Copy pass done 2026-10-16 (web-w8, see log): every page's rendered text read in
        both builds; glued words fixed, terms aligned to GLOSSARY.md, claims linked.
- [x] W11 W6 review follow-ups (2026-10-15): pool tooltip on the last days overruns the chart
      (PoolShares.svelte tipTop); Reserves line lost its amber on a down week (direction only in
      numbers now); flow tooltip "Running" vs legend "Running net"; rename the `.demo` tag class;
      public_build.rs scan() misses addresses packed together in wasm data (split at hrp/base58)
      (2026-10-15, see log)
- [x] W10 Visual polish: typography scale and rhythm, spacing grid, card and table styling,
      hover/focus/press states, subtle motion (respecting reduced motion), empty/loading
      skeletons, consistent icons, a cohesive look on desktop and phone; before/after
      screenshots of every page recorded in the log (the project owner 2026-10-14: "keep improving the
      styling") (2026-10-15, see log; dark only by the W3 decision)
- [ ] W12 Continuous styling (standing item, never ticked; the project owner 2026-10-15: "pls continuously
      improve the website styling. i think it can still be much better"). Each round: screenshot
      every page at 1280 and 390 px, write a design critique (the 5 weakest things, ranked), fix
      them, compare before/after screenshots, log the round here. Aim higher than tidy: a clear
      visual identity, strong hierarchy, generous layout, polished data visuals and motion. Keep
      W2/W3 rules (phone, accessibility, contrast) and real data only. The website lane runs a
      W12 round whenever no other W item is running.
      - [x] W9 follow-up (2026-10-17, docs/audit/website-final.md): Reserves first visit grew
        2,025 -> 2,050 KiB (private, cleared): a sixth font face (IBM Plex Mono 500, +14.5 KiB) the
        page did not load at W1 and fallback Reserves does not load, plus JS +7 and CSS +2.7 KiB;
        find the label-derived text set in mono 500 and check whether it needs that weight.
        Checked 2026-10-17: it is each company's balance (`span.mono.big`, W12 round 2's card
        figure, not shown in the fallback build), the same weight as every figure tile on Home,
        Data and Run. Kept: with the 500 face blocked the browser draws it with the 400 face (no
        synthetic weight below 600), visibly lighter at 412 and 1280 px
        (docs/audit/website-final/reserves-mono500-compare.jpg). Not preloaded: below the fold
        (y 929 px at 412) and not the LCP element. Reserves wire (audit.mjs, cleared)
        2,099,892 B at 9462150, 2,100,409 B with this branch merged with ade3709.
      - [x] Explorer's Live blocks strip is still cut on the left at 360 px, with no scroll cue.
        Done in W12 round 9 (2026-10-17): with motion off the strip is a scrollable row, the first
        block unfaded and a fade only on an edge with blocks hidden past it; with motion on it still
        slides with both edges faded (each block passes through the middle).
- [x] W9 Final pass: re-run the W1 audit, compare numbers, publish (2026-10-17, see log;
      docs/audit/website-final.md)

## Meeting polish (2026-10-29, before the 20:00 UTC meeting)

The owner (2026-10-29 13:55 UTC): "work solely on the website improvements for the last 6-7 hours
... ensure its fully professional and everything is top notch ... ensure the styling is perfect
... do playwright tests opening the actual site". Work order: MP0 first, then MP1, then the audit's
findings by severity. Each item: build, browser tests, check-all, commit and push. The owner gave a
standing OK for Onyx deploys (2026-10-29 14:30 UTC: "u have my ok always for onyx deploys"): deploy
after a passing check-all, then re-test the live site; no change after 19:00 UTC.

| # | Item | Status | Result |
|---|---|---|---|
| MP0 | Live audit: Playwright on the live site, every page at 390, 768, 1280 and 1440 px: console errors, failed requests, sideways scroll, axe (WCAG A/AA), broken links, full-page screenshots; a visual review of every screenshot; findings become MP items | done 2026-10-29 | `apps/web/tests/live-site.mjs` on release `c7ed039`: 10 pages x 4 widths, 0 console errors, 0 failed requests, 0 sideways scroll, 0 axe violations, one h1 each; 287 links, the only failures are bot blocks (Basescan 403, usgs.gov 403) and two slow government hosts; the home tab title is wrong. Visual review of 221 screenshot slices by three reviewers: findings in MP2-MP10 |
| MP1 | Add data: a clear "Add data with AI or the API" button at the top of the page that opens the steps for an AI connector (MCP) and for the API (`/v1/submit/check`, `/v1/submit`, `/v1/submit/{id}`), each checked against the live API | done 2026-10-29 | `AddWithAgents.svelte`: an "Add data with AI or the API" button under the Add data title opens two tabs: the AI steps (`claude mcp add --transport http onyx https://onyx.cryptagraph.com/mcp`, ask, confirm, follow by receipt) and the API steps (proposal file, `/v1/submit/check`, `/v1/submit`, `/v1/submit/{id}`), each copyable; the check and status calls were run against the live API (the check routed the Treasury example to level 2 and previewed $40,582,274,254,843.57; an unknown receipt answers 404). The hosted connector now offers `onyx_propose_dataset` (`deploy/systemd/onyx-mcp.service`, THREATS NL21-2, docs/agents.md). e2e `tests/product.mjs` (MP1 lines: opens, focus, both tabs, fits, axe, sends nothing) |
| MP3 | Markets (tonight's demo): the live Base Sepolia section first, under the title, and the practice game after it; a clear first state before the chain is read (no row of empty cards); the RPC notes behind "Advanced"; practice stats tagged Simulated and rank "Unranked" before a first trade; one date format; one heading scale; Markets in the header | done 2026-10-29 | Live section first under the title, kept mounted while a practice market is open; "Practice markets" heading before the game; before a read each card shows its shape; the RPC field and its notes under "Use a different RPC"; buckets under 10 bets show the pool on one line (no empty bars and dashes) and the winning bucket says Won; rank "Unranked" before a first trade; vague close labels made phrases; the tabs scroll as one row on phones; the folded-section "+" keeps its space site-wide; Markets in the header (MP13) |
| MP4 | Home: tab title "Add your data" (should name Onyx); the markets block says "No trades are open" (false since NL6: market 1 settled, market 2 open) and its heading is far smaller than its twin; step 03 says submissions are not connected; one arrow style for internal links; no one-word last lines | done 2026-10-29 | Tab title "Onyx · Data anyone can check"; the markets block says what is live (market 1 settled, market 2 open, test insurance) at the same heading size as its twin (it used `--text-h3`, which is not a token); step 03 names the AI and API ways to send; internal links use one arrow |
| MP2 | Add data sends to the live API (`VITE_ONYX_SUBMIT_API=1` in the public build): the form returns a real receipt, "Already submitted data?" looks up a receipt; home step 03 and the page copy follow | done 2026-10-29 (owner: "Don't skip anything for after the meeting") | Runtime switch instead of a build flag (`src/lib/submit-api.svelte.ts`): the page reads the site's own `intake.json`; every build ships `{"submit": false}`, so the offline e2e (s1, U3 and others) stay as they were, and `scripts/deploy-site.sh` writes `{"submit": true}` into the hosted release (`ONYX_SITE_SUBMIT=0` keeps it offline). With it on, steps 3 and 4 check and send through the same origin's `/v1/submit/check` and `/v1/submit`, and "Already submitted data?" looks a receipt up. Checked on the dev server with the file switched on (the receipt field appears, one GET of `intake.json`, no request to `/v1` before the visitor asks) |
| MP5 | Internal words on public pages: "08 / ADD DATA" page numbers, M6/M7 milestone codes, the API page's run-log box and its verify paragraph, repo paths on Run, "[company total held back] = the scan" | done 2026-10-29 | Done inside MP6, MP8, MP9 and MP11 (page numbers in MP13); the OpenAPI description and the How it works note no longer say there is no public endpoint |
| MP6 | How it works: two stat tiles show only "–"; the Bitcoin chart fills half its card; "Ready" pills look like buttons | done 2026-10-29 | Stat tiles say "Loading…" (and why, if a value never comes) instead of "–"; a lone chart spans its card; M6/M7 codes gone; check status as a dot and a word; milestone labels top-aligned at 768 px |
| MP7 | Explorer: the Live blocks row cuts a card in half at 390 and 768 px | done 2026-10-29 | Below 860 px the Live blocks are a grid (2 per row on phones with "Show all 24 blocks", 4 at 768 px); one block label style "#970,543" |
| MP8 | Run: notes split mid-sentence across two columns at 1280 px; mixed number formats; "198 sent / 215 linked"; one line breaks badly at 390 px | done 2026-10-29 | One-column notes; plain words for the held-back total; repo paths and commands under "How this was built"; "sent by the run, 7 contract deploys included" and "linked below: 191 run steps, 20 gas returns, 4 payments"; minutes next to seconds, ETH rounded with the full value on hover; thousands separators; a quiet "Log" toggle; the 390 px event line stacked |
| MP9 | API: paid-route cards fill half the column; "GET" alone on a line at 390 px; "This description"; "Nobody hosts a public endpoint yet" | done 2026-10-29 | Served datasets as a table with the run log folded; a plain opening for the client check; no "M6 run"; route cards fill the row; method and path on one line; the status line names the hosted test node; groups named by data type |
| MP10 | Add data: the "Not asked" pill, a button styled like an input, the levels paragraph as a list, step tabs wrapping at 390 px | done 2026-10-29 | Pill "Not loaded"; the levels as a five-row list; step tabs 2 x 2 on phones; "Already submitted data?" points to the AI and API ways to check a receipt |
| MP11 | Browse data: the ETF table overlaps at 390 px and is cut at 768 px; shell commands, recipe footers and internal codes (ROADMAP, ADR, THREATS ids) in body copy; a "Checks out" badge on rehearsal-only sections; about 20 screens long (fold the secondary sections); sentences split across CSS columns; one formula shown raw | done 2026-10-29 | ETF table: "Change" label, stacked below 900 px; one "Reproduce it yourself" fold per section; the Ethereum recipe line once; internal codes gone ("planned"); the SEC clause dropped and 13 uncovered funds in one fold; neutral "Rehearsal"/"Example" badges; Estimates, Signed by the source and Track records folded; "Every kind of data" right after the dataset cards; no sentence split across columns; the estimate leads with "Average of N days, 95% range"; 2-decimal headline; no mid-token breaks; the Bitcoin chart's dots and orange bars explained |
| MP12 | Labels and Reserves: 29 blank test tiles before a run; the signed message unframed; status pills that look like disabled buttons; Reserves is one held-back notice under "Balance over the week" | done 2026-10-29 | 29 dashed idle tiles with "29 test vectors, not run yet"; the signed message framed; idle status as a dot and a word; Reserves is "Company reserves: waiting for data rights" with links to what is live |
| MP13 | Header: Markets missing from the top nav; no active state on some pages; the "08 / ADD DATA" page numbers clash with section numbers | done 2026-10-29 | Markets in the header (four primary destinations); the kicker above each title is the page name without its nav number |
| MP14 | Second live review (two reviewers on the deployed release): fix the safe rough edges before the 19:00 UTC freeze | done 2026-10-29 | Markets: one "press to load" hint instead of four repeated lines, record dates kept whole, "Test insurance" in the display face; Browse data: "Available via the paid API" set as a note, not in the big figure type; Add data: the receipt section reads as a pointer, not a field-less form; footer "Every check runs in your browser, on the network's own code." Checked, not a bug: How it works fills its two counts when they scroll into view (24 and 9,850 on the live site). The three items first left for later were done in MP16 |
| MP15 | The owner: "only Claude can be connected". The AI steps showed a Claude Code command first; show each common assistant's own setup on the same address | done 2026-10-29 | A picker in the AI tab: ChatGPT (default; Developer mode connector, no authentication), Claude app (custom connector), Claude Code (`claude mcp add`), Cursor (`mcpServers.url`), VS Code (`servers` with `type: http`), Gemini CLI (`httpUrl`; its `url` is SSE), Other (any Streamable HTTP client); shapes from each client's docs on 2026-10-29; docs/agents.md lists them; e2e `MP15` clicks each and checks its setup carries the hosted address |
| MP16 | The owner: "Don't skip anything for after the meeting": every remaining finding of both reviews | done 2026-10-29 | Browse data: plain recipe names (ids and hashes in the folds), one date style in prose ("4 Oct 2026"), status as dot and word, no lone separators on phones, one blocks table, 2-decimal summaries, the ETF fund column widened, no mid-token breaks, badges on their own line ("stated" in the tooltip and intro), one-line notes under folded sections, thousands separators. Run: step checks in plain words through a display-time mapping (data files unchanged; the exact check is the first line of the step's Log; Binance reads "refused (no independent cross-check)", as its log records), "39 min" finality, a complete hardware table. Explorer: arrows and a fade on the wide strip, one count and hash style. Labels: the route in a "For developers" fold. Reserves: an honest hero and no empty band. How it works: no milestone codes, "Sealed answers", plainer jargon and copy. API: "Internal test" labels, no error code in notes. Markets practice: one date style, the trade feed opens full. Home: no line-leading dash, step 03 says the site sends. One payment line on every page. Wanted data: no idle pill |
| MPZ | Deploy (standing OK), then re-run the MP0 audit on the live site | done 2026-10-29 | `2f19303` after check-all passed (the first run on `c442956` failed 4 layout checks: the Run "Log" tap target, the Markets tabs scrolling sideways on phones, the Labels vectors panel taller than the Brier panel at 1000 and 1080 px; fixed). API (`scripts/deploy-api.sh`, exit 0): the live connector lists `onyx_propose_dataset`, and a proposal checked through it without `confirm` came back level 2 with `recorded: false`. Site release `20261029T161657Z-2f19303`. `tests/live-site.mjs` on the live site: 10 pages x 4 widths clean (0 console errors, 0 failed requests, 0 sideways scroll, 0 axe violations); on screen: tab title "Onyx · Data anyone can check", header Browse data / Markets / How it works / Add data, the Add data panel's connect command, Markets read from the chain with market 1 Settled (winning bucket marked Won), market 2 Trading open, nothing refused |

## Log (newest first)

- 2026-10-29 Release `20261029T205321Z-4ffc135` (check-all passed on `4ffc135`): MP2, MP14 and MP16 live.
  The release's `intake.json` is `{"submit": true}`: on the live site the Add data form shows the
  receipt field, and its "Check with the API" step posted only to `/v1/submit/check` and previewed
  the Treasury example's value (nothing was sent for review). Live audit at 390 and 1280 px: all 10
  pages clean. The public repository github.com/onyxnetworkofficial/onyx (owner's request, 2026-10-29)
  was made from `scripts/public-snapshot.sh` (no names, secrets or label data; one neutral author,
  no AI co-author) and passed check-all on its own (1,797 Rust tests, 650 contract tests, every
  chain rehearsal); the snapshot fixes are merged here from the `public-release` branch.

- 2026-10-29 Release `20261029T172915Z-21b77fb`: MP14 second-review fixes and MP15 (connect the Onyx connector from ChatGPT, Claude, Claude Code, Cursor, VS Code, Gemini CLI or any MCP client). Live re-audit at 390, 768 and 1280 px: all 10 pages clean; on screen the picker shows each client's setup with the hosted address.

- 2026-10-29 Meeting polish (MP0-MPZ, owner: "fully professional ... styling perfect ... playwright tests opening the actual site"): a live audit and a visual review of every page, then fixes on all ten pages, the Add data AI/API panel and proposals in the hosted connector; release `20261029T161657Z-2f19303`, re-audited live (clean). MP2 (the form sending to the live API) parked until after the meeting.

- 2026-10-26 Markets is the dashboard (the owner: make the arena the Markets page, drop the
  repetitive explainer views). The old Markets page (illustrative market, empty positions,
  placeholder resolution, terms draft, futures simulator) is gone; Markets now opens on the arena,
  adds Create (list a Yes/No market on an Onyx dataset; it trades at once) and Verify data (the
  stake simulator, still checked by `tests/t10.mjs`). `#/arena` redirects to `#/markets`; the
  footer is back to 10 pages. `tests/markets.mjs` replaces `tests/arena.mjs`. The contract rules
  in `markets.ts` keep their shared vectors.
- 2026-10-26 Markets arena (`#/arena`), the project owner's ask for a working prediction-market
  prototype to show the partners ("everything should be working"): practice credits only, prices
  and other traders simulated in the browser, no network request. Yes/No and range markets priced
  by an LMSR market maker (`src/lib/arena.ts`), a live trade feed, buy ticket with the payout,
  cash-out, settlement replayed as the observer check (7 fetch, 5 drawn, 5 agree) with demo
  controls, 30-second flash rounds, parlays, portfolio with a daily practice limit, leaderboard,
  quests and badges. `tests/arena.mjs` (in the e2e) runs every part at 1280 and 390 px. The real
  rules stay in `markets.ts` and the testnet contracts; the backend comes later.
- 2026-10-18 W12 round 14 review (fresh reviewer; merged origin/main to f29b407 with C21/C23, round 13's review,
  the smoke lock and Phase P docs; doc conflicts only, both sides kept, duplicated TRACKER rows dropped). Diff
  b780d16..a14fccf: CSS, classes and test code; no sentence, number, data file or public-build guard changed.
  Priority, a real fade bug (round 13, on main): the fade showed a waiting section only when an IntersectionObserver
  saw it in view, and the observer samples drawn frames only, so a scroll that passes a section between two frames
  (a fling, a jump, or a machine too loaded to draw) left it at opacity 0; round 14's gate run 3 counted 1 dimmed on
  Home, and another branch at load 56 1 on Run. Reproduced with Chrome on one starved core (taskset, nice 19, busy
  loops): e2e's scroll-through left 1-3 Home sections at opacity 0 in 3 of 6 and 2 of 8 runs, each one a section no
  drawn frame showed. Fix (`src/lib/fade.ts`): a section is shown once reached, its top above the viewport's bottom
  edge (in view or scrolled past), checked on every scroll, resize and observer entry; one already reached at its
  first observation never fades. Sections still below keep waiting, so the fade is kept. After: 0 of 8 starved runs
  dimmed. w12 adds a check that does not need load: two scrolls in one task (no frame between) put a section in view
  and then past it; it must be shown, the sections below still waiting (Home and Run, 1,280 and 390 px; 4 fail on
  the pre-fix build). e2e's scroll-through now ends at the bottom with a drawn frame before going back up (as a
  reader's does; the bottom sections were otherwise never drawn in view at all), not paced per step, so it still
  exercises skipped frames. Round 13's review print check had moved after a scroll to the last section, where with
  this fix nothing waits; it now runs before that scroll and requires waiting sections.
  Second load race: e2e's "running the checks moves focus to the checks" read "after the checks ended" at load 56.
  Cause in the page, not the test: `checkNow` moved focus a frame later (for the command palette) but started the
  checks at once, and with no frame drawn the worker finished first. The checks now start in that frame, right after
  the focus move (`running` set at the click). e2e adds a check with frames held 8 s after the click: focus must land
  with the checks running and none finished (1 finished on the pre-fix build). Third: Labels' vectors-versus-Brier
  checks at 1,000 px (and round 14's Brier form checks, whose wait swallowed its timeout) now wait for the Brier
  scores at every width (a 236 px Brier panel at load 42 meant no scores yet).
  Review of the round: renders of Home, Labels and Data at 375, 761, 1,000, 1,059, 1,280 and 1,600 px (private;
  fallback below): no sideways scroll, nothing past the edge, none of the round's elements clipped, focus rings on
  the first 30 Tab stops, Home's check rows 94 px on a phone and 78 px from 761 px, pipeline notes two lines at 761 px,
  Brier labels one line beside their sliders. Findings, all fixed: (1) Low: the empty track panel's new `.ptitle`
  let a 60-letter word run out of its 375 px panel; `.ptitle` gets `overflow-wrap: break-word` (min-content width
  unchanged, so no row that sizes to its titles moves); (2) Low: round 8's panel-title check, "counted apart, not
  loosened", was looser on fallback Data (it used to require no panel title there; it then allowed any title off a
  check card); it now allows only check cards' titles and the empty track panel's one, on both builds. The round's
  own checks failed 10 times on its pre-round build and pass after (its log). w12 on the merged tree's private
  build: 387 ok, 0 failed. Gate: `ONYX_WEB_PUBLIC=1 scripts/check-all.sh` exit 0 on the merged tree, first run (e2e cleared
  919 ok, fallback 798 ok, private 938 ok, ALL CHECKS PASSED), then check-docs and check-secrets after this entry.
  Fallback renders (the gate's fallback build) at the same widths: no finding. apps/web/build is the private build. Not checked: Safari, a real phone, a real screen reader.

- 2026-10-18 W12 round 14 (branch web-style-14 from 44277f4, round 13 under review; merged origin/web-style-12 with
  its review and origin/main 06cb6d5 and 2d8981b, conflicts only in this log and docs/TRACKER.md, both sides kept;
  private preview build). CSS, layout and markup only: no sentence, number or honesty tag changed, no new label, no
  public-build guard touched, the fade (round 13) left alone. Looked at Home, Data and Labels at 375, 761, 1,000, 1,280
  and 1,600 px before and after (reduced motion, tests/shots.mjs), the other four pages at the same widths before, and
  probed each of round 13's ideas with injected CSS before editing. Changes: (1) Home's seven check names take `.ititle`
  (display face, `--text-lg`, 400) like the pipeline's roles above them and Run's steps (they were medium 15 px sans);
  on a phone they step down to `--text-md` so a two-line row stays under round 2's 100 px (94 px; 104 px at 22 px);
  (2) Home's pipeline at 761-999 px is a flex row: each role's width follows its one-line text (basis its max-content,
  shrinking by it), beams keep 40 px; every note now two lines ("Rerun a sample; cheaters lose their bond" was three),
  neighbouring notes 48 px or more apart; from 1,000 px the even grid stays. Tried first: notes spreading into the
  half-beams (the Validators and Chain notes came 10 px apart at 761 px) and 24 px beams (no gain); (3) Labels' Brier
  panel from 1,059 px (where the vectors run 10 a row) is a form: each slider's label beside its slider in an 11rem
  column the score names share, so the slider tracks and the score bars start on one edge; the panel's natural
  height fell 404 to 340 px, so the vectors panel (355-359 px) sets the row and its button sits one gap under the last
  result (the band: 61 to 16 px at 1,100-1,600, 65 to 16 at 1,059); the Brier panel's spare 15-19 px goes between its
  slider rows; Labels 2,218 to 2,172 px at 1,280; (4) Data's track-record headlines ("Agreed 31 of 34 resolved
  (91.1%)") take `.ititle` (they were mono 18 px sentences under the display-face "Example"), the source ids stay in
  mono; the empty panel's "No real track records yet." takes `.ptitle` (it was bold 15 px sans). Page heights at 1,280
  px: Home 5,183 to 5,194, Data 11,013 to 11,020; at 375 Home 7,910 to 7,916, Data 16,669 to 16,639. No sideways
  scroll on Home, Data or Labels at 375, 761, 1,000, 1,059, 1,280 and 1,600 px (scrollWidth); the Brier sliders keep
  their 2 px focus ring. Before / after in docs/audit/website-style-14/ (7 *-compare.jpg, 404 KB; before = the merged
  tree without this round's changes). tests/w12.mjs gains the round-14 checks: the check names in their heading's
  face at 400 and at least 1.3 x their fold (Home, 1280 and 390); the track headlines an item title's size and the
  empty panel's title a panel title's (bare references), the ids in mono (Data, 1280 and 390); the five notes at most
  two lines and at least half a beam apart (761, 861, 999); at 1,059 and 1,280 px the vectors' button one panel gap
  under the last result, the Brier scores one gap above their footer, each label beside its slider and sliders and
  bars on one edge. Round 8's panel-title check counts Data's check-card titles apart (`.panel.check`), since the
  track panel now has one on the fallback build too (still none expected on fallback check cards, at least one on
  the others). On the pre-round build the new checks fail 10 times (367 ok); on this one w12.mjs passes 377 ok
  standalone (346 ok on the fallback build).
  Gates: `ONYX_WEB_PUBLIC=1 scripts/check-all.sh` failed three times, then passed: run 1 exit 1 (fallback build: round
  8's panel-title check expected no `.ptitle` at all on fallback Data, and the track panel's title is one; it now counts
  check-card titles apart, fixed above); run 2 exit 1 (cleared build: e2e's first `header.top` wait hit 30 s at load
  average 33, 20% IO wait, before any check ran); run 3 exit 1 (fallback build: round 13's "every section fully shown
  after a scroll through it" counted 1 dimmed section on Home before axe's second pass); run 4 exit 0 (ALL CHECKS
  PASSED; e2e cleared 907 ok, fallback 786 ok, private 926 ok, 0 failed), then check-docs and check-secrets after this
  entry went in. Run 3 is not this round's code (no fade, section or script changed; Home's sections are the same):
  8 repeats of that scroll on Home at 1,280 x 800 with motion on, 60 ms steps and frame-paced steps, left 0 sections
  dimmed; decided at strike 3 to rerun unchanged and leave the fade and its test to round 13's review (logged in
  docs/TRACKER.md). apps/web/build is the private build.
  Not checked: Safari, a real phone, a screen reader, motion-on renders (no motion changed); Run, Reserves, Explorer
  and API after (not touched).
  Ideas for round 15: (a) Explorer's "Commit, then reveal" panel: two small cards in the left third of a full-width
  panel; (b) Labels at 861-1,058 px still leaves 21 px under the vectors at 861 px; the Brier form could start lower
  if its label column fits; (c) Home's pipeline at 761-999 px: the uneven columns move the beams off centre between
  icons (7-27 px); beams drawn icon to icon would even it; (d) the API's "Where each dataset has been served"
  panel: bold mono dataset names inline in long paragraphs; (e) the fade could start a little earlier (a bottom root
  margin), once round 13's review has judged it.

- 2026-10-18 W12 round 13 review (fresh reviewer; merged origin/web-style-12 to 56c56ed first, since round 12's
  review fixes were not in round 13, then origin/main 06cb6d5 with D22; doc conflicts only, both sides kept). Diff
  2fea9c9..44277f4: CSS, classes, `src/lib/fade.ts` and test code; no sentence, number or guard changed. Probed the fade
  on the private build with motion on: every page loads with no section in view dimmed; a back navigation from deep on
  Run, a reload at depth on Labels, resizes to 375 and 1,600 px, `window.find` of a waiting section's heading and a Tab
  into it each leave none in view dimmed; waiting sections stay in the accessibility tree; print media shows all; by
  the code, JS off or no IntersectionObserver never sets `data-fade` (read, not run); CLS on Run through a scroll 0.0003 (paused 0.0012); no DOM mutation
  at rest on any page, so the MutationObserver's scan costs nothing there. Renders at 375, 761, 1,000, 1,280 and 1,600 px
  on all 7 pages, private and fallback, motion on after a scroll: no sideways scroll, nothing past the edge, nothing
  clipped in the round's elements, focus rings on the first 25 Tab stops; element views of Home's pipeline (761, 1,000,
  1,600), Data's Example heading (375, 1,280) and Run's gantt (375, 1,280) read as intended. Findings, both fixed:
  (1) Low: with the fade, axe read the sections below the first screen only with every fold open, so a closed fold's
  own text there (Run's "show N transactions" summary) was no longer measured; e2e adds an axe pass scoped to the
  sections that faded in, folds closed, between the as-loaded and the folds-open passes; (2) Low: the print claim had
  no check; w12 adds one (still-waiting sections show in print) and compares the fade's duration with `--dur-slow`
  rather than a pinned 0.32 s. The round's 11 new checks fail with the round's source reversed (357 ok, 11 failed) and
  pass after. Gate: `ONYX_WEB_PUBLIC=1 scripts/check-all.sh` 0 on the merged tree (cleared 898 ok, fallback 777 ok,
  private 917 ok, ALL CHECKS PASSED), then check-docs and check-secrets after this entry. apps/web/build is the private
  build. Not checked: Safari, a real phone, a real screen reader, the browser's own Ctrl+F bar (window.find stood in).
  Seen, not changed: with IntersectionObserver deleted, Run hangs on the pre-round build as on this one (not the fade).

- 2026-10-18 W12 round 13 (branch web-style-13 from 2fea9c9, round 12 under review; private preview build). CSS,
  layout and markup only, plus one small script for the fade: no sentence, number or honesty tag changed, no new label,
  no public-build guard touched. Looked at all 7 pages at 375, 761, 1,000, 1,280 and 1,600 px before (reduced motion,
  tests/shots.mjs) and probed each of round 12's ideas. Changes: (1) Data's track records "Example" heading (the last
  bold 15 px sans heading on the site, under a 46 px serif h2) takes `.ptitle` (display face, a panel title's size:
  28 px at 1,280, 22 px at 375); its "example · invented data" tag stays in the body face; (2) Home's pipeline notes
  take their column's width (the 18ch cap set them in 140 px of a 166 px column) with balanced lines: "Rerun a sample;
  cheaters lose their bond" two lines instead of three at 1,000-1,600 px (Home 5,203 to 5,183 px at 1,280); at 761-950
  px the 102-121 px columns still give it three (16 px beams would only fix it from 861 px and hide the beams; kept);
  (3) Run's gantt step names at `--text-base` (15 px) over their 13 px detail (both were 13 px, told apart by weight
  alone, beside 22 px step titles); body face kept, the rows are a chart (round 12); Run 7,124 to 7,151 px at 1,280,
  11,738 to 11,803 at 375; (4) sections below the first screen fade in as they come into view (`src/lib/fade.ts`, an
  IntersectionObserver started by the layout; `[data-fade]` in app.css): each section's first observation decides,
  so one on the first screen at load is never dimmed; one first seen off screen waits at opacity 0 and fades in over
  `--dur-slow` (320 ms) once any of it is in view; sections a page adds later are watched as they appear; screen and
  motion on only: reduced motion, the pause button (`data-motion="off"`) and print show every section at once. axe
  treats opacity 0 as hidden, not low contrast, so the motion-on e2e axe pass now runs its second pass (folds open)
  after a scroll through the page and first checks that no section is left waiting or part-way through its fade
  (7 pages x 2 widths, 0 dimmed); the first pass, as loaded, is unchanged. Not done: idea (b), Labels' 45 px above
  the vector button at 1,100-1,280 px: 29 results make 3 rows of 10 (35 px squares, width-bound) or 4 rows of 9 or 8
  (40-47 px), which overshoot the Brier panel by 22-68 px and move the band there; a non-square or spread grid reads
  worse than the band. Decided after three layouts failed (logged in docs/TRACKER.md). No sideways scroll on any page at the five
  widths (scrollWidth, 35 of 35). Before / after in docs/audit/website-style-13/ (5 *-compare.jpg plus
  labels-fade-1280-after.jpg, a motion-on sequence: load, 60 ms after a scroll, 760 ms after; 232 KB; before =
  2fea9c9). tests/w12.mjs gains the round-13 checks: the Example heading in its section heading's face at 400 and a
  bare `.ptitle`'s size, its tag in the body face (Data, 1280 and 390); the five pipeline notes at most two lines
  (1,000, 1,280, 1,600); every gantt name at least 1.1 x its detail in the body face (Run, 1280 and 390); on Run at
  1280 and 390 px with motion on, the first-screen sections fully shown at load, every section below waiting at
  opacity 0 and the last fading in (320 ms) once scrolled to; with reduced motion and with the pause button every
  section shown at once. On the pre-round build they fail 11 times (350 ok); on this one w12.mjs passes 361 ok
  standalone (330 ok on the fallback build). Gates: `ONYX_WEB_PUBLIC=1 scripts/check-all.sh` failed twice on my own
  new or touched checks, then passed: run 1 exit 1 (cleared build: Labels' footer count, 2 of 3; the check ran before
  the verifier drew the Brier scores at load average 30-57; it now waits for them like Run's step lists); run 2 exit 1
  (fallback build: the Example check's reference `.ptitle` does not exist on fallback Data; it now measures a bare
  `.ptitle`); run 3 exit 0 (ALL CHECKS PASSED; e2e cleared 885 ok, fallback 764 ok, private 904 ok, 0 failed), then
  check-docs and check-secrets after this entry went in. Merged origin/web-style-12 (nothing new) and origin/main
  97dd5b4 (the round-11 merge, no content change) before the gates. apps/web/build is the private build.
  Not checked: Safari, a real phone, a screen reader; the fade on a slow phone and with a back-navigation scroll
  restore (a section restored into view might fade once).
  Ideas for round 14: (a) Home's "Run the checks" check names (bold 15 px sans, sentences) beside the display-face
  titles; (b) Home's pipeline at 761-950 px: the longest note still three lines; (c) Labels at 1,100-1,280 px, the
  45 px band (needs a layout idea, not a grid count); (d) Data's track-record cards: the mono 18 px headline beside a
  display-face sub-heading; (e) the fade could start a little earlier (a bottom root margin) so a fast scroll never
  shows an empty band.

- 2026-10-18 W12 round 12 review (fresh reviewer; merged main 97dd5b4, no tree change, then d24867c with C20, no
  conflict). Diff e6e3f3f..2fea9c9: CSS, classes and test code only; no sentence, number or guard changed (build-web.sh,
  the public-build tests and the held-back panels untouched). Probed Home, Run, Labels and Data at 375, 561, 761, 861,
  1,000, 1,100, 1,280 and 1,600 px in the private and the fallback builds (scrollWidth, elements past the edge, title
  overflow and line counts, Run's facts, the vector grid against the Brier panel, Data's heading, status and tiles) and
  viewed element renders: no sideways scroll, facts one line each (fallback's "Held back" too), Data's heading one line
  from 1,100 px with the status centred on the tiles, the fallback's block table under the chart. Swept Labels at
  841-1,281 px with 20, 29 and 40 results. Findings, both fixed: (1) Low: from 1,059 px the Brier note fits on two
  lines and that panel is 20 px shorter, so 8 a row (46-47 px squares) made the vector panel up to 50 px taller and
  left the empty band under the Brier scores; 8 a row now ends at 1,058 px, 10 a row from there as from 1,100 px;
  (2) Low: `.ititle` had no wrap rule, so a 50-letter word in a step title (the run file's text) ran out of its card
  and scrolled a 375 px page 97-125 px sideways; it now breaks (`overflow-wrap: anywhere`); the probe's numbers at the
  eight widths are unchanged by both fixes. Accepted as is: at 861 px the vector grid leaves 21 px (8 a row, squares
  34 px); with 20 results the band returns (45-67 px) and with 40 the vector panel is the taller one (the grid is
  tuned to the 29 vectors the export ships). The round's 10 new checks fail on the pre-round source and pass on
  2fea9c9; none pins a pixel value (gaps, line ratios, faces). New checks: the vector panel at most 1.5 gaps taller
  than the Brier panel's content at 1,000 and 1,080 px; a 50-letter word in each kind of item title stays in its card
  at 375 px (Home, Run); both fail on 2fea9c9. Gates: two runs failed on the machine (Chrome
  `net::ERR_NETWORK_CHANGED`; a 30 s Playwright wait at load 58); at strike 3 the two table waits in e2e.mjs take
  wait.mjs's 240 s cap; `ONYX_WEB_PUBLIC=1 scripts/check-all.sh` 0 on the merged tree (cleared 861 ok, fallback 740
  ok, private 880 ok, ALL CHECKS PASSED), then check-docs and check-secrets after this entry. apps/web/build is the
  private build. Not checked: Safari, a real phone, a screen reader, motion-on renders.

- 2026-10-18 W12 round 12 (branch web-style-12 from e6e3f3f, round 11 reviewed and merged with main; private
  preview build). CSS, layout and markup only: no sentence, number or honesty tag changed, no new label, no
  public-build guard touched. Looked at all 7 pages at 375, 761, 1,000, 1,280 and 1,600 px before (reduced
  motion, tests/shots.mjs, which now takes `WIDTHS=`) and probed each idea. Ranked: (1) item titles in bold or
  medium sans beside serif headings and card titles: Home's five roles, Run's 14 steps, 2 indexer rounds and 3
  earning cards; (2) Run's "Client checks" value on two lines beside three one-line facts at 761, 1,000, 1,280
  and 1,600 px; (3) Labels' test-vector panel: beside the taller Brier panel the spare height (45 px at 1,280,
  77 px at 1,000) sat under the run button, 61 / 93 px above the footer; (4) Data's first heading broke after
  "block" in a 248 px column at 1,280 and 1,600 px. Changes: (1) one rule for list-item titles: `.ititle` in
  app.css (display face, `--text-lg`, 400) on Home's pipeline titles, Run's step and round titles and its three
  earning cards; the gantt rows, company names and seal names stay in the body face (data labels); (2) Run's
  round facts (`.facts.fit`, from 561 px) size to their content and share the spare width, one line each; at
  761 px the fourth takes its own row; phones keep the grouped card; (3) Labels' results grid takes the spare
  height under its title, so the button sits one gap above the footer like Explorer's paired panels; at 861-1,099
  px the results run 8 a row (four rows of larger squares fill the panel; 10 a row left 93 px empty at 1,000);
  from 1,100 px 10 a row stays (8 would make the panel 50 px taller than the Brier one and move the empty band
  there); (4) from 1,100 px Data's heading spans the section on one line and its status sits beside the four tiles
  under it, centred on their row (the header box is `display: contents`); the block chart starts 25 px lower (581
  to 606 px at 1,280). Not done: idea (5), a fade on the section rise: the rise is scroll-driven
  (`animation-range: entry 0% entry 40%`), so a section whose top is on the first screen would stay partly
  transparent until the reader scrolls, and the motion-on axe pass in e2e would measure that dimmed text (logged
  in docs/TRACKER.md). Page heights at 1,280 px: Home 5,201 to 5,203, Data 10,844 to 10,869, Run 7,125 to 7,124,
  Labels the same; at 375 px Run 11,665 to 11,738 (larger step titles), Home 7,901 to 7,910. No sideways scroll on
  any page at the five widths (scrollWidth measured, 35 of 35). Before / after in
  docs/audit/website-style-12/*-compare.jpg (5 files, 428 KB; before = e6e3f3f). tests/w12.mjs gains the round-12
  checks: item titles in their section heading's face at 400 and at least 1.3 x their note (Home, Run, 1280 and
  390), the gantt labels not; Run's round facts one line each at 761, 1,000 and 1,280 px; Labels' button one panel
  gap above the footer and the results one gap under the title (1,280), filling the panel (1,000); Data's heading
  one line with the status beside the tiles, centred (1,280). Round 10's Data check now measures the chart against
  the tiles (one section gap under them) instead of a pinned 600 px. On the pre-round source the new checks fail
  10 times (330 ok); on this one w12.mjs passes 340 ok standalone.
  Gates: `ONYX_WEB_PUBLIC=1 scripts/check-all.sh` 0 (ALL CHECKS PASSED; e2e cleared 850 ok, fallback 729 ok,
  private 869 ok, 0 failed; no run needed a rerun), then check-docs and check-secrets after this entry went in.
  apps/web/build is the private build (the gate's last).
  Not checked: Safari, a real phone, a screen reader; motion-on renders were not viewed (no motion changed).
  Ideas for round 13: (a) Data's TrackRecords "Example" sub-heading (h3, bold sans 15 px) beside serif headings;
  (b) Labels at 1,100-1,280 px: the vector panel still has about 45 px spare above its button; (c) Run's gantt
  labels at 13 px beside 22 px step titles on the same page; (d) a fade for sections could start only below the
  first screen if the reveal moved to an IntersectionObserver (code, not CSS); (e) Home's pipeline notes are
  capped at 18ch and wrap to three lines at 1,000-1,280 px.

- 2026-10-18 W12 round 11 review (fresh reviewer; merged main 4296f2c with D3, no conflict). Diff 75f10ce..770d904: CSS,
  classes and test code only; no sentence, number or guard changed (the Reserves edits add `ptitle` and `r` classes). Probed
  all 7 pages at 375, 600, 761, 1,000, 1,100, 1,280 and 1,600 px (scrollWidth, elements past the edge, clipped text,
  element shots viewed): no sideways scroll anywhere. Found and fixed: (1) Medium: global `.seg` also styled PoolShares'
  bar segments (padding, border, pill corners; bars no longer to scale), now `.seg[role='group']`; (2) Low: Explorer's
  checks wrapped at two widths at 761-910 px, now stacked at the select's width under a 420 px row (container query,
  replacing the 560 px media query); (3) Low: a phone's stacked last row kept its rule in a panel (on the `tr`, which
  the round's rule and check missed). w12.mjs: pool bars to scale (Data, 1280 and 390), Explorer at 800 (stacked) and
  1,000 px (one line), the last-row check includes the row, Run's step panels compared with a bare `.panel`. Not a
  finding: the site is dark-only (W3), so there is no second theme to check; touch keeps the site-wide sticky hover.
  Gates: `ONYX_WEB_PUBLIC=1 scripts/check-all.sh` 0 (cleared 840 ok, fallback 719 ok, private 859 ok); the first run
  failed 2 of my own new checks (reference panel was a compact one), fixed and rerun. apps/web/build is the private build.

- 2026-10-18 W12 round 11 (branch web-style-11 from 75f10ce; private preview build). CSS, layout and
  markup only: no sentence, number or honesty tag changed, and no new label. Critique of all 7 pages at
  1280, 1024 and 390 px, ranked: (1) Run's run switch stopped 47 px short of the right edge at 1,280 px,
  and every segmented switch (Run's runs; Data's company, scale, range and day switches) took the filled
  button's hover: a transparent option rose 1 px with a cream glow under it; (2) Reserves' company names
  and Home's three proof-card titles in bold sans under or beside serif titles and numbers; (3) Labels'
  attestation buttons wrapped to two rows of different widths at 761-1,000 px and on a phone; (4)
  Explorer's row proof: "Change 1 sat, then check" wrapped alone under the select and the first check;
  (5) Run's step panels (`.netbox` 8 / 16 / 16 px, `.ganttbox` 12 / 16 / 16 px), the last panel padding
  exceptions; (6) micro-details: the Reserves comparison's "Theirs" and "Onyx" figures left-aligned (the
  decimals did not line up), a second hairline under the last row of every table in a panel (16-26 px
  above the footer's or fold's own), counts in pills, switches and buttons in proportional figures.
  Changes: (1) one segmented switch in app.css (`.seg`, `.chipbtn`; the copies in Run and Data are gone):
  hover lightens an option in place (ink text on `--hover`, no lift, no glow), the chosen one brightens,
  press dims; from 1,100 px Run's switch spans its column to the section's edge, the runs sharing the
  spare width on one line each; the API's request/response tabs lighten on hover too; (2) Reserves' company
  names take `.ptitle` and Home's proof titles the display face at `--text-xl` (400, tight); (3) Labels'
  two buttons stack at one width in the right column at 761-1,000 px and full width on a phone (at
  1,001 px and up they share a row as before); (4) Explorer's select takes its own row, both checks side
  by side under it (stacked at its width on a phone); (5) Run's step panels take the panel's 24 px (16 px
  sides on a phone, where 24 px made the 14 steps 294 px taller), the first step's ring and the first
  gantt row 24 px down; (6) the two figure columns align right; no rule under a panel table's last row;
  tabular figures on buttons, switches, chips, pills, fact and key/value lists, legends, figure tiles,
  logs and labels (Pill.svelte too; running text keeps proportional figures). Checked and already right:
  every icon 16, 18 or 22 px in one stroke (w10), one 2 px focus ring on every Tab stop (a11y), hover on
  every control (w10; my probe of 242 controls flagged only the two scroll regions, the hidden skip link
  and three links that break across two text columns, which it could not hover at their centre),
  every numeric table column right-aligned except block and height keys. Page heights at 1,280 px: Home
  5,186 to 5,201, Run 7,077 to 7,125, Reserves 3,831 to 3,839, Labels 2,219 to 2,218, the rest the same; at
  390 px Run 11,302 to 11,350. Before / after in docs/audit/website-style-11/*-compare.jpg (6 files, 540 KB;
  before = 75f10ce). Checked: all 7 pages at 1280, 1024 and 390 px before and after (reduced motion),
  viewed; the switch hover and the Labels panel at 900 px viewed; no sideways scroll on any page at 360,
  390, 640 (1,280 at 200% zoom), 800, 1,000, 1,100 and 1,280 px (scrollWidth measured). tests/w12.mjs
  gains the round-11 checks, fallback-aware (Run: the switch ends within 1 px of the section's edge, each
  run on one line, an option lightens on hover with no transform or shadow; every step panel 24 px (24 /
  16 on a phone) with its first row 24 px down; Reserves (not fallback) and Home: card titles in the
  display face at 400 and at least 22 px; Explorer (not fallback): select alone, checks side by side or
  stacked at its width; Reserves (not fallback): the two figure columns right-aligned, one right edge
  each; every page: tabular figures on those controls, no rule under a panel table's last row; Labels at
  900 and 390 px: the buttons stacked at one width, in the right column at 900, 44 px tall on a phone).
  On the pre-round build the new checks fail 30 times (296 ok); on this one w12.mjs passes 326 ok
  standalone. W10's hover check caught the chosen option's missing hover state (fixed before the gates).
  Gates (run one after another): `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` 0 (717 ok, 0 failed),
  `ONYX_RIGHTS=cleared ONYX_PUBLIC=1 scripts/build-web.sh` 0 (834 ok, 0 failed), `ONYX_WEB=1 scripts/check-all.sh` 0
  (ALL CHECKS PASSED, 853 ok in its private build), check-docs 0, check-secrets 0 (both run after this entry went in;
  no run needed a rerun), then the final private build `ONYX_PUBLIC=0 scripts/build-web.sh`.
  Not checked: Safari, a real phone, a screen reader.
  Ideas for round 12: (a) Home's "Who does what" pipeline titles (Blocks, Miners, ...) and Run's step
  titles are still bold sans; decide one rule for list-item titles; (b) Run's "Client checks" fact wraps
  to two lines beside three one-line mono facts; (c) the Labels vector grid leaves the panel's lower third
  empty beside the Brier sliders; (d) Data's first heading breaks onto two lines beside the tiles at 1,280
  px; (e) a section's rise could fade in as well (opacity) for a softer transition, motion on only.

- 2026-10-17 W12 round 10 review (fresh reviewer, after merging main 13cc595 with C10 and the
  open-source prep; no conflict, the licence check and its e2e test kept). 404 page: one 64 / 48 px
  slab like the inner pages (slab 434 px tall at 1,280, 479 at 390; the space under it is `main`'s
  existing min-height, not the round), buttons wrap to two rows at 390, no sideways scroll; viewed.
  Beside-the-heading layouts: no overlap and no sideways scroll at 1,280, 1,100, 1,099, 1,024, 390
  and 200% zoom of 1,280 and 1,100 (`.wrap` caps content at 1,040 px, so 1,100 lays out as 1,280);
  at 1,099 Run's badges and picker and Data's tiles drop under the heading; DOM order unchanged
  (h2, badges, three run buttons, select); the run buttons take Tab in order, Enter and Space pick
  a run (heading date follows), 2 px focus ring. With reduced motion and with `data-motion="off"`
  no element on 8 pages has a non-zero transition (the global `transition: none !important` covers
  `--dur-slow`). Explorer: `label[for=scenario]` still names the select; select and buttons 44 px
  at 390. Source diff: no sentence, number or honesty tag changed; the fallback build shows no
  company figure (Reserves' held-back notice, Data's tiles are block data). Fixed: once the
  validator wrote its log, the rerun button (pinned to its panel's foot) dropped 61 px below
  Validate at 1,280; with a log shown the paired panels now share title, controls and log rows
  through a subgrid, so both buttons stay on one line (w12.mjs clicks Validate and checks it).
  Gates: fallback public build 0 (677 ok), cleared public build 0 (791 ok), `ONYX_WEB=1
  scripts/check-all.sh` 0 (ALL CHECKS PASSED, 810 ok), check-docs 0, check-secrets 0, private
  build 0 last (810 ok); no run needed a rerun. Not checked: Safari, a real phone, a screen reader.

- 2026-10-17 W12 round 10 (branch web-style-10 from 4f2b7f0; private preview build). CSS, layout and
  markup only: no sentence, number or honesty tag changed, and no new label. Critique of all 7 pages at
  1280 and 390 px, ranked: (1) Explorer's rerun panel stood half empty beside the validator's two rows
  (select, then its button on a second line); (2) the API's slab card ran 203 px tall (fact line on three
  lines, the three perks one a row), 87 px above the title beside it; (3) Home's proof levels on a phone
  read in a 224 px column beside the number and icon (every line wrapped; 919 px of rungs); (4) Run's
  first screen gave the run picker a row of its own under the heading; (5) Data's first screen gave the
  four figure tiles a row of their own, the block chart from 618 px; (6) Labels' attestation: the seals
  alone in an empty right half, the buttons under the signed text at the left. Changes: (1) the paired
  Explorer panels end on their controls (each control row at its panel's foot, the two buttons on one
  line), the validator's label over its select like a form field, the select and Validate on one row;
  (2) the API's side card takes a 520 px column (`.slabside.wide`): fact line on two lines, perks two a
  row, card 151 px, slab 53 px shorter; (3) on a phone each proof level is a small card: number, icon and
  name on the top row, the "how", the example tag and the status across the full width (rungs 919 to 852
  px, Home 7,922 to 7,830 px at 390); (4) from 1,100 px Run's heading stands at the left and its badges
  and the picker form a column beside it (`.runhead`, only with up to three runs; the picker's left edge
  is set by the heading, so a chip growing as its run loads moves nothing to its left): tiles 573 to
  519 px; (5) from 1,100 px Data's heading and status sit at the left and the four tiles beside them
  (`.statlead`, a 768 px column so each label stays on one line): chart 618 to 581 px; (6) from 761 px
  Labels' attestation is two even columns, the signed text left and the seals with both buttons under
  them right (panel 382 to 324 px). Consistency pass (one spacing, radius, shadow and motion scale; the
  rendered values were already all tokens, so this is about which token each role takes): one-off values
  removed: `--sp-11` (80 px) and `--sp-12` (96 px), used only by the 404 page's taller slab (OnyxSlab's
  `compact` switch is gone, every page opens on the 64 / 48 px slab); panel padding pairs used once:
  Data's chart panel 20/16/16, Run's indexer checks 12/16, Home's pipeline 32/16 (now the dense 16 px or
  the panel's 24 px); inner-card padding 16 (Home's market result) and 16/24 (Labels' set card), now
  12/16 like the block and seal cards; the Reserves API facts, the one card in a panel drawn as a
  top-level card (18 px, gradient edge, drop shadow), now an inner card (12 px, hairline, no shadow);
  the pipeline's 56 px icon tiles, 18 px with a drop shadow inside a panel, now 12 px without; Home's
  slab note, the slab's one 12 px card, now 18 px like the drawing and the side cards; seven one-off
  transition durations (0.15, 0.18, 0.2, 0.25, 0.3, 0.4 s, most with plain ease) now `--dur` (160 ms,
  hover) or the new `--dur-slow` (320 ms, a status change) with `--ease`. Kept on purpose: Run's step
  panels' 8 and 12 px top padding (their first row carries its own 16 px, so the content starts 24 px
  down like every panel). Page heights at 1,280 px: Home 5,202 to 5,186, Data 10,819 to 10,774, Run 7,123
  to 7,077, Explorer 2,106 to 2,083, Labels 2,276 to 2,219, API 2,179 to 2,127, Reserves the same. Before /
  after in docs/audit/website-style-10/*-compare.jpg (7 files, 460 KB; before = 4f2b7f0). Checked: all 7
  pages at 1280 and 390 px before and after (reduced motion), viewed; no sideways scroll and no overlap of
  the new pairs at 360, 390, 640 (1,280 at 200% zoom), 1,000, 1,100 and 1,280 px (scrollWidth measured);
  Explorer's pair at 1,000 px (one row), Labels at 800 px (the buttons wrap in the right column). A card
  audit (every bordered box: radius, padding, nesting) before and after at 1280 and 390 px.
  tests/w12.mjs gains the round-10 checks, fallback-aware (Explorer, private: both buttons on one line,
  the label over the select, the select beside Validate; API: fact line on two lines, perks two a row, the
  card at most 40 px above the title block; Home: at 390 every rung's name on its number's row and the
  rest full width, at most 880 px of rungs, at 1280 the columns kept; Run: badges and picker in a column
  beside the heading at 1280, under it at 390; Data: four tiles beside the heading and status on one row,
  chart above 600 px, at 1280; Labels: both buttons on one row under the seals beside the text at 1280,
  under the text at 390; every page: each card in a panel 12 px with no shadow, each card on the slab 18
  px; with motion on every transition 160 or 320 ms; one slab padding on every page, the 404 page's too,
  and no `--sp-11`/`--sp-12`). On the pre-round build the new checks fail 13 times (271 ok); on this one
  w12.mjs passes 284 ok standalone.
  Gates (run one after another): `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` 0 (676 ok, 0 failed),
  `ONYX_RIGHTS=cleared ONYX_PUBLIC=1 scripts/build-web.sh` 0 (790 ok, 0 failed), `ONYX_WEB=1 scripts/check-all.sh` 0
  (ALL CHECKS PASSED, 808 ok in its private build), check-docs 0, check-secrets 0 (load average 9-40; no run needed a
  rerun); after this entry's numbers went in, check-docs and check-secrets again, then the final private build below.
  Not checked: Safari, a real phone, a screen reader.
  Ideas for round 11: (a) Run's picker column ends 47 px short of the right edge at 1,280 px; reserve the
  chips' loaded width so it can sit flush right without moving; (b) Reserves' company cards title in bold
  15 px sans beside display-face panel titles; (c) Labels at 761-1,000 px: the attestation's two buttons
  wrap in the right column; (d) Run's step panels (`.netbox`, `.ganttbox`) could take the panel padding
  with their first row's own top padding removed, the last panel padding exceptions; (e) Home's proof
  cards still title in bold sans under display-face numbers.

- 2026-10-17 W12 round 9 review (fresh reviewer, after merging main 16c453d with C12; no conflict,
  no web change from main). Explorer strip with motion off: a focusable region named "Live blocks"
  (tabindex 0, reached by Tab), 24 list items in the one visible copy, a 2 px accent ring on its
  wrapper (seen at 1,280 px); ArrowRight steps one block (162 px), End reaches the last; ArrowDown
  still scrolls the page and unfocused Home leaves the strip alone. With motion on: no tabindex or
  role, two copies, the slide animation and the named list as before; the pause button turns the
  row on and off. Fixed in Marquee.svelte: (1) the key handler took Alt/Ctrl/Meta/Shift + arrow or
  Home/End too (Alt+Left is the browser's Back); modified keys now stay the browser's; (2) the fade
  classes only re-read on the row's own resize, so a block arriving with the row at its end left the
  right fade off; the observer now watches the track too; (3) a row scrolled while paused kept its
  offset when motion came back on; it returns to the start. Checked: body text of all 7 pages
  identical to the pre-round build (private, reduced motion); slab cards beside the title at 1,280
  and 1,024 px (no squeeze), under it at 390 and at 200% zoom of 1,280 and 1,024 px; no sideways
  scroll on any page at those five widths; proof-level examples are text spans (muted ink on a
  bordered tag), a live level's status says "Live" beside its ring. Gates: fallback public build 0,
  cleared public build 0, `ONYX_WEB=1 scripts/check-all.sh` 1 (w4.mjs nav click timeout at load
  average 34) then 0 on the rerun, check-docs 0, check-secrets 0, private build 0. Main moved again
  (1e1beb2, C50 onyx-tlock; no web, script or demo file): merged, then check-all 0, check-docs 0,
  check-secrets 0, private build 0 last (public builds not rerun: nothing they read changed).
  Not checked: Safari, a real phone, a screen reader.

- 2026-10-17 W12 round 9 (branch web-style-9 from b1edadc; private preview build). CSS, layout and
  markup only: no sentence, number or honesty tag changed, and no new label. Critique of all 7 pages
  at 1280 and 390 px, ranked: (1) access bug: with motion off (reduced motion, or the header's pause
  button) Explorer's live-block strip stood still with `overflow: hidden`, so the blocks past the sixth
  (1,280 px; past the second at 390) could not be seen at all, and the first block sat under the
  strip's left fade; (2) the inner pages' first screens: the lede or fact lines stacked under the title
  left the slab's right half empty and pushed the first section down; (3) Home's proof levels still
  titled in bold 15 px sans, the examples a thin grey line in the middle column; (4) Home's "What stops
  cheating" second row: the labels tile a third (title on two lines) beside a two-thirds bond tile
  with its lower half empty; (5) Explorer's rerun and validator check: one button each in a full-width
  card. Changes: (1) Marquee.svelte: when `motion.off`, the strip is one row that scrolls sideways
  (`overflow-x: auto`, snapping to whole blocks, thin scrollbar), the loop's second copy is dropped,
  the row is a focusable region named "Live blocks" (arrow keys step one block, Home and End go to
  either end), and an edge fades only while blocks are hidden past it (classes set from the scroll
  position); the focus ring sits on a wrapper so the fade cannot mask it; with motion on nothing
  changed. (2) Data, Reserves, Run, Explorer, Labels and API: the slab's ledes and fact lines are one
  `.slabside` block after the h1 (DOM order kept); from 961 px it is a glass card in a 420 px right
  column whose foot sits on the title's foot, the kicker kept on the title (app.css); under the title on
  phones as before. Slab bottom at 1,280 px: Data 361 to 325, Reserves 437 to 398, Run 401 to 334,
  Explorer 361 to 325, Labels 409 to 370, API 397 to 412 (its card holds the fact line and three
  perks). (3) Each proof level's name in the display face at the panel-title size, its examples a
  bordered tag, its status opening on a ring (filled for a live level; the words say "Live" too).
  (4) The bento is six columns: row one 4 + 2 as before, row two 3 + 3, the labels tile's seals beside
  their sentence, both tiles one height. (5) From 961 px the rerun and validator steps sit side by side
  (each with its own rail icon) until a rerun draws its company bars, then both go full width again.
  Page heights at 1,280 px: Home 5,233 to 5,202, Explorer 2,234 to 2,106, Run 7,190 to 7,123. Before/after
  in docs/audit/website-style-9/*-compare.jpg (7 files, 329 KB; before = b1edadc). Checked: all 7 pages
  at 1280 and 390 px before and after (reduced motion), viewed; Explorer and Reserves at 1,000 px, Home at
  640 px (1,280 at 200% zoom), Explorer at 360 px viewed; no sideways scroll on any page at 360, 390,
  640, 1,000 or 1,280 px (scrollWidth measured). tests/w12.mjs gains the round-9 checks, fallback-aware
  (Explorer, motion off: the strip `overflow-x: auto`, a focusable region, every block drawn once, the
  first unfaded and a right fade; ArrowRight moves it and End brings the last block fully into view with
  the fade moved left; Home: five proof-level titles in the display face with a bordered example tag,
  the labels and bond tiles one size side by side with a one-line title at 1,280 px and stacked at 390;
  Explorer (preview): rerun and validator panels side by side, one height, at 1,280 and stacked at 390;
  every inner page: the slab's side block a card beside the title with its foot within 24 px of the
  title's at 1,280 and under the title at 390). On the pre-round build the new checks fail 22 times
  (235 ok); on this one w12.mjs passes 257 ok standalone.
  Gates (run one after another, finished by the lead): `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` 0, `ONYX_RIGHTS=cleared ONYX_PUBLIC=1 scripts/build-web.sh` 0, `ONYX_WEB=1 scripts/check-all.sh` 0 (ALL CHECKS PASSED), check-docs 0, check-secrets 0; final private build below.
  Not checked: Safari, a real phone, a screen reader.
  Ideas for round 10: (a) Explorer's rerun panel stays half empty beside the validator's two-row
  controls; give the validator's select and button one row at 961-1,280 px or the rerun panel its
  fuel line placeholder slot; (b) the API's slab card runs one line taller than its title (fact line
  plus three perks), so its first section starts 15 px lower than before; set the perks two a row;
  (c) Home's proof levels on a phone run 82 px taller with the larger titles; tighten the phone rung;
  (d) Run's first screen still opens on the run picker and four tiles under a long section title;
  (e) Data's first screen: the block chart still starts at 620 px; the four tiles could sit beside the
  section heading on a wide screen.

- 2026-10-17 W12 round 8 review (fresh reviewer, after merging main 91994bd with the W9 follow-ups
  and C11; no conflict). The round-8 checks run against main's own web sources (round 8 left out,
  new tests in): private build exit 1, exactly the 10 round-8 checks failed (Home drawing 347 px,
  0 display-face panel titles on Home and Data at both widths, no all-blocks panel at both widths,
  Labels' set card not in the right half, vectors 15 a row of 22 px; phone title 42 px) and nothing
  else, so the checks bite. Note: Home's first proof card text was already on the first screen
  there (872 px of 900); the drawing's width is what fails. Found and fixed: (1) the BIP-322 squares
  showed pass and fail by green and red only (sighted readers; the words were screen-reader only), now
  35 px squares that a colour-blind reader could not tell apart: each result now draws a tick (match)
  or a cross (differs) as a CSS mask, no glyph from a font and nothing added to the accessibility tree;
  a square not run stays empty; e2e checks all 29 ticked and named "variant body: result, matches",
  and that a differing result draws the cross; (2) the drawing's recipe hash was still 12 px (the
  global `.chip`): now 13 px like the rest of it. Checked: no sentence, number or tag changed (the
  table's markup moved byte for byte); the table sits after "Do the blocks add up?" at the end of
  Company flows, under the block chart in the fallback build, whose net column stays dashes; motion
  stays behind prefers-reduced-motion and the tilt behind motion.off. Viewed with reduced motion at
  1280, 1024, 700 and 390 px and 640 px at 2x (1,280 at 200%): no overlap, hashes on one line, no
  sideways scroll; vector squares 35 px at 1280, 33 at 1024, 22 at 390. Gates (load average 10-20):
  fallback public build exit 0 (630 ok), cleared public build exit 0 (744 ok) (both before the 12 to
  13 px chip fix, CSS only), `ONYX_WEB=1 scripts/check-all.sh` exit 0 (759 ok),
  `scripts/check-secrets.sh` 0, `scripts/check-docs.sh` 0, final `ONYX_PUBLIC=0 scripts/build-web.sh` exit 0 (759
  ok, 0 failed). No run needed a rerun. Not checked: Safari, a real phone, a screen reader.

- 2026-10-17 W12 round 8 (branch web-style-8 from eea242d; private preview build). CSS, layout and
  markup only: no sentence, number or honesty tag changed, and no new label. Critique of all 7 pages
  at 1280 and 390 px, ranked: (1) Home's first screen: the drawing beside the 80 px title was a 346 px
  card of 12 px text, and the proof cards' text started below the fold at 1280 x 900; on a phone the
  title was 42 px with 70 px of empty slab above it; (2) panel titles mixed the display serif
  (Explorer, Labels) with bold 15 px sans (Home's "What stops cheating" tiles, Data's three check
  cards); (3) Data's "All 24 blocks as a table" fold floated alone between the data map and the track
  records, far from the blocks it lists; (4) Labels' set panel left its right half empty beside the
  donut; (5) the BIP-322 card was half empty above its footer beside the taller Brier card.
  Changes: Home's hero gives the drawing a `clamp(300px, 34vw, 400px)` column and starts 40 px higher;
  the lede reads in a 58ch measure (3 lines, was 4); the drawing (HeroVisual) is set at 13 px with
  taller blocks, 40 px links and a 76 px seal (less padding and a smaller step between 861 and 1100 px,
  each hash on one line); the first proof card's text now starts at 862 px. `--text-hero` grows with a
  phone's width (`clamp(40px, max(min(11.6vw, 46px), 6.2vw), 80px)`: 45 px at 390, 42 at 360, 80 at
  1,290+), the phone hero 24 px higher. A shared `.ptitle` (app.css) sets a panel's own title in the
  display face at the panel heading's size: Home's four tiles (icon on the first baseline) and Data's
  three check cards (markup keeps its span / b, not a heading). Data's all-blocks table is a panel
  (`.tablefold`, its 55 px top row opens it) at the end of the company flows section, or of the block
  chart's section when a rights-fallback build holds the company flows back (one Svelte snippet,
  rendered in one place per build). Labels: from 641 px the set's name, count and hash are a card in
  the right half beside the donut (the donut's height, so the measured slot holds); from 861 px the 29
  vector results are ten a row of 35 px squares (22 px on a phone). Page heights at 1,280 px: Home 5,192
  to 5,232, Data 10,870 to 10,855, Labels 2,315 (same). Before/after in
  docs/audit/website-style-8/*-compare.jpg (6 files, 260 KB; before = eea242d). Checked: all 7 pages at
  1280 and 390 px before, Home, Data and Labels after (reduced motion), viewed; Home measured at 320-1440
  px (title 2 lines from 360 px, no sideways scroll; 640 px = 1,280 at 200% zoom: title 46 px, 2 lines).
  tests/w12.mjs gains the round-8 checks, fallback-aware (Home: a 380-400 px drawing beside a two-line
  title and the first proof card's text on the first screen at 1,280 px, a 44-48 px two-line title and no
  drawing at 390; every `.ptitle` in the panel heading's face, size and weight, Home 4, Data 3 or 0 in
  the fallback build, no bold-sans tile or check title left; one all-blocks panel, its top row at least
  44 px and the panel's width, last in the section it belongs to; Labels' set card in the right half
  (private) and the vector squares ten a row at 30 px or more). Not run against the pre-round build.
  Gates (load average 11-34): `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` exit 0 (621 ok,
  0 failed); `ONYX_RIGHTS=cleared ONYX_PUBLIC=1 scripts/build-web.sh` exit 0 (735 ok, 0 failed);
  `ONYX_WEB=1 scripts/check-all.sh` exit 0 (748 ok, 0 failed); `scripts/check-docs.sh` 0;
  `scripts/check-secrets.sh` 0; final `ONYX_PUBLIC=0 scripts/build-web.sh` exit 0 (748 ok, 0 failed). No run needed a
  rerun.
  Not checked: Safari, a real phone, a screen reader.
  Ideas for round 9: (a) Explorer's live-block strip: with motion off it is a still strip whose first
  block is faded and whose blocks past the sixth cannot be reached (make it a scrollable row with a
  right-edge cue when still); (b) Home's proof ladder rows still title in bold sans, and its five rows
  leave the middle column thin at 1,280 px; (c) Home's "Labels signed by the company" tile wraps its
  title beside a half-empty "Wrong answers" tile; balance the bento's second row; (d) Explorer's "Rerun
  in this browser" and "Validator check" panels are one button each in a full-width card; pair them
  side by side; (e) the inner pages' first screens at 1280 x 900, as Home's this round (Data's
  chart starts near the fold under its title, tiles and section opener).

- 2026-10-17 W12 round 7 review (fresh reviewer, after merging main twice: 2048fb9 with C4, then
  e845d40 with C6's track records on Data and Home and the lead's gas-snapshot fix; no conflict, no
  round-7 rule touches C6's markup). No sentence, number or honesty tag changed (diff read; the
  Explorer and Labels notes keep their text, only `muted small` became `fine` in a `.pfoot`). Step
  numbers: the rehearsal's rings were CSS counter content with empty alt text, so a screen reader
  never read the number a sighted reader sees; the `ol` does not make up for it (with `list-style:
  none` Safari drops its list role, and the step table beside it has no number column). Fixed the
  round-6 way: each step's number is its own `.n` span in the shared ring (the look is unchanged);
  w12.mjs now checks the ring's text is its position and not aria-hidden. Also fixed (round 6, seen
  here): Reserves' week line, a 1fr grid of two nowrap figures, ran "−11,464.15992583 BTC" into "at the
  scan" in the 353 px cards at 761 px; the figures are now a wrapping flex row (equal columns when both
  fit, the second under the first when not); measured clear at 320-1280 px. Footer at 761-1440 px:
  no overlap between wordmark, list and state line, no link wider than its cell, the same footer
  markup (no new landmark); desktop links are text height (19 px), as before the round, phones keep
  44 px. Two-paragraph `.duo` at 640-1440 px (640 = 1,280 px at 200% zoom; type is px, so page zoom is
  the case): no paragraph overflows or leaves its box, the pairs end on the same line from 900 px.
  Gates: (load average 15-23) `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` 0
  (611 ok); `ONYX_RIGHTS=cleared ONYX_PUBLIC=1` 0 (725 ok); `ONYX_WEB=1 scripts/check-all.sh` 0 (736 ok;
  before the second merge it failed only on the two stale OnyxBudgetShort gas values that main's
  a47ad0d corrects); `scripts/check-docs.sh` 0; `scripts/check-secrets.sh` 0; final `ONYX_PUBLIC=0
  scripts/build-web.sh` 0 (736 ok). Seen at 1280, 900 and 390 px in the private build.
  Not checked: Safari, a real phone, a screen reader.

- 2026-10-17 W12 round 7 (branch web-style-7 from ade3709; private preview build). CSS, layout and
  markup only: no sentence, number or honesty tag changed, and no new label. Critique of all 7 pages
  at 1280 and 390 px, ranked: (1) the site footer set its page list at the far right, 500 px from the
  wordmark at 1,280 px, with the state line under both; (2) two-paragraph `.duo` blocks gave each
  paragraph half the width whatever its length, so Run's run notes ended 3 lines beside 5 and Data's
  history ledes 5 beside 4; (3) panel footers kept a 68-character measure in full-width panels: the
  API's one-sentence footer broke onto a second line, and the long ones (Data's pool note, Run's
  deploy, indexer and fee-split notes, Reserves' comparison note) ran 4-6 lines beside an empty half;
  (4) Explorer's input and live-block notes and Labels' three panel notes still floated as small
  print; (5) Run's local-rehearsal steps had no numbers or rail, unlike the Base Sepolia steps above
  them; (6) in Labels' two side-by-side panels, the vectors note floated mid-card beside the Brier
  note at the foot of the taller card. Changes: the footer (+layout.svelte) puts the list and the
  state line in one column beside the wordmark from 761 px, the list four a row (source order kept;
  phone unchanged); `.duo` with two paragraphs (app.css) is a flex row whose columns take their share
  of the text (flex shrinks each from its one-line length), so both end on about the same line (Run's
  notes 4 and 4 lines, Data's ledes 5 and 5), the rule the second paragraph's edge (Chrome 152 also
  paints `column-rule` in a flex gap, so it is set to none there); `.pfoot > .fine` reads up to 88ch
  (the API footer is one line), and a long footer takes `.pfoot.duo` (two columns from 900 px; two
  paragraphs side by side as above); Explorer's and Labels' notes are `.pfoot` rows (`.fine`); in a
  `.section.two` pair a footer sits at its panel's foot (flex column, `margin-top: auto`), so both
  footers line up; the rehearsal's honest and dispute steps get the Base Sepolia ring and rail (a CSS
  counter in a 28 px ring, `content: counter() / ""`, so the number stays out of the spoken text, the
  ol already gives the position; the bars keep one block axis, the axis labels moved with them).
  `text-wrap: pretty` on `.duo` and footer paragraphs. Page heights at 1,280 px: Home 5,265 to 5,192,
  Data 9,834 to 9,709, Run 7,329 to 7,190, Reserves 3,983 to 3,869, Labels 2,354 to 2,315, API 2,243
  to 2,149, Explorer 2,311 to 2,234. Before/after in docs/audit/website-style-7/*-compare.jpg (11
  files, 791 KB; before = ade3709). Checked: all 7 pages at 1280 and 390 px before and after (reduced
  motion), viewed. tests/w12.mjs gains the round-7 checks, fallback-aware (every `.duo` two columns
  with one rule at 1,280 px and one column at 390, two paragraphs ending within 1.5 lines; Data 6
  `.duo` (5 in the fallback build), Run 3 and Data 1 long `.pfoot.duo`; the API footer one line;
  Labels' paired footers at one height; the footer list beside the wordmark within 80 px, four a row,
  the state line under it (stacked, two a row, on a phone); every rehearsal step a 28 px counter ring
  on a 1 px rail with its name after it; footers counted on Explorer and Labels). On the pre-round
  build the new checks fail 8 times and a throwing wait stops at Labels' paired footers; on this one
  w12.mjs passes 223 ok standalone.
  Gates (load average 24-34): `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` exit 0 (601 ok,
  0 failed); `ONYX_RIGHTS=cleared ONYX_PUBLIC=1 scripts/build-web.sh` exit 0 (715 ok);
  `ONYX_WEB=1 scripts/check-all.sh` exit 0 (its web gate 726 ok, 0 failed); `scripts/check-docs.sh` 0;
  `scripts/check-secrets.sh` 0; final `ONYX_PUBLIC=0 scripts/build-web.sh` exit 0 (726 ok, 0 failed).
  No run needed a rerun.
  Not checked: Safari (gap decorations differ: the flex `.duo` rule is a border, so it does not
  depend on them), a real phone, a screen reader.
  Ideas for round 8: (a) Home's first screen: the hero visual is a small card beside the title and
  the proof cards' text starts below the fold at 1280 x 900; (b) Data's "All 24 blocks as a table"
  fold floats alone at the page's end; (c) panel headings mix display serif (Explorer, Labels) and
  bold sans (Home's "What stops cheating", Data's check cards); (d) Labels' "Label set" panel leaves
  its right third empty beside the donut and the set's name; (e) the BIP-322 panel is now mostly
  empty above its footer beside the taller Brier card.

- 2026-10-17 W12 round 6 review (fresh reviewer, after merging main f7c060e with C2, C5 and the W9
  report; no website source changed there; the only conflict was this log, both entries kept). No
  sentence, number or honesty tag changed (diff read; each week line's text is the same expressions,
  "·" included, now not drawn). Week line: the change keeps its ▲ + / ▼ − (colour is not the only
  cue), its colour is the chart's (--btc down, --line-net up), 9.0:1 and 7.7:1 on the card, the
  labels 5.8:1; the change is the existing last-minus-first of the chart's points, no new figure.
  Fallback build: Reserves held back (no cards, no week line), Explorer's live blocks render no
  company columns (liveRows stays empty) and say "Headers only". Run: DOM order title, check,
  transactions, log; the rail is a pseudo-element, the ring is the number's own span, so a screen
  reader still reads "1". Seen at 1280 and 390 px in the fallback and private builds. Fixed:
  w12.mjs measured the first `.flows` on Explorer, which is the rerun's once the recipe is rerun;
  the live blocks' list now has `data-testid="live-flows"` and the test reads that; a Reserves
  comment said the dot is drawn where the figures share a line (it never is). Gates (load average
  6-32): `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` 0 (576 ok); `ONYX_RIGHTS=cleared
  ONYX_PUBLIC=1` 0 (690 ok), and 0 (690 ok) again after the fix; `ONYX_WEB=1 scripts/check-all.sh` 0 (701
  ok; before the fix); `scripts/check-docs.sh` 0; `scripts/check-secrets.sh` 0; final
  `ONYX_PUBLIC=0 scripts/build-web.sh` 0 (701 ok, after the fix). Not checked: Safari, a real phone, a
  screen reader.

- 2026-10-17 W12 round 6 (branch web-style-6 from afcbf60; private preview build). CSS, layout and
  markup only: no sentence, number or honesty tag changed, and no new label. Critique of all 7 pages
  at 1280 and 390 px, ranked: (1) Run's 14 steps ran 2,310 px, every step a title, a check, a
  full-width transaction fold and a log fold stacked, the step numbers small and grey; (2) the note
  bars under Run's and Reserves' rows of cards hugged a 68-character note, the right half of the row
  empty under them; (3) Reserves' "Balance over the week" lede and the "Served through the API"
  intro were 4-6 lines beside an empty half, and each card's mono week line wrapped to two lines at
  1,280 px; (4) Explorer's row-proof tree sat centred in two thirds of its panel with the controls at
  the far right, and the live blocks' three companies stacked full-width bar pairs 380 px deep;
  (5) the API's "Your node checks the root" left half the width empty; (6) Home's testnet note box
  ran 70 px past its text; (7) Run's two indexer ledes stacked 8 lines. Changes: Run's steps read
  across from 900 px (`.netsteps.across`: what happened and its check on the left, transactions and
  log on the right, in that source order) with each number in a ring on a rail (`.netsteps`, so
  the indexer events too); the list 2,309 to 1,697 px at 1,280 px, unchanged on a phone (3,560 px;
  the badge column keeps the old width there). `.notebar` (app.css) spans the cards above it; a long note takes `.notebar.duo` (two
  columns from 900 px; was Data's SEC-only rule, now shared): Run's run notes, Reserves' check note.
  `.duo` gains one-column paragraph spacing; used by Reserves' lede and API intro and Run's indexer
  ledes. Each reserve card's week line is two labelled figures ("Week:" over the change in the
  chart's direction colour, "at the scan (#…):" over the total; the same text, the dot between them
  hidden, as on a phone each figure is a row, label left, value right). `.section.side` (app.css):
  a one-paragraph section sets its heading beside the text from 900 px (API). Explorer: the tree at
  the panel's left, its controls and result beside it under a hairline; the live blocks' and the
  rerun's companies a column each (each pair keeps its own scale, as before). Home's note is a grid
  that hugs its two lines. Page heights at 1,280 px: Run 8,103 to 7,329 px, Reserves 4,117 to
  3,983, Explorer 2,550 to 2,311, API 2,319 to 2,243. Before/after in
  docs/audit/website-style-6/*-compare.jpg (10 files, 790 KB; before = afcbf60). Checked:
  all 7 pages at 1280 and 390 px before the change and Home, Reserves, Run, Explorer and API after
  it (reduced motion), viewed. tests/w12.mjs gains the round-6 checks, fallback-aware (Reserves:
  lede, API intro and note bar in two columns at 1,280 px and one at 390, the bar as wide as the
  cards, each week line two figures side by side or one a row with each value on one line; API:
  heading beside / over its paragraph; Home: the note box ends within 6 px of its text at 1,280 px;
  Run: every step's number a 28 px ring on a 1 px rail, check left and transactions right on a wide
  screen and under on a phone, left first in the DOM, the run note bar as wide as the tiles in two
  columns; Explorer: the tree at the panel's left with its controls beside it (over them on a
  phone), the live blocks' companies a column each). On the pre-round build they fail 6 times and
  a throwing wait stops at the live-block columns; on this one w12.mjs passes 198 ok standalone.
  Gates (load average 7-20): `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` exit 0
  (576 ok, 0 failed); `ONYX_RIGHTS=cleared ONYX_PUBLIC=1 scripts/build-web.sh` exit 0 (690 ok);
  `ONYX_WEB=1 scripts/check-all.sh` exit 0 on the first run (its web gate 701 ok, 0 failed);
  `scripts/check-docs.sh` 0; `scripts/check-secrets.sh` 0; final `ONYX_PUBLIC=0
  scripts/build-web.sh` exit 0 (701 ok, 0 failed). No run needed a rerun.
  Not checked: Safari, a real phone, a screen reader.
  Ideas for round 7: (a) Explorer's "Live blocks" note and the Row proof's result line still float
  as small print (a `.pfoot` each); (b) the API's "served so far" footer and Data's level-2 notes
  leave half a row empty; (c) the footer's wordmark and page list leave a 500 px gap at 1,280 px;
  (d) Run's local-rehearsal step bars could share the rail look of the Base Sepolia steps; (e) the
  `.duo` columns balance unevenly on short ledes (3 lines beside 2); `text-wrap: balance` or a
  measured break could even them.

- 2026-10-17 W9 final pass: W1 re-run with the same commands on three builds of bf50858 (private,
  public cleared, public fallback; Chrome 152.0.7977.82, Lighthouse 12.8.2 median of 3, every
  measurement started at 1-minute load < 10) in docs/audit/website-final.md. First visit on the
  wire, W1 -> now: Home 3,214.6 -> 221.5 KiB private, 2,067.7 -> 218.7 cleared / 212.4 fallback;
  API 2,025 -> 189-194 KiB; Explorer and Labels (private) still 3.1 MiB (3,179-3,181 KiB;
  verifier + recipe input); fallback Reserves 2,025 -> 171 KiB. Lighthouse perf 73-94 -> 88-94
  (private), 69-94 -> 89-96 (public); Data TBT 1,290 -> 66 ms public; private Labels LCP 8.7 -> 2.9 s;
  A11y/BP/SEO 100 in all 63 runs. Keyboard and phone: skip link everywhere, Run 56 Tab stops,
  0 targets < 24 px, 0 text < 12 px; contrast 132 pairs, 0 below AA. Regressions (small, all
  within Lighthouse's "good"): Run CLS 0 -> 0.033 (run picker), private Labels CLS 0 -> 0.014,
  Data LCP 3.1 -> 3.4 s, public Labels LCP 2.8 -> 3.0 s, Reserves +24 KiB (a sixth font face);
  follow-ups under W4 and W12. Measured only; no site change.

- 2026-10-17 W12 round 5 review (fresh reviewer, after merging main 4c58ad1 with B2/B3's observer
  keys; no website file changed, no conflicts). No sentence, number or honesty tag changed (removed
  and added words of the diff compared); the Reserves switch is the round-4 "Data tables" control.
  Reading quality of `.duo`, measured in the private and fallback builds: at 1,280 px each column
  is 500 px (about 70 characters of 15 px text) and 4-7 lines, the two columns within a line of
  each other; at 900 px 410 px and 5-8 lines; below 900 px one column. The history's two ledes
  take a column each; the pool, level-2 and SEC ledes and the SEC note flow one paragraph across,
  so the eye goes back up 4-7 lines, all on one screen: kept (one column was 9-12 lines beside an
  empty half). Reserves' switch checked: native button, collapsed, `aria-controls` names the four
  tables, Enter opens and Space closes, focus ring seen, each table under its chart at 1,280 and
  390 px; the fallback build holds Reserves back, as before. Fixed: (1) Home's checks filled row
  by row, so the left column read 1, 3, 5, 7 against the list (and Tab and screen reader) order;
  they now fill the left column first (`grid-auto-flow: column`, `--rows` half the list rounded
  up): left the data checks, right the delivery, attestation and signature checks; rows still
  pair, so hairlines align; tests/w12.mjs checks the order. Left: a check whose neighbour opens its
  fold or has a longer result gets a taller state bar (its row grows); still its own cell. Gates
  (load average 15-20): `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` exit 1 on the
  first run (the new order test, edited during the run, read the old build), then 0 (569 ok);
  `ONYX_RIGHTS=cleared ONYX_PUBLIC=1` 0 (675 ok); `ONYX_WEB=1 scripts/check-all.sh` 0 (686 ok);
  `scripts/check-docs.sh` 0; `scripts/check-secrets.sh` 0; final `ONYX_PUBLIC=0
  scripts/build-web.sh` 0 (686 ok). Not checked: Safari, a real phone, a screen reader.

- 2026-10-17 W12 round 5 (branch web-style-5 from d73663a; private preview build). CSS, layout and
  markup only: no sentence, number or honesty tag changed, and no new label (the Reserves switch
  is the round-4 "Data tables" control, reused). Critique of all 7 pages at 1280 and 390 px, ranked:
  (1) Data's long section ledes (One week, Who mined the blocks, Beyond Bitcoin, SEC filings) and the
  SEC source note ran 6-10 grey lines in a 68-character column with the right half of the page
  empty; (2) the API's "Where each dataset has been served so far" list floated under the endpoint
  cards in the left column, 450 px of text beside an empty column; (3) Reserves still drew a "Data
  table (67 rows)" fold in each of its four cards; (4) Home's title was 68 px, smaller than every
  inner page's 88 px title, so the first page had the weakest heading; (5) Home's seven checks were
  full-width rows, a title at the left and its status 600 px away, 530 px deep; (6) the pool panel's
  closing note was body-size text after its table fold, and each reserve card's week line floated
  under its chart. Changes: `.duo` (app.css, one rule) sets a long lede or note in two text columns
  with a hairline between on a screen of 900 px or more (two paragraphs a column each, one long
  paragraph flowing from the first column into the second; source order kept; one column below
  900 px), used by Data's four long ledes and the SEC source note (its bar now spans the two cards);
  the API's served list is its own panel across both columns, the four datasets two a row under
  hairlines with the dataset's name in --ink, "Nobody hosts a public endpoint yet…" its footer
  (the fallback note sits in the panel too); Reserves wraps its legend and cards in a `TableGroup`,
  so one "Data tables" switch in the legend row (`aria-expanded`, `aria-controls` naming the four
  tables) opens every card's table right under its chart; each card's week line is its footer row;
  the pool note is the pool panel's footer (`.pfoot`, `.fine`); `--text-hero` is clamp(42px, 6.2vw,
  80px) (was 68 px at most), two lines in the hero's column, the proof cards' titles still on the
  first screen at 1280 x 900; Home's checks sit two a row from 900 px (rows pair up, so hairlines
  align; 300 px shorter). Before/after in docs/audit/website-style-5/*-compare.jpg (9 files, 576 KB;
  before = d73663a). Checked: all 7 pages at 1280 and 390 px in the private build before the change,
  and Home, Data, Reserves and API after it (reduced motion), viewed. tests/w12.mjs gains the round-5 checks, fallback-aware (Data:
  every `.duo` reads in two columns with a rule at 1280 px and one at 390, 5 of them, 4 in the
  fallback build; the pool note is its panel's last row; API: the served panel spans the section,
  four datasets two a row (one on a phone), a footer last; Reserves: one collapsed switch names the
  four tables, no fold per card, opens all four with rows and closes them; Home: the title at least
  76 px on two lines at 1280 px, the checks two a row (one on a phone); footers counted on Data 2,
  Reserves 5, API 1). tests/e2e.mjs reads the reserve tables through the switch. On the pre-round
  build the new checks fail 15 times; on this one w12.mjs passes 183 ok standalone.
  Gates (load average 16-26): `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` exit 0
  (569 ok, 0 failed); `ONYX_RIGHTS=cleared ONYX_PUBLIC=1 scripts/build-web.sh` exit 0 (675 ok);
  `ONYX_WEB=1 scripts/check-all.sh` exit 0 on the first run (its web gate 686 ok, 0 failed);
  `scripts/check-docs.sh` 0; `scripts/check-secrets.sh` 0; final `ONYX_PUBLIC=0
  scripts/build-web.sh` exit 0 (686 ok, 0 failed). No run needed a rerun.
  Not checked: Safari, a real phone, a screen reader.
  Ideas for round 6: (a) Reserves' "Balance over the week" lede and the "Served through the API"
  intro (6 lines inside its panel, right half empty) could take `.duo` too; (b) the API's "Your node
  checks the root" is a lone paragraph with an empty right half (its three code names could be a
  chain, `.eq`, or sit beside the endpoints); (c) Home's testnet note box is wider than its text;
  (d) Run's 12 steps run about 2,500 px with a mono "The driver's log" fold in each, the step
  numbers small and grey; (e) each reserve card's mono week line wraps to two lines at 1,280 px
  (two labelled figures would read better); (f) Explorer's row-proof panel centres its tree with
  the controls far right and the left third empty.

- 2026-10-17 W12 round 4 review (fresh reviewer, after merging main 50e0d77 with E9/E10's test
  waits; the merge had no conflicts). No sentence, number or honesty tag changed; the one new
  label, "Data tables", is plain and true. The switch is a native button with `aria-expanded`,
  collapsed by default, keyboard operable (Enter opens, Space closes, checked) with the global
  focus ring (seen); every table stays reachable, right under its chart. Fixed: (1) the switch now
  names the tables it opens in `aria-controls` (each grouped table registers its id with the
  TableGroup); (2) the round-4 tests swallowed their waits' timeouts (`.catch(() => {})`); they
  now use wait.mjs `attr` and throwing waits, and check the switch starts collapsed and names
  every table; (3) the history's day switch, freed from `.wrap` by the class fix, stretched across
  the panel's grid as a mostly empty 1,006 px bar at 1,280 px: it now hugs its days (428 px). The
  `.wrap` to `.multi` rename touched only Data's company and day switches. No-JS and print are
  unchanged (a hash-routed app, and tables were built only when opened before too). Fallback: the
  multiples are held back there, the tests skip them; Home, Run and level-2 seen in the fallback
  build. Gates (load average 17-20): `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh`
  exit 1 on the first run (a `$props.id()` placement this review introduced, fixed), then 0 (561
  ok); `ONYX_RIGHTS=cleared ONYX_PUBLIC=1` 0 (665 ok); `ONYX_WEB=1 scripts/check-all.sh` 0 (676
  ok); `scripts/check-docs.sh` 0; `scripts/check-secrets.sh` 0; final `ONYX_PUBLIC=0
  scripts/build-web.sh` 0 (676 ok). The day-switch fix came after the two public builds (CSS only,
  a section the fallback build holds back). Not checked: Safari, a real phone, a screen reader.

- 2026-10-17 W12 round 4 (branch web-style-4 from e95192c; private preview build). CSS, layout and
  markup only: no sentence, number or honesty tag changed. One new control label, "Data tables"
  (the panel switch below), replaces eight "Data table (N rows)" folds; nothing else is new text.
  Critique of all 7 pages at 1280 and 390 px, ranked: (1) Data's small multiples repeated a "+ Data
  table" fold under every chart (3 under the series, 5 under the history), a 44 px row each between
  charts; (2) Run still had notes floating between cards (the run summary and reserve-round lines
  under the tiles, the rehearsal's source line, the recount note) and the dispute chain floated
  between its panel and the payouts; Data's SEC source note floated under its cards; Run's indexer
  intro was set as small print; (3) on a phone a check chain stranded its operators ("range rows
  →" at a line's end, "→ final …" or "· 7 of 7 days" at a line's start); (4) Reserves' API panel ran
  its four facts in a column beside a short rule, the rule's column half empty; (5) Home's proof
  cards ended their text 60-80 px above a link floating at the card's foot; (6) a class collision:
  Data's company and day switches used `.seg.wrap`, which also took the page container's `.wrap`
  (1,080 px max, auto margins, 20 px side padding), so the company switch sat mid-row at 1,280 px and
  centred on a phone; (7) Data's level-2 facts on a phone kept a 120 px name column, so values took
  two or three lines. Changes: `TableGroup` (new component) gives a panel of small multiples one
  "Data tables" switch in its legend row (`aria-expanded`); each chart's table stays the element right
  under its chart (W3) and opens and closes with the switch, its own fold not drawn; a hairline
  parts one company's chart from the next; `.eq` is one shared rule in app.css (was a copy on Data
  and Run) and `.eqp` keeps an operator with its value (arrows and Σ lead the next value, list dots
  trail the one before); the dispute chain is its panel's footer row; `.pfoot` and `.notebar` take
  several notes (grid); Run's two summary lines share one note bar, the rehearsal line and the
  recount / held-back note have theirs, the fee split's two notes are its panel footer, the
  indexer intro reads as section ledes; Data's SEC source note sits in a note bar; Reserves' API
  facts are `dl.facts` cards (shared `.fact > dt / dd`, two a row on a wide screen; on a phone one
  grouped card, `.facts.stack` puts each name over its value), its comparison note is the panel
  footer; Home's proof-card links are each card's footer row under a hairline, at one height; the
  switch class is `.seg.multi`; level-2 facts stack name over value under 560 px.
  Before/after in docs/audit/website-style-4/*-compare.jpg (7 files, 360 KB; before = e95192c).
  Checked: all 7 pages at 1280 and 390 px in the private build before the change and Home, Data,
  Reserves, Run after it (reduced motion), and Home, Data and Run in the fallback build, viewed; the
  open state of the series tables at both widths (no sideways scroll, no console error).
  tests/w12.mjs gains the round-4 checks, fallback-aware (one switch per multiples panel and no
  per-chart fold; the history switch opens every table under its chart, with rows, and closes them;
  the company switch at the row's right edge; every chain operator on one line with its value; no
  loose note on Run, the dispute chain its panel's footer; footers counted on Run 5, Home 3,
  Reserves 1; the API facts two a row with the endpoint on one line, one a row on a phone; proof
  footers at one height; level-2 names over values on a phone). On the pre-round build they fail 23
  times; on this one they pass (173 ok standalone).
  Gates (load average 7-21): `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` exit 0
  (561 ok, 0 failed); `ONYX_RIGHTS=cleared ONYX_PUBLIC=1 scripts/build-web.sh` exit 0 (665 ok);
  `ONYX_WEB=1 scripts/check-all.sh` exit 0 on the first run (its web gate 676 ok, 0 failed);
  `scripts/check-docs.sh` 0; `scripts/check-secrets.sh` 0; final `ONYX_PUBLIC=0
  scripts/build-web.sh` exit 0 (676 ok, 0 failed). No run needed a rerun.
  Not checked: Safari, a real phone, a screen reader.
  Ideas for round 5: (a) Data's long section ledes (Beyond Bitcoin, SEC filings, Who mined the
  blocks: 6-9 lines of grey) could sit beside their panel on a wide screen; (b) the pool panel's
  closing note is body-size text after its fold, not a `.pfoot`; (c) API page: "Where each dataset
  has been served" floats as a long bulleted note under the endpoint cards; (d) Reserves' four
  per-card "Data table (67 rows)" folds could share one switch like the multiples; (e) the
  level-2 card's source link takes a 44 px row on a phone, leaving a gap under its name.

- 2026-10-17 W12 round 3 review (fresh reviewer; main had nothing new, merge base bf50858, so no
  conflicts). Diff read: no visible sentence, label or honesty tag changed (spans, wrappers, moved
  notes); the only new numbers are the OKX preview's scale and cut labels. Traced to
  apps/demo/site/data/demo.json `live.per_block`: block 970,545 is 455.279 BTC in, 369.482 out;
  the next busiest is 970,562 at 58.511 in, so 455 > 3 x 58.5 and the scale is nice(58.511) =
  60.0; both cut bars carry the break mark and their rounded value, and no other block passes
  60. The fallback build ships no preview (no compact OKX chart on Home, "no OKX flow figure"
  check passes, the public-build scan tests pass). Fixed: the compact chart is one `role="img"`
  with no data table, so the cut values were visual only; its text alternative now adds "Block
  #970,545 runs past the scale (60.0 BTC each way): 455 BTC in, 369 BTC out." (FlowChart
  `cutNote`, compact only; Data's full charts keep their table). Gates (load 8-30):
  `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` exit 0 (551 ok);
  `ONYX_RIGHTS=cleared ONYX_PUBLIC=1 scripts/build-web.sh` exit 0 (648 ok);
  `ONYX_WEB=1 scripts/check-all.sh` exit 101 then exit 0 (659 ok): the failed run's one failure
  was onyx-api's `store_load_is_not_quadratic_in_rounds` timing test under load (10k rounds
  4.1 s, 40k 32.7 s), Rust this branch does not touch. Home, Run, Reserves and Data spots
  screenshotted at 1280 and 390 px in the fallback and private builds and viewed; no sideways
  scroll and no console error on those pages. Not checked: Safari, a real phone, a screen reader.

- 2026-10-17 W12 round 3 (branch web-style-3 from bf50858; private preview build). CSS, layout
  and markup only: no sentence, label or honesty tag changed (spans and wrappers added, no words).
  The one changed number is a chart scale: Home's OKX preview reads 60.0 / 0 / -60.0 (was 500),
  with its one outlier block cut and labelled 455 in, 369 out, the export's values. Critique of
  all 7 pages at 1280 and 390 px, ranked: (1) code names and values in running text broke inside
  themselves ("btc-entity-reserve-" / "v1" on Run at 1280, "crates/onyx-" on a phone, the run
  date in the indexer tag, "six-" / "block steps" in Data's history rows); (2) notes under panels
  floated as grey paragraphs between sections (Run x3, Data, Reserves); (3) Run had three card
  looks (figure tiles, flat contract/fact cards, flat pricing tiles), and on a phone each contract
  was its own 80 px card; (4) Home's proof cards 01 and 03 sat half empty beside 02 (400 px tall);
  (5) the OKX preview read empty (one 455 BTC block set a +-500 scale); (6) the Reserves API table
  stopped at 70% of its panel (a `display: block` table shrinks to its content). Changes: inline
  `code`/`span.mono` in p, li, dd and label never wraps (app.css); the two longer than a phone
  line (Data's golden_history command, Reserves' servingCheck) are `.cmd`, breaking only between
  words, hyphenated words in `.nw`; `.nw` round the run date, the history row parts and the client
  checks; a panel's note is its last row under a hairline (`.pfoot`: Run's run note, axis note and
  indexer note, Data's block note), Reserves' shard note sits in a quiet bar under the cards
  (`.notebar`); one card family in app.css: `.fact` (name over value, values at --text-base) and
  `.tile` take the `.stat` surface, `.facts` is the shared grid, and on a phone a group of facts is
  one card with a 44 px row per fact (name left, value right; contracts stay links); Home's
  reserves card column is 1.4x (cards 400 to 337 px; equal columns when the card is held back);
  FlowChart's `clip`: when one block is over 3x the next busiest, the scale fits the next one and
  the outlier's bars get Data's break mark and value; Reserves' tables scroll in a `.tablebox`, so
  they span the panel. Before/after in docs/audit/website-style-3/*-compare.jpg (9 files, 400 KB;
  before = bf50858). Checked: all 7 pages at 1280 and 390 px (reduced motion) in a private build
  and Home, Run, Reserves in the fallback build, viewed (the client-checks spans came last and are
  seen in the fallback shots); no sideways scroll and no console error
  on any page at either width in the private and fallback builds (scratch probe, 14 of 14 each).
  tests/w12.mjs gains the round-3 checks, fallback-aware (no code value split or overflowing;
  panel notes are the panel's last row with a full-width rule; Run's cards share the tile surface,
  grouped rows >= 44 px on a phone; proof columns, equal when held back; the OKX preview's scale,
  cut count and labels computed from the export's per-block rows, skipped where FB hides it). On
  the pre-round build they fail 16 times; on this one they pass. Gates (load average 8-41):
  `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` exit 0 (551 ok, 0 failed);
  `ONYX_RIGHTS=cleared ONYX_PUBLIC=1 scripts/build-web.sh` exit 1 then exit 0 (648 ok): the failed
  run's one failure was the known Data Tab walk under load (44 stops, the footer's "Labels" link
  off screen), nothing round 3 touches; `ONYX_WEB=1 scripts/check-all.sh` exit 0 on the first
  run (its web gate 659 ok, 0 failed); `scripts/check-docs.sh` 0; `scripts/check-secrets.sh` 0;
  final `ONYX_PUBLIC=0 scripts/build-web.sh` exit 0 (659 ok, 0 failed).
  Not checked: Safari, a real phone.
  Ideas for round 4: (a) Data's history multiples repeat a "+ Data table" fold under every chart
  (5 in a row); one table switch for the panel; (b) Reserves' API panel leaves the prose column
  half empty beside the four facts; the facts could use the new `.facts` cards; (c) Run's dispute
  chain ("fake -> panel of 3 -> final") wraps its arrow onto its own line on a phone; (d) Run's
  remaining floating notes (the recount note, the local rehearsal's source line) could join the
  facts as a footer; (e) Home's proof card 01 still ends about 80 px above its link at 1280.

- 2026-10-16 W12 round 2 review (fresh reviewer, after merging main with the W8 copy pass,
  19a8a13). The only conflict was this log (both entries kept); the .svelte files merged cleanly,
  so W8's words and round 2's markup both stand, and the diff against main is round 2's alone.
  Diff read: no sentence, label or honesty tag changed; the only added numbers are the preview
  charts' scale labels, and they match the data (transactions 8,000 = nice(7,565), the busiest
  block; OKX ±500 BTC, the same scale as the Data page's full-scale OKX chart). Shimmer and lift
  are off under reduced motion (the global rule) and with the pause button; the card focus ring
  passes the Tab walk. Fixed: with live rows that do not check out (a forged OKX row), the OKX
  slot shimmered and stayed aria-busy for ever, which read as still loading next to a market that
  says "Check failed"; it now stops once the sums were tried (scratch probe: honest page draws the
  chart, forged page leaves a still slot, no aria-busy). Timing: round 2 adds no wait. A scratch A/B
  of the Home click-to-focus move, motion on, 6 trials each at load 22-25: median 26 ms as shipped,
  11 ms with round 2's shimmer and lift switched off, max 36 ms in both, against the test's fixed
  150 ms; the failing checks are waits that do not wait for the thing (a fixed 150 ms, a
  "settled" that passes before the app mounts), which the E10 lane is replacing. Gates:
  `ONYX_WEB=1 scripts/check-all.sh` exit 0 on the first run (load 27-31); `ONYX_PUBLIC=1
  scripts/build-web.sh` exit 1 then exit 0 (the failed run met an unmounted Home in the desktop
  a11y walk, landmarks 0,0,0,0, 800 ms after load at load 20-26); final `ONYX_PUBLIC=0
  scripts/build-web.sh` exit 1, exit 1, then exit 0 (625 ok, 0 failed) at load 25-32: the first
  met a half-loaded Data and Explorer in the Tab walk (44 and 18 stops instead of 57 and 27; the
  footer link then sat off screen), the second timed out after 30 s waiting for Home's market
  section in the W4 review; a different wait each time, none on anything round 2 changed;
  `scripts/check-docs.sh` 0; `scripts/check-secrets.sh` 0. Both builds screenshotted at 1280 and 390 px (7 pages, reduced motion) and viewed; a scratch
  probe found no sideways scroll and no console error on any page at either width in either build.
  Not checked: Safari, a real phone.

- 2026-10-16 W12 round 2 (branch web-style-2 from 0ccc90f; private preview build). CSS, layout
  and markup only: no visible sentence, number or honesty label changed (a W8 copy pass runs in
  parallel on web-w8). Critique of all 7 pages at 1280 and 390 px, ranked: (1) 64 px above and
  below every section left 130-200 px empty bands between panels; (2) prose panels held a
  62-character column in a 1,040 px card with the right half empty (Reserves "Where the base comes
  from" / "What each number proves" stacked, "Served through the API", Data's level-2 card); (3)
  Home's live preview charts had no scale (round 1 idea c); (4) phone density: each home check ran
  112 px (its 44 px "What this checks" fold under a 16 px gap), each ladder rung 180-230 px; (5)
  Reserves cards set each company's balance at body size, level with its name; (6) a focused proof
  card ringed only its title, the live preview card's hover changed a border colour nobody could
  see (gradient border), and the OKX chart's loading slot was an empty gap. Changes: one rhythm
  token `--section-y` (48 px, 32 on phones; API's own 24/80 px overrides dropped); a shared
  `.section.two` (two equal-height panels side by side from 861 px, used by Reserves and Labels);
  Reserves' API panel and Data's level-2 card put the prose beside the facts from 900 px, divided
  by a hairline; Home's compact charts draw their scale (top value, zero; In/Out ±max) in a right
  gutter in the shared axis style (`gutterFor` in chart.ts); phone check rows tuck the fold into
  the row's padding (94 px, the fold still 44 px tall) and ladder lines stack 8 px apart with the
  number and icon centred on the name; Reserves' balance at --text-lg (name above it on phones);
  the proof card's focus ring goes round the card (title underlined), the preview card lifts like
  the proof cards with its arrow stepping forward, and the loading slot shimmers like the other
  placeholders (still under reduced motion). Before/after side by side in
  docs/audit/website-style-2/*-compare.jpg (9 files, 620 KB; before = 0ccc90f). Checked: all 7
  pages screenshotted at 1280 and 390 px (reduced motion) in both builds and viewed; no sideways
  scroll and no console error on any page at either width; preview chart labels inside their panel;
  contrast unchanged (tests/contrast.py: 132 pairs, 0 below AA; new text uses existing pairs).
  tests/w12.mjs gains the round-2 checks (rhythm, two-panel sections, preview scale, phone check
  rows, card focus ring). Gates (load average 28-42 from other lanes): `ONYX_PUBLIC=1
  scripts/build-web.sh` exit 0 (611 ok, 0 failed); `ONYX_WEB=1 scripts/check-all.sh` exit 1
  twice, each time every repo step passed and one website timing check failed, a different one
  each run (the Data Tab walk met a half-loaded page, 26 stops instead of 44; the Home focus move
  checked 150 ms after a click whose focus waits a frame); both pass in the public build, in the
  other check-all run and in the final `ONYX_PUBLIC=0 scripts/build-web.sh` (exit 0, 622 ok, 0
  failed), and a scratch rerun of the Data Tab walk shows the same off-screen stop on the
  pre-round build under load; `scripts/check-docs.sh` 0; `scripts/check-secrets.sh` 0. Not
  checked: Safari, a real phone.
  Ideas for round 3: (a) Home's proof cards 01 and 03 sit half empty beside 02's long text at
  1280 px; (b) inline code in prose breaks mid-token (Data's history lede "ONYX_HISTORY_DIR=…
  cargo test", Run's "btc-entity-reserve-v1"); (c) Run has a second tile look (contract and "Root
  on chain" cards, label over value) beside `.stat`: one fact-card style; (d) notes under panels
  (Reserves "66 week shards…", Run "Contracts deployed…") float as orphan grey paragraphs: attach
  them to their panel as a footer row; (e) Home's OKX preview reads almost empty (one 450 BTC
  block sets the scale): a clipped scale with a break mark, as Data's "Up to 10 BTC"; (f) the
  focus underline spans the title's block, not its text.

- 2026-10-16 W8 copy pass (private build of web-w8 vs 7b9d4e4; text, links and tests only, no
  CSS). Method: a scratch Playwright script dumped every page's rendered `main` text and links
  (details opened) at 1280 px before and after; issues listed per page, then fixed. Run: three
  places where Svelte trims the space at the start of an `{#if}` block glued words together:
  "contractthe 2026-10-12 run deployed.In", "page).Root", "Public testnet· chain 84532" (and the
  same pattern in the Series chart tooltip, "#height· time"); each now has an explicit `{' '}`.
  The Base Sepolia pill said "REHEARSAL PASSED" (the driver's literal verdict) while the page calls
  only the anvil run a rehearsal; it now says "Run passed" (the recorded verdict is still tested).
  Dataset price: "never more than buying every row, never less than 4 rows' worth" (was "never
  more than every row, never less than 4 rows"; rule from crates/onyx-api/src/catalog.rs). Home:
  milestone M6 shown as "now" but PLAN.md has it done (deployed and exercised 2026-10-12): now done;
  level 2 "Nodes drawn at random" -> "Observers drawn at random ... seal their answers" (the Data
  page's term); validators "slash liars" -> "cheaters lose their bond"; the signature tile's
  "Two signatures or it does not count in full" -> "Both signatures make a label level 3; an
  unsigned list stays a claim, level 4" (the Labels page's levels); the bond tile links its proof
  (the Run page, where a cheater's bond is taken on Base Sepolia). Data: level-2 lede "This run is
  a local rehearsal" -> "This round" (GLOSSARY: round); data map "Bonded nodes" -> "Bonded
  observers". Reserves: Binance's and BitMEX's address lists (the two without a cross-check link)
  link their sources, Binance's PoR file folder and the BitMEX PoRL bucket (docs/ops/data-sources.md
  S7/S8, fixtures/labels/PROVENANCE.md). Explorer: the block range in the header was the literal
  "921,080–921,082"; now read from the shard's blocks (same value). Labels: "Official BIP-322
  vectors" links the BIP's vector folder (github.com/bitcoin added to the public guard's
  reviewed organisation list, public_build.rs ENTITY_HANDLES, or the public build refuses it); the Brier slider says what the score is (GLOSSARY).
  API: "Served in the local and Base Sepolia rehearsals" -> "the local rehearsal and the Base
  Sepolia runs". The first fix ("real value · test round") is unchanged. Kept as they were (true
  and tested): "Base Sepolia (testnet)", "Not yet checked", "Live run", the data map's "Live"
  for the recorded level-2 rounds (defined on the page as running in code today). Browser tests:
  each Base Sepolia run says "Run passed" and keeps the spaces around the chain id and the root
  note; the indexer scope says "settlement contract the <date> run deployed." with a space after.
  Screenshots (scratch, not committed): run scope, run footer, run header, home bento, home
  milestones, explorer header, reserve cards, Labels vectors and Brier panels, before and after.
  Browser tests: private 502 ok, public 491 ok, 0 failed (one public run first timed out on the
  forged-history day picker under load average ~40, unrelated to copy; reran clean).
  `ONYX_PUBLIC=1 scripts/build-web.sh`, `ONYX_WEB=1 scripts/check-all.sh`, check-secrets and
  check-docs exit 0.
  Open: repo paths named on the pages (scripts/, docs/, ADRs) cannot link until the repo
  visibility is decided (STATUS "Need from the project owner" 6).
  Review (fresh reviewer, after merging main with W12 round 1, 0ccc90f): conflicts in Run,
  API, the series tooltip and this log, resolved with W12's markup (`.fine`, `.nw` spans,
  `{" "}`) and W8's words; no space fixed twice. Claims checked: "Run passed" (the Base Sepolia
  runs are `mode: testnet`; docs/ops/demo-testnet.md calls only the fork run a rehearsal), M6
  done (PLAN.md), the cheater link (each Base Sepolia run took a 4 USDC bond), price rule, the
  Brier wording (GLOSSARY). Fixed: the two reserve source links opened nothing (BitMEX's
  `/data/porl` returned 404, Binance's folder an empty object); now the bucket's own file
  listing of `data/porl/` and Binance's newest wallet file (20260801, the one PROVENANCE says
  the indexer re-downloaded), each fetched 200. The guard's `github.com/bitcoin` is an exact
  handle (`github.com/bitcoinjane` is still refused; test added). Screenshots of every changed
  spot in the public build at 1280 and 390 px viewed; no horizontal scroll.

- 2026-10-16 W12 round 1 review (fresh reviewer, after merging main with L13 and the last W4
  commits; no conflicts). Diff read against main: no number or honesty label changed; added text
  is the API page's "Endpoints" heading, the footer's state line (same words as Home's note) and
  page list, and the slab kickers (aria-hidden). Both builds screenshotted at 1280 and 390 px and
  viewed: no horizontal scroll, no console errors, lazy sections load, Home's market still says
  "Not yet checked" before the run. Fixed: the market question's block range and threshold no
  longer break mid-value on a phone (.nw); Svelte dropped the space before " · chain 84532" on
  Run and " · <time> UTC" in the series chart tooltip (now {" "}). Screenshots trimmed to the 14
  fold compares, JPEG 70, max 1280 px (8.5 MB to 0.8 MB). The first public e2e run failed 2
  overview checks with load average 34; a probe and the rerun passed 7 of 7 (flake, not W12).

- 2026-10-16 W12 round 1 (branch web-style-1; private preview build). Started by an earlier
  agent and stopped unreviewed (WIP 705cf41); finished here. Critique of the WIP, ranked: (1) the
  mono fact and value lines broke inside a range or a figure, because the body's 62ch measure
  is about 485 px in mono (Data's slab "22:59– / 01:21 UTC", API's "· no public endpoint yet" on
  its own line, the week's "Blocks … · one bar per six blocks" on 2-3 lines in a 1,000 px panel);
  (2) Home's 7 checks were bordered cards inside the panel (box in a box, 50 px of width lost on
  a phone); (3) the market verdict put "BTC" of "Value 176.46741272 BTC" on its own line at 1280;
  (4) "company key" ran into the dashed ring of the Labels-signed tile (grid rows pushed it to
  the lower edge); (5) section openers, kickers, body-face sentences, one figure tile and the
  footer (the WIP's own work) kept as they were. Changes: `p.meta` measures up to 110ch and
  `.slabmeta` 96ch, with `.nw` (no wrap) around block ranges, time spans and BTC figures on Data,
  API and Reserves, so lines break between parts; the checks are one hairline-divided list with a
  2 px state bar per row (quiet ready, accent checking, green passed, red failed; the pill still
  says it in words) and titles at --text-base; the verdict column is 240 px; the seal's two lines
  sit together at the ring's centre. No data, text or honesty label changed (only spans added).
  Checked: screenshots of all 7 pages at 1280 and 390 px (reduced motion), viewed one by one;
  before/after side by side in docs/audit/website-style-1/*-compare-*.jpg (before = the pre-W12
  site, after = this round). No horizontal scroll on any page at 390 or 1280, with the browser in
  dark and in light scheme; a light-scheme browser gets the same dark site (dark only, W3
  decision; body #080909 on ink #f1eee6 in both). Browser tests (tests/e2e.mjs with w4 and w12):
  602 ok, 0 failed. Not checked: the public build's screenshots, Safari, a real phone.
  Ideas for round 2: (a) a calmer vertical rhythm, 64 px above and below each section leaves
  ~150 px empty bands between panels; (b) prose panels on Reserves ("Where the base comes from",
  "Served through the API") hold a 62ch column in a 1,040 px panel with the right half empty; a
  side column for the facts, or a narrower panel; (c) Home's live preview charts have no scale
  or baseline label, and the OKX chart reads as almost empty at that size; (d) Home's "What
  this checks" disclosures sit 44 px tall on phones (tap target) and spread the rows; group the
  detail under the title; (e) the footer wordmark and the slab could share one accent motion.

- 2026-10-15 W4 speed (both builds of web-w4 vs dc50e58). Target set first: home page first
  visit <= 250 KiB on the wire (gzip) in both builds, before any scroll or press; met (217 public,
  219 private, was 2,090 / 3,236). Changes: (1) the verifier (WASM 1.8 MiB gzip) and the private
  preview's recipe input (1.1 MiB) load on Home only when a section that shows checked values
  (figures, market, live chart) is on screen or the checks run (Verify, the palette); the figures
  show "–" until then; API shows its sample from demo.json and loads the verifier on its button;
  Labels starts its Brier scores when the browser is idle, not at mount. Data, Reserves, Explorer
  and Labels still load the verifier after first paint (their content is its output); history
  files load only on Data. (2) Main thread: CPU profiles of Data (4x throttling, Chrome 152)
  showed `toLocaleString('en-US')` at 1.0-1.3 s (a new Intl.NumberFormat per call; now one shared
  formatter, same text), the slab's WebGL shader compile holding the main thread 0.5-12.8 s on a
  software renderer (headless Chrome, SwiftShader; now drawn on an OffscreenCanvas in a worker
  where the browser has WebGL there, the old path otherwise), and the week's charts drawn 7 times
  (once per checked day; now once, the pill still counts the days). (3) `run/heads.json`: the
  build copies the few fields the home proof card reads from each listed run file (verdict,
  network, date, transactions, reserve round; 455 B instead of three 45-58 KB files; on the
  public build's allowlist). (4) Shipped JSON without indentation (whitespace outside strings
  only; the build refuses a file whose parsed value changes): 73 KB raw less, about 4 KiB gzip.
  Chunks: already one per route (shared 52 KiB gzip; each page 3-17 KiB of its own); the WASM is
  fetched once per visit (six page changes through the nav: 1 request) from
  `app/immutable/workers/assets/onyx_web_verify_bg-<hash>.wasm`, cacheable forever. wasm-opt
  (binaryen 131 via npx, free) measured and not kept: -Oz cut the WASM 3,854,971 -> 3,601,345 B
  raw but gzip grew 1,870,277 -> 1,894,203 B (-Os 1,893,179, -O2 1,899,852); the data section
  (1.35 MB, mostly secp256k1 tables and wasmi strings) does not shrink. Not done: fonts (119 KiB,
  six faces, already latin subsets) are now half of the home page's bytes.

  Measured (Chrome 152, `tests/audit.mjs` with AUDIT_LOAD_ONLY=1 for wire bytes, one cold visit
  per page; `tests/bench.sh` Lighthouse 12.8.2 mobile, median of 3 runs, Data median of 8; load
  average 4-57 while measuring, mostly 15-35, so TBT and perf are noisy; bytes are not):

  | Page | Wire public, before -> after | Wire private | Requests (public) | Lighthouse perf public / private | Lighthouse TBT public / private |
  |---|---|---|---|---|---|
  | home | 2,089.6 -> 216.6 KiB | 3,236.4 -> 219.4 KiB | 56 -> 53 | 89 -> 87 / 85 -> 89 | 139 -> 260 / 272 -> 151 ms |
  | data | 2,207.4 -> 2,206.9 | 2,210.3 -> 2,209.8 | 57 -> 58 | 60 -> 66.5 / 63 -> 75 (8 runs) | 1,961 -> 1,312 / 1,865 -> 622 ms (8 runs) |
  | reserves | 2,032.7 -> 2,033.3 | 2,033.0 -> 2,033.6 | 44 -> 45 | 92 -> 87 / 93 -> 94 | 161 -> 386 / 129 -> 93 ms |
  | run | 230.5 -> 229.8 | 230.6 -> 229.9 | 47 -> 48 | 90 -> 86 / 75 -> 92 | 80 -> 248 / 358 -> 124 ms |
  | explorer | 2,034.3 -> 2,033.9 | 3,179.5 -> 3,179.1 | 46 -> 47 | 92 -> 80 / 93 -> 94 | 165 -> 360 / 134 -> 48 ms |
  | labels | 2,032.8 -> 2,032.4 | 3,178.0 -> 3,177.7 | 44 -> 45 | 86 -> 93 / 74 -> 75 | 315 -> 91 / 99 -> 55 ms |
  | api | 2,028.5 -> 192.3 | 2,028.8 -> 192.4 | 38 -> 38 | 91 -> 94 / 91 -> 94 | 196 -> 117 / 179 -> 61 ms |

  Data's 8 Lighthouse TBT runs ranged 827-7,421 ms before and 436-2,754 after (public), 902-4,567
  and 55-2,012 (private); the public after-runs and the 3-run medians on other pages sit inside
  that spread (the 3-run medians of the public build were taken at load 17-49 after, 14-40
  before), so only the byte columns and Data's 8-run medians are firm. Home's first paint does
  not change (FCP 1.5 s Lighthouse in all four); Labels' private LCP stays 8.7 s (it waits for the
  decoded input, its main content). One extra request per page: the slab's worker (1.6 KiB).
  Browser tests (new, tests/w4.mjs: home at 1280 and 390 px fetches no verifier, input or
  history and shows "–" until scrolled to the figures, which loads the verifier once and fills
  them; Verify alone loads it once and 7 of 7 pass; API shows the sample with nothing heavy and
  checks on the button; per page exactly the heavy files it needs on a first visit, none on Run;
  one WASM request across six page changes, from a content-hashed path; the slab canvas handed to
  a worker on Home, Data and Run; run/heads.json equal to the listed run files and read instead of
  them; every data file without indentation): public 481 ok (was 462), private 492 ok (was 473),
  0 failed. `ONYX_WEB=1 scripts/check-all.sh` passed.
  Review fixes (two predate W4): the home market said "Checked against the pinned root" as soon as
  the OKX row was decoded (decoding is no check) and before any check ran; it now says "Not yet
  checked" until the live-root check (Merkle root over the live rows = the pinned one) passes in
  this browser, then "Checked against the pinned root", and "Check failed: ..." when it fails.
  The lazy figures, the block range, the market answer and the empty flow chart showed a bare
  "–"/"…" or an unnamed box: each now has a visually hidden "Loading <what>" (the dash hidden
  from screen readers; the chart slot aria-busy). The slab falls back to drawing on the page, on
  a fresh canvas, when its worker fails (was: the flat ground). New tests (tests/w4.mjs): market
  text before Verify, after it, and with one live OKX row changed; loading names and axe on those
  parts before they load; the stone on the page without OffscreenCanvas; the fallback when the
  worker script throws; in the worker, frames run, stop on the pause button and run again (a
  test hook, localStorage `onyx-stone-test`, lets a software renderer drift). Browser tests after
  the fixes (merged with main at 2026-10-16): public 488 ok, private 499 ok, 0 failed;
  `ONYX_WEB=1 scripts/check-all.sh` passed. First visit re-measured after the fixes (same
  method, AUDIT_LOAD_ONLY=1, one cold visit), KiB on the wire public / private: home 217.3 / 220.0
  (53 requests each), data 2,207.3 / 2,210.2, reserves 2,033.7 / 2,034.0, run 230.2 / 230.3,
  explorer 2,034.3 / 3,179.6, labels 2,032.8 / 3,178.1, api 192.7 / 192.8. Home stays under the
  250 KiB target in both builds (+0.7 public, +0.6 private against the table above).

- 2026-10-15 W10 visual polish and W11 W6 follow-ups (both builds of web-w10 vs 8452d94).
  W10: one design-token set in app.css, and every page drawn from it: a type scale (`--text-xs`
  12, `sm` 13, `base` 15, `field` 16, `md` 18, `lg` 22, `xl` 22-28, `2xl` 28-36, `h2` 30-46, `h1`
  44-88, `hero` 42-68, `display` 80-220 px), five line heights (`--lh-display` 0.96, `none` 1,
  `tight` 1.15, `snug` 1.35, `body` 1.55), a 4 px spacing grid (`--sp-0..12`: 2, 4, 8, 12, 16, 20,
  24, 32, 40, 48, 64, 80, 96 px), five radii (`--radius-xs/sm/md/lg/pill`: 4, 8, 12, 18, 999; 50%
  for dots), named shadows (panel, lift, pop, button, rings, glow, active-item bars) and surface
  colours (`--header-bg`, `--slab-glass`, `--scrim`, `--hover`, `--line-bright`, `--accent-line`,
  `--good-soft`, `--good-line`); one ease and duration (`--ease`, `--dur` 160 ms). States: every
  button lifts on hover, sinks 1 px and dims when pressed; ghost buttons lighten; links get a
  full-strength underline; folds underline; fields and sliders brighten; the logo's glyph turns;
  nav items tint; table rows tint; all eased and all off under reduced motion or the pause
  button (W3). One section head (`.sh`: heading and status on one row, `--text-xl` inside a
  panel; Reserves' status pill no longer wraps under its heading, its two panel headings no
  longer at page-heading size) and one slab fact line (`.slabmeta`, 16 px under the title, was
  touching it on Reserves) in app.css instead of five copies; table heads in the caption style
  (12 px caps, tracked); one fold marker ("+"/"−" in mono accent) on every `<details>` (Home's and
  Run's were browser triangles); lists without the browser's 15 px margins; icons at 16, 18 or
  22 px (were 13-18 and 22) in one 1.75 stroke; placeholders: one bone style (a card on the page,
  bare inside a panel, was a panel in a panel), each named (Explorer's and Labels' said "Loading
  the data"); phone sections 40 px apart (was 44); Run's block axis on the rows' grid (was two
  calc() margins). Source (scratch scan of app.css and every `<style>`): literal font sizes 22 ->
  0, literal line heights 11 -> 0, literal spacing values 38 -> 1 (the screen-reader-only -1 px),
  literal radii 13 -> 0, box-shadow declarations not a token 16 -> 0, colour literals outside
  `:root` 36 -> 14 (all gradient stops and masks). Computed, all 7 pages at 1280 and 390 px
  (`tests/style-audit.mjs`, private build, reduced motion, Chrome 152):

  | Distinct computed values | Before | After |
  |---|---|---|
  | Font sizes | 24 | 15 (12 tokens; the fluid ones differ by width) |
  | Line heights (ratio to size) | 10 | 5 |
  | Spacing values (padding, margin, gap) | 30 | 12 (all `--sp-*`) |
  | Corner radii | 14 | 6 (all `--radius-*`, 50%) |
  | Shadows | 8 (1 a token) | 7 (all tokens) |
  | Colours not an app.css token (text, fills, borders, chart marks) | 10 of 34 | 0 of 29 |
  | Icon sizes / stroke widths (source) | 7 / 3 | 3 / 1 |
  | Fold marker styles | 2 | 1 |
  | Placeholders: inside a panel / unnamed | a card in the card / 2 | bare bones / 0 |
  | CSS, private build (raw / gzip) | 69.3 / 12.3 KiB | 73.0 / 12.5 KiB |

  Screenshots, full page, every page at 1280 and 390 px, reduced motion:
  docs/audit/website-w10/<page>-{before,after}-{desktop,390}.jpg (7 pages). Lighthouse 12.8.2
  mobile, private, one run each, before -> after (load average 10-14): perf home 90 -> 88, data
  70 -> 73, reserves 96 -> 96, run 91 -> 91, explorer 94 -> 94, labels 66 -> 75, api 95 -> 94;
  accessibility, best practices, SEO 100 on all 14 before and after; bytes +1 to +4 KiB a page.
  Contrast (tests/contrast.py): 132 pairs (was 99; the new header, glass, hover and good-soft
  grounds added), 0 below AA. W11: the chart frame keeps every tooltip inside its chart (its top
  is clamped by the tooltip's measured height); the pool day tooltip is one line shorter (the
  total in the head line), 220 px wide (all tooltips, was 200) at 12 px, and the pool rows 30 px
  apart (was 26), so a 9-line day fits; a reserve week that ended lower draws its line and dot
  in --btc (the "down / out" colour, 9.73:1 on --panel-solid) and ends in a ▼ triangle, a higher week
  in --line-net with a ▲, the card line reads "Week: ▼ −…" or "▲ +…", the chart's name says
  lower/higher at the end of the week, and the legend explains both; the flow tooltip says
  "Running net" and the series tooltip "Running total" (their legends' words; the series table
  column too); `.demo` -> `.real-tag` (testid `level2-tag`); `public_build.rs` scan() also tries
  every substring that starts at a bech32 hrp (bc1/tb1/bcrt1, any case, to every end in the run)
  or a base58 version character (1/3/m/n/2, lengths 26-35), so addresses packed back to back or
  glued to other text are found (new test: label and unlisted addresses packed in wasm-like
  data are refused, known ones packed together pass; it fails without the split). On the real
  public build the split found 6 addresses the old scan missed, all in the verifier WASM's data
  section: the address-spelling vectors of `BtcAddressCanon` (adapters/source-btc/src/lib.rs:
  BIP-173's example in both cases, the genesis coinbase's, a P2SH and a testnet example, a
  leading-ones case), stored back to back; they are spec examples, not label rows, so the known
  list now also reads `BtcAddressCanon::vectors()` (the guard on the public build: 5.5 s). Browser tests
  (new, tests/w10.mjs, run by e2e.mjs: per width, every computed font size, line height,
  spacing value, radius, shadow and colour on all 7 pages is a token; icons 16/18/22 px in one
  stroke; one fold marker; every control on every page changes on hover and every button when
  pressed (1280 px); transitions run, and stop with the pause button or reduced motion; every
  placeholder named, two looks; tooltips inside every Data chart at both ends and on every pool
  day; reserve trend triangle, colour and name per company; tooltip words equal the legends';
  no `.demo` class): public 462 ok (was 422), private 473 ok (was 433), 0 failed. `ONYX_WEB=1 scripts/check-all.sh` passed.

- 2026-10-15 W6 chart consistency, and W8's first fix (both builds of web-w6 vs ce9a9b7).
  W8: the Treasury and both SEC values were tagged "demo data", but they are real: each
  observer fetched them over HTTPS (the recorded `response_body`). The tag now says "real value
  · test round" (same outlined style), and the copy says the value is the Treasury's real
  published figure for its record date / the companies' real filed figures, fetched from
  fiscaldata.treasury.gov / data.sec.gov on 2026-10-11 (UTC, read from each round's window),
  and that only the round was a test: observers and draw were a local rehearsal. W6 inventory
  (before): 4 chart components, each with its own frame, keys, hover and tooltip (widths
  170/180/190/200 px, radius 10 or 12, shadow or not); BTC ticks from 3 formatters (Flow and
  Series `fmt`, Reserves whole BTC, zero missing on Flow); block heights "970,543" on the block
  chart and "#970,543" elsewhere; no x axis on the flow chart or the Flows-over-time charts; the
  "In ↑ / Out ↓" labels in Plex Sans among mono ticks; the gap bars in a fifth colour
  (--accent-2); the Reserves line amber when the week ended down, cream when up, with a blue dot;
  pool bars with `title` tooltips only (no keyboard); three legend swatch styles (10 px r2, 10 px
  r3, line); the fee-split bar in its own colour order; Run's step bars without a data table;
  blank boxes for empty charts; 5 unnamed loading placeholders on Data and Run. After: one
  `ChartFrame` (one Tab stop, arrow keys, pointer pick, highlight band, one 200 px tooltip, one
  empty message, the data table folded right under) used by the block, flow, series/history,
  reserve and pool charts; `src/lib/chart.ts` holds the one tick format (`tickBtc`: grouped
  whole BTC from 100, one decimal from 1, two below, true minus), "#height" x ticks at the first,
  middle and last point (time or day above when known), a shared left edge (>= 54 px) so stacked
  charts line up, and the colour rule: bars above zero / in --accent, below / out --btc, every
  line --line-net, categories --pool-1..6 in a fixed order (pools and the fee split),
  --pool-other, unknown hatched; `ChartLegend` draws the flow, series, history and reserves
  legends (unit on the right, controls after it); pool and fee legends use the same `.key`
  swatch; the pool bars are a keyboard chart with a day tooltip; Run's two step-bar panels got a
  block axis ("block 0 … block 684") and data tables; every placeholder names what it waits for.
  Screenshots (one chart section per page, desktop at 75 % and 390 px):
  docs/audit/website-w6/{data,reserves,run}-{before,after}-{desktop,390}.jpg. Measured on the
  private build (scratch inventory script: focus each chart, arrow key, read computed styles;
  Chrome 152; Lighthouse 12.8.2 mobile; load average 13-30, so timings are noisy):

  | Data, Reserves, Run | Before | After |
  |---|---|---|
  | Charts on the shared frame / own frame code | 0 / 4 | 5 / 0 |
  | Tooltip styles (computed) | 4 | 1 |
  | Charts with a keyboard tooltip (Data + Reserves) | 14 | 15 (pools) |
  | Axis text styles (font, size, colour) | 3 | 2 (mono 12 px muted; the end value in ink) |
  | BTC tick formatters / height label formats | 3 / 2 | 1 / 1 |
  | Charts without an x axis (Data) | 4 (flow + 3 series) | 0 |
  | SVG mark colours on Data / Reserves (fills and strokes) | 7 / 2 | 6 / 1 |
  | Legend swatch styles | 3 | 2 (square, line) |
  | Data tables Data / Reserves / Run | 11 / 4 / 0 | 11 / 4 / 2 |
  | Unnamed loading placeholders (Data, Reserves, Run) | 5 | 0 |
  | Chart component code (lines) | 1,002 in 5 files | 774 in 8 (+ 160 lines of shared CSS in app.css) |
  | First-load wire, private (tests/audit.mjs) data / reserves / run | 2,208.8 / 2,029.7 / 227.6 KiB | 2,209.5 / 2,031.9 / 229.9 KiB |
  | Lighthouse perf, median, data (3 runs) / reserves (3) / run (5) | 70 / 94 / 89 | 70 / 95 / 82 |
  | Lighthouse TBT, same medians | 1,020 / 160 / 190 ms | 990 / 130 / 360 ms |

  Run's TBT ranged 90-720 ms before and 210-620 ms after across runs as the load moved between
  13 and 30; its JS grew 1.6 KiB (DataTable, chart colours), so the gap is within the noise but
  not proven to be noise. Accessibility, best practices and SEO 100 on all three, before and
  after. Contrast unchanged (the marks use tokens W3 measured; the gap bars moved from --accent-2
  to --accent). Browser tests (new: W8 tag and copy with the fetch date; per page one tooltip
  style on the arrow keys, one axis font, every tick in the shared formats, no bare block
  height, every mark in an app.css token at >= 3:1 on the panel, one swatch set, a data table
  under every chart; Run's step tables equal the run file; fee-split bar and legend colours
  match; pool day tooltip by keyboard; every placeholder named while loading): public 422
  ok (was 395), private 433 ok (was 401), 0 failed. `ONYX_WEB=1 scripts/check-all.sh` passed.

- 2026-10-15 W5 home page clarity, with the home page's share of W10 (both builds of web-w5 vs
  9e0dd44). The hero now says, in one sentence: "Onyx is a contributor-run data network: anyone
  can submit data, others rerun the work to check it, and buyers pay per row and check every
  answer against a root posted on Base Sepolia (testnet)." under the headline "Data anyone can submit, check and
  buy" (was "...check and settle on" and a peer-run/markets line). One primary step, "Verify it
  in your browser" (runs the 7 checks and scrolls to them), one secondary, "See the live run"
  (was "Check it now" / "Explore a shard"). A calm note under them: "Testnet only · private
  preview" in the preview, "Testnet only · early preview" in the public build (it has no
  private-preview banner), then "Payments are test USDC on Base Sepolia. No public endpoint
  yet." Three proof cards lift over the slab's edge: (1) "Full run passed live on Base Sepolia 3
  times", latest date, transactions and earlier dates read from the run files -> Run page (moved
  up from the Milestones section); (2) "Reserves for OKX and BITB matched their own published
  figures exactly" with both totals, address counts, when (OKX at its 2026-09-08 snapshot, block
  965,949) and against whose list -> Reserves page; the figures live in `src/lib/reserves.ts`,
  shared with the Reserves table, and the totals are now in fixtures/reserves/cross-checks.json
  (`theirs_sats`, `onyx_sats`, optional fields on `CrossCheckRecord`; the serving check does not
  read them), which the browser tests compare against the page, so a drift fails; entity names
  only, no label rows, so the public build shows the same card; (3) "Verify in your browser", a
  button that runs the in-browser checks and then says "7 of 7 passed"; running the checks
  (either button or the palette) moves focus to the checks' heading. Cards 1 and 2 are one link
  each: the title is the link and stretches over the card, so the whole card opens the record
  and the spoken name is the title alone (review: the first version read the whole card). Polish: 8 px rhythm in
  the hero, headline 42-68 px on two lines at 1280 px, lede 17-20 px in slab ink, 48 px CTAs
  with press state, cards with display-serif numbers, icon tiles, hover lift and arrow nudge,
  focus ring from app.css, all transitions off under reduced motion or the pause button; below
  860 px the hero drawing is hidden (the console shows the same root check) and below 560 px the
  CTAs are full width. Meta and og descriptions use the same sentence; og.jpg re-rendered from the
  public build. Screenshots (public build, reduced motion, first screen): docs/audit/website-w5/
  home-{before,after}-{desktop,390}.jpg: before, the first screen at 390 px was the headline and
  the block drawing; after, it is the sentence, both steps, the testnet note and the first proof.
  Measured (Chrome 152, Lighthouse 12.8.2 mobile, load average 20-30, so CPU timings are noisy):

  | Home page | Before | After |
  |---|---|---|
  | First-load wire, public (tests/audit.mjs, AUDIT_PAGES=home) | 2,085.8 KiB, 54 requests | 2,087.7 KiB, 55 (JS +1.2, CSS +0.7 KiB) |
  | Lighthouse total bytes, public / private | 2,098 / 3,245 KiB | 2,100 / 3,247 KiB |
  | Lighthouse perf, public, back to back | 71 (TBT 760 ms, LCP 3.6 s) | 73 (TBT 670 ms, LCP 3.6 s) |
  | Lighthouse perf, private (separate runs) | 69 (TBT 840 ms, LCP 3.7 s) | 66 (TBT 980 ms, LCP 3.9 s) |
  | Lighthouse a11y / best practices / SEO, both builds | 100 / 100 / 100 | 100 / 100 / 100 |
  | Run line rendered, median of 3, public, normal / throttled | 1,008 / 3,809 ms | 318 / 2,647 ms |
  | Same, private | 1,168 / 3,176 ms | 718 / 3,452 ms |
  | Run proof's distance from the top, 1280 / 390 px | 4,434 / 6,345 px | 758 / 770 px |
  | Reserves proof card's top, 1280 / 390 px | none | 683 / 987 px |
  | Page height at 390 px / Tab stops (public, audit) | 7,110 px / 24 | 7,694 px / 26 (at c97e7bb) |

  Bytes, scores and timings were measured at c97e7bb, before the review's copy and markup fixes;
  positions and heights after them. Bytes and scores are unchanged within noise (no data or script moved; W4 owns the 1.8 MiB
  verifier WASM). "Run line rendered" = navigation to `[data-testid=testnet-headline]` in the
  DOM (the same element before and after; scratch script, not committed). Phone 390 px: 0
  controls under 44 px, 0 text under 12 px, no side scroll. Browser tests (new: headline and
  sentence, the sentence and meta description name Base Sepolia (testnet), testnet note visible
  on the first screen, each proof link named by its title only, OKX and BITB figures equal to
  fixtures/reserves/cross-checks.json on the home card and the Reserves table, focus moves to the
  checks, one filled and one ghost CTA, proof 1 text
  from the run files, proof links resolve to #/run and #/reserves and open those pages, proof 2's
  figures equal the Reserves table's, proof 3 runs 7 of 7, no label address on the home page,
  public build fetches no recipe input, 3 cards in a row at 1280 px and one column at 390 px):
  public 395 ok (was 372), private 401 ok (was 379), 0 failed. `tests/audit.mjs` takes
  AUDIT_PAGES and `tests/bench.sh` takes PAGES to measure one page.

- 2026-10-14 W3 accessibility, WCAG 2.2 AA (both builds of web-w3 vs d7f6547; axe-core 4.14.0
  via `tests/a11y.mjs`: every page at 1280 and 390 px, as loaded and with every `<details>`
  open, tags wcag2a/2aa/21a/21aa/22aa + best-practice; Chrome 152). Decision: **dark only**,
  written in app.css: one palette drawn for one ground, every text pair >= 5.72:1 and every
  chart fill >= 3:1; no prefers-color-scheme light theme (W6 "both themes" means this one).
  Changes: `--pool-other` #5f5d58 -> #75726b (unknown stays hatched, so the two neutral shares
  differ by pattern, not only tone); every full chart (block, flow, series, reserve) is one
  Tab stop with a name, Left/Right/Home/End move its tooltip and Esc hides it, and a "Data table"
  fold under it holds the same numbers (built only when opened; stacks on phones); the pool
  bars got the same table (per day, per pool, total); the compact overview charts inside a link
  are images with a summary; header search button named by its words ("Search pages and
  actions ⌘K", aria-keyshortcuts) instead of a mismatched aria-label; a header pause button
  (aria-pressed, remembered) stops the stone slab, the live-block strip and the beams
  (WCAG 2.2.2), and reduced motion or the button turns off every CSS animation and transition,
  the count-up, the smooth scroll, the hero tilt and page transitions; the preview banner is a
  labelled `aside`; scrolling boxes (API sample, label list, block table, data tables) are named
  focusable regions; API Request/Response buttons say which is shown (aria-pressed); command
  palette options are plain listbox options (no button inside), the field shows focus, the
  active option has an edge bar; BIP-322 vector dots, live blocks and the hero seal say their
  result in words; `scroll-padding-top` keeps focus clear of the sticky header (2.4.11).
  Already right and now tested: `lang="en"`, one header/nav/main/footer, one h1 first and no
  skipped level on all 7 pages, every drawing named or hidden, every field labelled.
  axe violations (nodes; 7 pages x as loaded / folds open), before -> after:

  | Build | 1280 px | 390 px | Rules (nodes, both widths and states) | Palette open | Menu open |
  |---|---|---|---|---|---|
  | public | 8 / 8 -> 0 / 0 | 1 / 1 -> 0 / 0 | label-content-name-mismatch 14, scrollable-region-focusable 4 (all serious) | 9 -> 0 (nested-interactive) | 1 -> 0 |
  | private | 15 / 16 -> 0 / 0 | 8 / 9 -> 0 / 0 | region 28 (moderate), label-content-name-mismatch 14, scrollable-region-focusable 6 | 9 -> 0 | 2 -> 0 |

  Not seen by axe, before -> after: `--pool-other` on `--panel-solid` / `--panel` / `--bg`
  2.78 / 2.83 / 3.03 -> 3.81 / 3.88 / 4.15:1 (tests/contrast.py: 99 pairs, 2 -> 0 below AA;
  lowest text pair unchanged, `--muted` on the banner 5.72:1); charts whose values a keyboard
  can reach 0 -> all (14 on first load: 10 on Data, 4 on Reserves); data tables beside charts
  1 (Data's block table) -> 16; pause control for auto-moving content none -> header button;
  Tab stops at 1280 px, private (same Tab walk as the test), before -> after: home 23 -> 24,
  data 26 -> 49 (charts and their tables), reserves 14 -> 23, run 46 -> 47, explorer 19 -> 20,
  labels 18 -> 19, api 14 -> 15; stops without a visible ring 1 (API) -> 0; no trap.
  Lighthouse not re-run (W2 measured accessibility 100 on all 14). Not checked: a screen
  reader by ear, Windows high-contrast (forced colours). Browser tests (axe at 1280 and 390 px
  on every page, folds open, palette and menu open, failing on any serious or critical
  violation; landmarks, headings, names; Tab walk with a ring on every stop; chart arrow keys;
  block, pool and reserve tables equal the export; palette contrast from the live stylesheet;
  reduced motion and the pause button): public 372 ok (was 313), private 379 ok (was 320),
  0 failed.
- 2026-10-14 W10 visual polish added; styling items (W5, W6, W10) take priority after W3

- 2026-10-14 W2 phone layout (both builds of web-w2 vs b046886; same method as W1:
  `tests/audit.mjs` at 360/390/430 px and 1280 px keyboard, `tests/bench.sh` Lighthouse 12.8.2
  mobile; load average 30-47, so timings and TBT are noisy single runs). Changes: below 760 px
  the header is logo + search + a 44 px "Menu" button (aria-expanded, Esc closes and refocuses)
  that opens all 7 pages as 48 px rows; a "Skip to content" link is the first Tab stop on every
  page; phone controls (buttons, selects, search, summaries, sliders, copy buttons) are >= 44 x 44;
  `.panel` is one shrinkable column (fixes the Explorer validator card at 360 px); the Run page
  folds each step's transactions behind "Show N transactions" (steps with more than 2), each a
  44 px row named by its function and hash; wide tables (Reserves x2, Data blocks, Labels list)
  stack as label/value rows under 560 px and the API sample wraps; text under 12 px raised to
  12 px (chart axes, hashes, chips, donut caption). Desktop look unchanged.
  Phone, private preview, 360/390/430, before -> after (public build: same, Explorer 16 -> 12
  and Labels 15 -> 11 targets, all 0 under 44 px):

  | Page | Targets | < 44 px | < 24 px | Text < 12 px | Inner side-scrollers |
  |---|---|---|---|---|---|
  | home | 21/21/22 -> 17 | 19/19/20 -> 0 | 9 -> 0 | 14 -> 0 | 1 -> 0 |
  | data | 25/25/26 -> 21 | 22/24/26 -> 1/2/2 | 3/4/6 -> 0 | 112 -> 0 | 2 -> 0 |
  | reserves | 12/12/13 -> 8 | 12/12/13 -> 2/3/4 | 4/4/5 -> 0 | 28 -> 0 | 3 -> 0 |
  | run | 244/244/245 -> 38 | 239/239/240 -> 1/0/1 | 230/231/232 -> 0 | 0 -> 0 | 1 -> 0 |
  | explorer | 22/22/23 -> 18 | 22/22/23 -> 0 | 8 -> 0 | 103 -> 0 | 1 -> 0 |
  | labels | 17/17/18 -> 13 | 17/17/18 -> 0 | 6 -> 0 | 4 -> 0 | 2 -> 0 |
  | api | 11/11/12 -> 7 | 10/11/12 -> 0 | 1 -> 0 | 5 -> 0 | 2 -> 0 |

  The targets still under 44 px are links inside sentences (WCAG 2.5.8 inline exception; given
  5 px extra hit area). Page side scroll 0 before and after at every width. Run page Tab stops
  247 -> 46 (1280 px) and 244 -> 38 (390 px); Lighthouse label-content-name-mismatch on Run
  215 -> 0. Skip link: none -> first stop on all 7 pages. Run page height at 390 px 13,284 ->
  11,471 px; Reserves 5,350 -> 6,050 (stacked tables). Lighthouse mobile perf, public / private,
  before -> after: home 81->80 / 67->76, data 75->69 / 67->75, reserves 90->87 / 82->91, run
  86->82 / 74->76, explorer 88->75 / 83->93, labels 90->85 / 61->69, api 86->90 / 90->88 (noise:
  no script or data changed); accessibility, best practices, SEO 100 on all 14 before and after.
  Browser tests (W2 checks at 390 px on every page: no side scroll, nothing past the edge, no
  inner side-scroller, all primary controls >= 44 x 44, no text < 12 px, skip link first and
  moves focus, the menu lists all 7 pages and opens the next; Esc; desktop nav; Run folding,
  Tab stops, 44 px rows and names): public 313 ok (was 257), private 320 ok (was 264), 0 failed.
- 2026-10-14 W7: the 2026-10-14 Base Sepolia run on the Run page (scripts/build-testnet-run.py:
  14 steps, 198 transactions sent, 215 basescan tx links, reserve round 3 settled and one OKX
  row sold for 2,500 micro-USDC behind serving check btc-entity-reserve-v1, Binance 403); three
  runs, latest first; picker = buttons for the newest 3 plus a dropdown of every run once there
  are more, dropdown only on phones (44 px tall); home headline "Full run passed live on Base
  Sepolia 3 times — latest 2026-10-14: reserve data sold on chain, 198 transactions"; API page
  no longer says entity-reserve was served only locally. The parser now accepts step 6a's
  Finalized block up to 5 blocks before step 5's head read (the live chain gave 48113186 vs
  48113187). Browser tests: public 257 ok (was 233), private 264 ok (was 240), 0 failed.
- 2026-10-14 W1: baseline in docs/audit/website-baseline.md (both builds of 793eee8, Chrome 152,
  Lighthouse 12.8.2 mobile). First-visit wire (gzip): 2.0 MiB on every page but Run (210 KiB),
  3.1 MiB on Home/Explorer/Labels in the private preview (recipe input); the verifier WASM is
  1.79 MiB of it. Lighthouse performance 69-94 (public) / 73-94 (private); accessibility, best
  practices, SEO 100 everywhere; TBT up to 1,290 ms (Data). Contrast: 99 palette pairs, text
  all >= 5.72:1, 2 failures (`--pool-other` chart fill 2.78-2.83:1 on panels). Keyboard: focus
  ring on every stop, no trap, no skip link, ~200 stops on Run. Phone: no sideways scroll; nav
  hides Explorer/Labels/API; most tap targets < 44 px (186-188 < 24 px on Run); Explorer card
  overflow at 360 px. Top 10 issues ranked in the report.
- 2026-10-14 plan created at the project owner's request ("lets do all this")
