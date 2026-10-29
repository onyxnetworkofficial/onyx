# onyx-source-edgar

Level-2 source (ADR-0027 amendment for period subjects): the crypto-asset units a company states in
its SEC filings (XBRL `us-gaap:CryptoAssetNumberOfUnits`), read from EDGAR's `companyconcept` API by
a pinned fetcher. A fact is "the filing for period P states N units", never a statement about
addresses. Rights and the recording: `fixtures/PROVENANCE.md`.

- Rerun the recorded rounds and write the website summary:
  `cargo run -p onyx-source-edgar --example level2-round -- --book /var/lib/onyx/edgar-book.json --out apps/web/static/level2/sec-holdings.json`
  (replays the recorded draws; `scripts/level2-rehearsal.sh` runs it on a local anvil with real
  on-chain draws, L2RND-R1)
- Also write one query-API results store per company (`ONYX_OBSERVATIONS`):
  add `--store-dir DIR`.
- Receipt key (ROADMAP B3): `--receipt-key FILE` (0600) signs reveal receipts with the
  deployment's own hot key; without it only a replay or a local anvil runs, on the public test
  key; the run refuses a key that is also `--coordinator`, `--owner`, `--reporter` or a pool
  member.
  Runners that share a coordinator address share its receipt key (THREATS B2-R1-L1): two keys
  on one coordinator can swap the signer between a `setReceiptSigner` and the next draw.
- EDGAR observers stay rehearsal-only (recorded pages, simulated observers) until an operator
  role contact exists for the User-Agent (THREATS SEC-4); independent observer processes
  (ROADMAP B2) exist for the Treasury feed (`onyx-source-fiscaldata`).

**Operator warning: the round book is durable state.** Keep the `--book` file (and the
results stores, `--store-dir`) on durable, backed-up storage, with one runner per book (`<book>.lock`
enforces it on one host). Never run a fact that has a draw in progress with a fresh `--book`
path, on a new host, or on ephemeral storage (a container without a volume): a book lost after
its round's receipts are on chain VOIDS that fact for good (every client refuses it; THREATS
REVLOG-R5-1d, REVLOG-R9-F3). A book lost before that point is handled from the chain and the
results store (its draw adopted, followed or given up; REVLOG-R8-2, REVLOG-R9-F1); restoring an older backup can stop the fact until the
latest book is back.
**Results store path:** a missing `--store-dir` file stops the run; pass `--new-store` only on a fact's first run on a deployment (a mistyped path must never read as "nothing published"; REVLOG-R10-3). A run refuses `--new-store` once the chain shows draws of the fact and the book holds none (REVLOG-R11-3). A run refuses a store naming another chain, registry or registry deploy block (`onchain`), or naming none while it has a chain (REVLOG-R10-1, R11-1); tag a store written before that check with `--tag-store` (a mode of its own: it checks every draw the store publishes against the chain's `drawOf`, tags only on a full match, and exits; REVLOG-R11-2), then run again without it.
