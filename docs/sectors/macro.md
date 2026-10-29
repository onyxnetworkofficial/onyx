# Sector: macro (stub)

## Scope

Official economic statistics: central bank rates, inflation, employment,
money supply. Fed data is the first candidate.

## Claim model fit

- `VerificationMethod::OfficialSourceMatch { official }`: the value must equal what
  the official publisher released. The release is pinned as
  `Snapshot::Document { published_unix, content_hash }`.
- Revisions: a revised release is a new document snapshot and a new claim; the old claim stays valid for its snapshot.
- Values: `Value::Fixed` for rates and percentages (no floats), `Value::Int` for counts.
- Subjects: `Subject::Instrument("series:...")`.
- Where an official source is unavailable, `CrossSourceQuorum` across independent redistributable mirrors.

## Rights issues

- Aggregators such as FRED carry series from many publishers, and series can have their own terms. Check per series before ingesting. Do not assume a whole aggregator is open.
- Record license and attribution per series in `DataRights`.

## Open questions

- First series to support, and its verified license?
- How to pin a release when the publisher's file changes format or URL.
