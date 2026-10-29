# onyx-source-signed: example level-3 source

Proof level 3 lane (a), "the source signs it" (ADR-0028, docs/spec/proof-ladder.md "Level 3").
A source publishes its signing keys on its own HTTPS origin and hands out signed reports;
this crate turns those two JSON documents into `SourceKey`s and `SignedReport`s. Verification,
replay and equivocation rules are in `onyx_coordinator::signed`.

- `fixtures/onyx-source-keys.json`: a synthetic keys document, as a source would serve it at
  `https://<origin>/.well-known/onyx-source-keys.json` (RFC 8615).
- `fixtures/example-report.json`: a synthetic signed report.
- Both are made by the test `fixtures_are_reproducible` with a test key derived from the label
  `onyx test signed source key 1`. It is not a real key, holds nothing on any network, and the
  data is invented for tests. They name no real source. Regenerate with
  `ONYX_BLESS=1 cargo test -p onyx-source-signed`.
- Tests: `cargo test -p onyx-source-signed`.

## Formats

Keys document (UTF-8 JSON, no floats):
```json
{"version": 1, "source": "example.signed",
 "keys": [{"signer": "0x<20-byte address>", "scheme": "eip191"}]}
```

Signed report:
```json
{"source": "example.signed", "key": "0x<SourceKey::id>", "metric": "example.signed.reserve",
 "subject": "entity:example", "value": {"uint": "42"}, "observedUnix": 1700000000,
 "sequence": 1, "signature": "0x<65 bytes r||s||v>"}
```
`value` is one of `{"uint": "<decimal>"}`, `{"int": "<decimal>"}`,
`{"fixed": {"mantissa": "<decimal>", "scale": <0..38>}}`, `{"bool": true|false}`,
`{"bytes": "0x<hex>"}`. Numbers are strings, never JSON numbers. The source signs, with EIP-191
`personal_sign`, the text `onyx-signed-source-v1:0x<SignedPayload::digest>`.

## Plugging in a real source

1. Check the source's terms first. Onyx never ingests a source whose terms forbid redistribution
   (hard limit); unclear terms mean no.
2. The source publishes its keys document on its own origin (or a TXT record at
   `_onyx-source-keys.<origin>`). Review: fetch it, confirm the origin is the publisher named in
   the source's reviewed `DataRights`, record the document hash and time (`keys_from_document`).
3. Register each key with `SignedSources::register_key`, a reviewer action: it needs the
   store's `Reviewer` and is logged with the reason (ADR-0028 amendment 3). All keys of one
   source must be published under one origin.

No real source is wired in: picking one is a rights review, logged as a TODO in THREATS (L3-F8).
