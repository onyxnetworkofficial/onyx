# Spec: claims and lanes

## Scope
The sector-agnostic unit of data. Covers the `Claim` type, the two lanes, verification
methods, values and validation rules. Types: `crates/onyx-core/src/claim.rs`.

## Model
A claim says: metric `M` for subject `S` has value `V` at source snapshot `T`, checkable by
method `X`, computed with label set `L` (if labels were used), and servable under rights `R`.

Lane and proof level (proof-ladder.md) are derived from the method, never stored separately, so
they cannot disagree:

| Method | Lane | Level | Checked by |
|---|---|---|---|
| `Recompute { recipe }` | Objective | 1 | Rerun the recipe on the snapshot, compare bytes |
| `OfficialSourceMatch` | Objective | 4 | Compare with the pinned official document (the claim's own source) |
| `CrossSourceQuorum { sources, threshold, tolerance }` | Objective | 4 | `threshold` of the pinned `SourceRef`s agree within `tolerance` |
| `ReObserved { spec, result }` | Objective | 2 | Settled level-2 round of the feed `spec`; `result` is the round's `result_hash` (proof-ladder.md) |
| `FirstPartyAttestation { scheme }` | Subjective | 3 | Signature by the entity (BIP-322, EIP-191, EIP-712) |
| `CalibratedJudgment` | Subjective | 5 | Scored later against ground truth (labels.md) |

Rows vs claims: a committed batch (one Merkle tree) holds rows of one schema. Each row is the
canonical encoding of a claim body for that schema. The commitment holds the shared fields
(recipe, label set, snapshot, schema).

## Interfaces
- `Claim::validate(&dyn Registry) -> Result<(), ClaimError>`: runs on every ingest path (ADR-0012).
- `Claim::decode(bytes, &dyn Registry)`: the only ingest decode.
- `Claim::lane() -> Lane`.
- `Value` variants: `Int(i128)`, `UInt(u128)`, `Fixed { mantissa, scale }`, `Bool`, `Bytes`. No floats.
- `Subject` variants: `Entity`, `Address`, `Instrument`, `Aggregate`. No natural-person variant.

## Rules (enforced and tested)
1. Objective claims carry no confidence; subjective claims must carry one in 0..=1,000,000 ppm.
2. Quorum: at least 2 sources, each a pinned `SourceRef`; `threshold >= 2` and a strict majority;
   strictly ascending by source id; the claim's own source is one of them; numeric values only;
   every source registered. Tolerance must be 0 until the per-metric policy (M3) defines a cap and a
   unit independent of the claimed value's scale (R-QUORUM-2). The claim's source must carry the most
   restrictive rights in the quorum, and restricted sources with different terms cannot be combined (R-RIGHTS-2).
3. Attestations must have evidence (the signature blob hash).
4. Fixed-point values are normalized; scale ≤ 38.
5. Rights must equal the registry's reviewed rights for the source (rights.md, ADR-0012).
6. Every id follows the ASCII grammars in `onyx_core::ids`. Subjects must be registered: entities
   in the entity registry, addresses in canonical form for a registered namespace, instruments in a
   registered namespace with an uppercase body, aggregates registered (R-SUBJ-1).
7. Evidence strictly ascending; `Value::Bytes` at most 1024 bytes; `Fixed` scale 1..=38.
8. Ingest decodes with `Claim::decode` (decode and validate in one step), except the two
   intake-only methods: `ReObserved` (level 2, `ObservationResults::accept_claim`, ADR-0027) and
   `SignedSource` (level 3, `SignedSources::accept_claim`, ADR-0028), which `Claim::decode`
   refuses because their backing (a settled round, a verified signed report) is checked only there.

## Assumptions
- Every metric fits one of the five methods. A new method is a core change with an ADR.
- Snapshots are either a chain block (height plus hash) or a content-hashed document.
- Subject identifiers are namespaced strings; their registry is out of scope for M0.

## Open questions
- Do we need a "derived" method for metrics computed from other finalized claims (composition)?
- Tolerance for quorum on prices: absolute only, or relative in ppm too?
- Should a claim carry an expiry for subjective values (labels go stale)?
