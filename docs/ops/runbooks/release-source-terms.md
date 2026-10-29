# Runbook: re-read every served source's terms before a release (L15 d, LC-27)

**Trigger:** before every release of the website or the API, and before ticking launch gate
LC-27. THREATS L2-SRC-4: a source can change its terms; we re-read them before we ship again.

**Who:** the loop runs the check and reads the pages it can; a page our fetch cannot read needs a
human (the project owner). A changed term that restricts redistribution goes to the project owner before the release.

**Time:** about 2 minutes when every row is fresh; about 5 minutes per page to re-read.

## Steps

1. Run the check from the repository root:

   ```sh
   python3 -I scripts/check-source-terms.py
   ```

   Expected when all is fresh: one `ok` line per served source, then
   `source terms ok: N served sources`, exit 0. On 2026-10-16 it printed `UNREAD S8` (Binance's
   site terms were never readable by fetch) and `FAIL S4` (label set v2 includes S8), exit 1.

2. For each `STALE` or `UNREAD` line, open the link in a browser (a headless fetch often gets
   no text from script-rendered pages; never record a page you did not read).

3. Compare what the page says with the quote in
   [data-sources.md section 1](../data-sources.md#1-current-sources) for that source.
   - Same: go to step 4.
   - Changed, still allows what Onyx does: update the quote (with the page's own "last updated"
     date if it shows one) and go to step 4.
   - Changed, now forbids copying, redistribution or derived works, or you cannot tell: do not
     release. Stop serving the source's shards ([stop-serving-shard.md](stop-serving-shard.md)),
     set its decision to **The project owner** or **blocked** in section 1, add it to section 4, and tell
     the project owner.

4. In [data-sources.md section 5](../data-sources.md#5-release-step-served-sources-terms-lc-27-threats-l2-src-4),
   set that row's **Last read** to today (`YYYY-MM-DD`). For an `UNREAD` row, also replace
   `not recorded` with the URL you read.

5. Run step 1 again. Repeat until it exits 0.

6. Commit the changed `data-sources.md` with the release, and name the run in the release note
   (for example "source terms re-read 2026-10-16, exit 0").

## Verify

- `python3 -I scripts/check-source-terms.py; echo $?` prints `0`.
- `scripts/check-docs.sh` passes (it runs the table's shape check and the script's self-test).

## Rollback

Nothing to roll back: the step changes only `data-sources.md`. If a source was stopped in step 3
and the project owner later clears it, serve it again by the rollback in
[stop-serving-shard.md](stop-serving-shard.md#rollback-serve-it-again).

## Notes

- Threshold: 90 days, written in data-sources.md section 5 ("Re-read threshold") and read by the
  script from there. No document set one before; the choice is logged in docs/TRACKER.md
  (decision log, 2026-10-16).
- A new source: add its row to section 1 and section 5 together, or `scripts/check-docs.sh` fails.
- Not part of `scripts/check-all.sh` on purpose: a date going stale would turn every unrelated
  commit red. check-all runs only `--lint` and `--self-test`.
