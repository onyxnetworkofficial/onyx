# Spec: labels

## Scope
Subjective lane for "address X belongs to entity Y": label set format, versioning, first-party
attestations, calibration scoring, and the spread of metrics across label sets. Built in M2.
Types fixed in M0: `crates/onyx-labels`.

## Interfaces
- `LabelSet { name, version, labels }`, labels strictly sorted by address, one per address.
- `LabelSet::validate(&dyn Registry)`: every address in its namespace's canonical spelling, checked
  by an `AddressCanon` from the sector adapter (`BtcAddressCanon`: mainnet only, bech32 lowercase,
  base58 as is). Unknown namespaces are rejected. Every entity must be registered with the same kind (ADR-0012).
- `Label { address, entity, kind: EntityKind, confidence (ppm), evidence }`.
- `EntityKind`: Exchange, Etf, Fund, Protocol, Custodian, Issuer, MiningPool, Treasury. No person kind.
- `LabelSet::content_hash()`; claims pin `LabelSetRef { name, version, content_hash }`.
- First-party attestation (ADR-0015, `onyx_labels::attest`): `Attestation { statement, address_sig,
  entity_key, entity_sig }`. Both signatures are over the exact statement bytes:
  `Onyx address attestation v1\ndomain: <network id>\nentity: <entity id>\naddress: <ns:body>\nvalid_from: <height>\nexpires: <height>\nnonce: <32 hex>`.
  The address signature is checked by the namespace's registered `AddressProof` (BTC: BIP-322 `smp`/`ful`,
  `Bip322Proof`); the entity signature is EIP-191 by a key the registry lists for the entity.
  `Attestation::verify(ctx, registry)` judges validity over `ctx.ranges` (the shard's block range,
  derived by `assemble_input` from the spec) and refuses revoked addresses (`Registry::revoked_from`).
  The network domain is `Registry::domain()`.
- Revocation (`onyx_labels::revoke`): `Onyx address revocation v1` statement with domain, entity,
  address, effective height and nonce, EIP-191 signed by a registered entity key; recorded in the
  registry by `apply_revocation` with its key and hash; `remove_entity_key` keeps them unless
  governance lists them by hash (L14-F5).
- `LabelSet::check_attestations(atts, ctx, registry)`: every label citing evidence kind `attestation`
  must have a valid attestation for exactly its address and entity; full confidence requires one.
  Strict, for publishers. The validator path (`assemble_input`) uses `attested_subset`, which drops
  failing labels and keeps the rest. Caps: 8 evidence entries and 2 attestations per label, 2^16
  cited attestations and 16 MiB of them per check.
- `LabelSet::check_servable(resolves)`: refuses sets named `test.*`, labels without evidence, and
  evidence hashes the store cannot resolve (R-LBL-FIX-1). Wired into delivery in M7.
- Calibration (ADR-0016, `onyx_labels::score`): `score_all(forecasts, attestations_with_rounds, registry, registry_root)`
  scores each forecast as a distribution over the K registered entities (Brier primary, clamped log
  reported); forecasts count only if committed before the truth was published, not by the owner's
  own key, and at most 10 per owner per contributor.
- Versions (ADR-0017, `onyx_labels::version`): `VersionLog::append(set, round)` requires the next
  version number, the same name, new content and a later round; each `Manifest` names the content
  hash it supersedes. `diff(old, new)` gives added, removed and changed labels.
- Spread (ADR-0017, `onyx_labels::spread`): `spread(&[SetValues])` over at least 2 distinct sets
  (ordered by content hash) gives per-key values per set plus min and max.
- Dated label sets (ADR-0037, format 2, `onyx_labels::dated`): `DatedLabelSet { name, version,
  published, members }`, `Membership { label, from, to: Option<u64> }` (block heights, inclusive;
  `None`: still listed at `published`). `content_hash()` commits a Merkle root over the Borsh
  memberships (`member_proof`, `verify_membership`); `window(f, t)` is the plain `LabelSet` a shard
  over blocks `f..=t` runs; `check_successor(prev)` is the version rule; `from_undated(set)` reads a
  format-1 set as valid at every height.

## Rules
1. Labels name organisations only. Requests to label a person are refused (rights.md).
2. A first-party attestation is top-confidence evidence, but it can be revoked by the entity.
3. Contributors are scored on calibration against later ground truth, not on agreeing with each other.
   Agreement voting rewards copying (RESEARCH.md: Bittensor weight copying).
4. Dated membership (ADR-0037, THREATS RES-2):
   - R-DATED-1: members strictly ascending by `(address, from)`; two ranges of one address never
     overlap, whatever the entities (an address is never in two entities at once); adjacent ranges
     with the same label are one range; nothing starts after `published`, no closed range ends at
     or after it.
   - R-DATED-2: `from` is the first block at or after the source's dated publication that first
     lists the address for the entity; `to` is the block before the first publication that no
     longer lists it. No range is set from any other date.
   - R-DATED-3 (window): a shard over `f..=t` runs exactly the members valid at every height of the
     range; a membership change inside the range is refused (`ChangesInside`), so shards start at
     every change.
   - R-DATED-4 (no back-dating): version n+1 keeps version n's memberships at every height up to
     n's `published`; it may close an open range at or after it and add ranges that start after it.

## Indexer bridge (ADR-0031)
Fixture label sets are built from the project owner's the entity indexer by reproducible converters
(`scripts/build-label-set-v2.py`, `scripts/build-label-set-v3.py`). Rules:
- R-IDX-1: only seed rows a company itself signed or published are imported (signed PoR,
  exchange-published PoR lists, ETF issuer lists, custodian reserve lists). Cluster expansions,
  `inferred` rows, third-party (`*_thirdparty`) attribution, mining-pool payouts and filings
  labels are never imported. Filed addresses are a deny list only.
- R-IDX-2: a source is imported only once its redistribution terms are read and recorded (the
  converter's `CLEARED` list; `EXCLUDED` lists refused sources with reasons, and the build stops if
  one is listed as cleared). Today no source beyond v2 is cleared, so no v3 fixture ships.
- R-IDX-3: proof level per label: signed rows are level 3, published lists are claims (level 4
  on main; `Unstaked` when level 4 returns with bonds). A shard's level is the weakest input.
- R-IDX-4: a cleared custodial reserve list is labelled with the product it backs (kind
  `Custodian`), never as the custodian's own holdings. wBTC's list is excluded (its site's terms
  forbid redistribution); cbBTC's waits on a rights review.
- R-IDX-5: each new set keeps every label of the previous set unchanged (kind and confidence),
  and earlier sets stay byte-identical (their goldens and pins keep reproducing). Exception: a
  label removed on request (docs/ops/label-removal.md) is left out of the next set.

## Removal and privacy
Old label set versions are kept in restricted storage until every root that pins them is past
its challenge window, so a removal request cannot kill a dispute (R-REMOVE-1). Privacy screening
happens before publication, because published rows with proofs cannot be recalled.

## Assumptions
- Dated publications (a PoR snapshot, an issuer page capture) carry a date the source states, and
  the first block at or after that date is the membership's first height (ADR-0037). The mapping
  from a date to a height uses block timestamps from a header chain checked for proof of work.
- Entities will publish one Onyx signing key (an EVM-style address) through their own channels, and
  governance registers it. Without it, an entity's labels stay below full confidence.
- BTC exchanges can sign from P2WPKH, P2TR key path, or multisig (P2WSH, P2SH, P2SH-P2WSH) addresses.
  Time-lock and other script-path addresses cannot attest yet.
- Enough ground truth will arrive (attestations, disclosures) to score contributors.
- Entities will sign if it gets them better-quality public numbers.

## Open questions
- Dated set v3 of `test.onyx.entities.btc` from the sources' snapshot archives (ROADMAP D10): the
  per-row first and last publication dates, and whether a source's date is its publication or its
  reserve snapshot time.
- Scoring rule: decided in ADR-0016 (Brier primary, clamped log reported). The project owner may override (PLAN q4).
- Selection bias: a contributor who labels only obvious addresses scores well. Fix planned with
  validator-assigned canaries (M4).
- Which label sets a published spread must use (publishing policy, M7).
- Label meaning when custody and ownership differ (an ETF's coins at a custodian): a set must say
  which one it labels; scoring assumes one owner per address.
- Label set governance: who may publish a new version (governance doc).
