# Sectors

Onyx starts crypto only. Every other sector fits the same claim model
(`crates/onyx-core`) through new adapters and recipes, never core changes
([ADR-0010](../adr/0010-crate-boundaries.md), [onboarding checklist](../ops/onboarding-sector.md)).

| Sector | Status | Main verification methods |
|---|---|---|
| [crypto.md](crypto.md) | Active (BTC first) | Recompute, FirstPartyAttestation, CalibratedJudgment |
| [equities.md](equities.md) | Stub | OfficialSourceMatch, CrossSourceQuorum |
| [macro.md](macro.md) | Stub | OfficialSourceMatch |
| [commodities.md](commodities.md) | Stub | CrossSourceQuorum, OfficialSourceMatch |
| [prediction-markets.md](prediction-markets.md) | Stub | Any method; delivery via settlement endpoint |
| [cypher.md](cypher.md) | Stub | Open question |
