# onyx-source-page: example level-2 fetcher

Reads one value from a JSON page by JSON pointer, for proof level 2 ("re-observed",
ADR-0027, docs/spec/proof-ladder.md). The fetcher only turns page bytes into an answer;
each observer does its own HTTPS GET.

- `fixtures/example-page.json` and `fixtures/example-page-later.json` are synthetic pages
  written for tests. They are not real data and name no real source.
- Tests: `cargo test -p onyx-source-page` (five observers settle a price, a liar is slashed;
  floats, missing selectors and non-JSON pages answer `Unavailable`).

## Plugging in a real public source

1. Check the source's terms before anything else. Onyx never ingests a source whose terms
   forbid redistribution (hard limit). If the terms are unclear, do not add the source.
2. Pick a page that needs no account, no API key and no payment, and that serves the same
   bytes to every client for the value you read (no personalisation).
3. Write an `ObservationSpec`: `fetcher = keccak256("onyx-fetcher:json-pointer:v1")`,
   `url` (https only), `selector` (a JSON pointer), `kind` (exact or numeric with a scale and
   tolerance), a short window, `min_observers` of at least 3, and the source's `DataRights`
   (reviewed license id, `Allowed` or `Restricted` with a terms id).
4. Observers fetch inside the window, run `onyx_core::observe` with this fetcher, commit,
   then reveal (`onyx_coordinator::observe`).

The first real source is `adapters/source-fiscaldata` (U.S. Treasury Fiscal Data, rights reviewed in its `fixtures/PROVENANCE.md`).
