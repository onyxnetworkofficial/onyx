# Sector: Cypher, private pooled data (stub)

## Scope

Contributors pool private data (for example trading desks' flow data) to produce
aggregate metrics, without revealing their individual rows.

## Claim model fit (tentative)

- Commitments still work: a contributor commits to a Merkle root over its private rows; the rows stay offchain and private.
- Aggregates are published as claims; individual rows are never served.
- Verification is the hard part: validators cannot rerun a recipe on data they cannot see.

## Open questions

- Verification method: TEE, MPC, ZK proof over committed rows, or audited sampling? Each needs an ADR and likely a new `VerificationMethod` variant (core change → ADR first).
- Privacy of aggregates: minimum contributor count, noise, or both?
- Rights: contributors must grant redistribution of aggregates but not rows; does `DataRights` need a separate field for derived outputs?
- Who may join the pool, and how a leaking contributor is detected.
