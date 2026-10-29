# Submission intake and local review (U2)

The node records proposals and serves receipt status. Review approval is not verification or
publication. No submission is automatically added to a public dataset or paid a reward.

## Enable intake

Use the normal testnet/local node configuration in [config.md](config.md). Create a directory
readable only by the operator on a local disk, outside all web roots, then set:

```sh
install -d -m 700 /path/to/private-intake
export ONYX_SUBMISSIONS=/path/to/private-intake/submissions.journal
```

An unset variable disables intake. A missing parent, locked or damaged journal refuses node startup.
On Unix, journal, lock and compaction files are forced to owner-only mode (`0600`), and new samples
are written only to this private operator directory.
The node's existing connection limits, client attribution, deadlines and budgets cover the routes.
The store additionally admits at most 16 outstanding disk workers, including cancelled requests.
The Caddy configuration proxies the two exact POST paths with a 64 KiB cap and receipt GET/HEAD.
No reviewer endpoint exists. Never copy the journal, local review output or uploaded content into
`apps/web/static`, a Git commit, an export bundle or a publicly readable directory.

For a same-origin local frontend against the node:

```sh
VITE_ONYX_SUBMIT_API=1 ONYX_SUBMIT_PROXY=http://127.0.0.1:8080 pnpm --dir apps/web dev --host 127.0.0.1
```

The Vite proxy is development-only. A deployed connected build needs the static frontend and
`/v1/submit` at the same origin; the U5 bundle supplies the deployment handoff. Without the build
flag the page remains an honest offline JSON download. No keys belong in either build variable.

## Review while the node is stopped

1. Stop the node gracefully and wait for its process to exit. A still-running writer holds the
   journal lock; the CLI must fail rather than bypass it. Keep the directory private.
2. Build the operator tool: `cargo build -p onyx-api --example submit-review`.
3. List a page (at most 100 entries):
   `target/debug/examples/submit-review /path/to/private-intake/submissions.journal list`.
   For another page append the last receipt ID. Entries are ordered by ID, not by receipt time.
4. Inspect one receipt using `... JOURNAL show sub_…`. This output contains private proposal data.
   Read the source and licence evidence independently; a submitter's checkbox is not clearance.
5. Decide against the exact revision and content hash from show/list:
   `... JOURNAL decide sub_… 0 0x… approved reviewed`.
   Only an accepted pending route can be approved. A held source cannot be cleared by this command.
   Initial rejections use `rejected` plus `rights_unresolved`, `unsupported_source`, `invalid_content`
   or `duplicate`. To withdraw an approval, use its current revision with `rejected withdrawn`.
6. Restart the node with the same journal path. Check the existing receipt in the frontend.
   Its public response is exactly `{id,status}`; local content and reasons remain private.

## Feed an approved Treasury submission (U4)

With the node still stopped after an approval:

```sh
cargo build -p onyx-source-fiscaldata --example feed-approved
target/debug/examples/feed-approved /path/to/private-intake/submissions.journal sub_… --out /path/to/private-intake/store.json
```

The worker runs the adapter's pinned observer round (recorded fetches and a simulated registry
today) and records the evidence in `submissions.journal.feed.json` beside the journal (owner-only).
A second run returns the recorded evidence and runs nothing. After a restart the receipt's public
status adds `check` (record date, round id, result hash, `registry: simulated`) and the page shows
"Checked by observers". Withdrawing the approval hides it. Keep the ledger with the journal in
backups; a damaged ledger stops the node from starting.

**Live observers** (pre-public loop PP9): for a record date with no recording, run

```sh
scripts/feed-live.sh /path/to/private-intake/submissions.journal sub_… /path/to/new-round-folder
```

It reads the approved record date, runs `scripts/level2-observers.sh` for that date (seven
observer processes with their own keys fetch api.fiscaldata.treasury.gov themselves; the draw is
announced on a local anvil registry), keeps the round's logs and `store.json` in the folder, and
records the evidence with `feed-approved ... --from-store store.json`. The receipt's `registry` is
the chain the store names (`eip155:31337` for the local registry), never a value the operator
types. The local chain is gone after the run, so the draw cannot be rechecked later; the round's
commits and reveals are rechecked from the store. Rehearsed 2026-10-25 for record date 2026-10-22:
the observers' value, 40,444,342,709,389.15 USD, equals the Treasury API's.

The decision command never edits the proposal. Stale revisions/hashes, unsupported transitions,
invalid reason codes and time moving backwards fail. Rejected proposals are terminal; corrected
information requires another receipt. Rejection retains history and does not free admission
capacity. At the default 10,000-submission cap, stop intake and plan an explicit archive/migration;
there is no hidden delete command. Back up the whole private ledger while the node is stopped.

A timeout can occur after persistence: do not automatically retry POST. A contributor should retain
any received ID and check it first. Duplicate content gets another ID and consumes quota; it grants
no reward. IDs are locators, not credentials. The current app offers no ownership/editing authority.

## Verification

- API tests: `cargo test -p onyx-api submit --lib` (concurrency, cancelled handlers, worker cap,
  replay corruption, legacy records, held sources, revision/hash binding and public status).
- Proxy parity: `cargo test -p onyx-node --test caddyfile`.
- Actual node rehearsal: `scripts/level3-book.sh local` also submits, checks the body limit and
  exclusive writer, stops for CLI approval, restarts and reads the same receipt.
- Connected browser rehearsal: `ONYX_SUBMIT_BROWSER=1 scripts/level3-book.sh local` adds an actual
  submission/receipt download/return visit, single-send handling, error states, phone layout and axe.
  Browser screenshots stay in `/tmp`; the temporary node ledger is deleted on exit.

This local run uses Anvil only. It does not establish public hosting, independent verification of
arbitrary data, rights clearance for new sources, production zkTLS, trading or contributor payouts.
