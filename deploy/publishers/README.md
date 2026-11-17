# The public record of publisher decisions (test network)

One folder per registered publisher and one per application that is not approved (ADR-0066 item 9;
how a decision is made: docs/ops/publisher-review.md). Written by `book-reviewer`, checked by
`scripts/check-publishers.py`, which also writes the status list `apps/web/static/publishers.json`.

| Folder | Holds |
|---|---|
| `<publisher id>/decision.json` | A registered publisher: a JSON list, oldest first. `approved` (with the application's Keccak and file, the id, origin, key, keys document, `yes` for the checklist lines 7, 8, 9, 10, 11 and 13 (no free text; line 12, the operator grouping, is not public), the limits, the `registerKey` transaction and the relay of the sample's report), later `paused`, `retired`, and each signed request that was taken, with its `sequence` |
| `<publisher id>/application.json` | The approved application's bytes (`application-2.json` for a further key) |
| `application-<16 hex>/decision.json` | An application that is not approved, named by the first 16 hex digits of its Keccak: `received`, `refused` (the reason code and, from the tool's report, the blocked `signer` with `blocks_signer_until`), `withdrawn` (the request's `sequence`), `expired` (the `signer` and its sample's root). No id, no origin, no data text, no sample |

`registered-before-record` is the state of a key the book held before this record began: the id,
the origin, the key and the keys document's Keccak as the book's `KeyRegistered` event has them,
with its transaction and block. No application and no checklist answers exist for these.

In the status list an approval reads `approved (record)` until a run of the checker with `--rpc`
found its registration in the book; then `approved`.

The book stays the source of truth for a key. The record adds what the book cannot hold: why.
