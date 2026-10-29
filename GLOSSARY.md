# Glossary

**Adapter (source adapter).** A crate under `adapters/` that turns a source snapshot into canonical recipe input. Implements `onyx_core::SourceAdapter`. Core crates never depend on adapters.

**Attestation (first-party).** An entity signs a message with an address's key to prove it controls that address. Top-confidence label evidence. `VerificationMethod::FirstPartyAttestation`.

**BIP-322.** Bitcoin standard for signing a message with any address type. Used for Bitcoin first-party attestations.

**Bond.** Funds locked by a miner (with a commitment) or a challenger (with a challenge). Lost on a losing outcome, returned on a draw.

**Borsh.** The canonical binary serialization Onyx uses for rows and types ([ADR-0002](docs/adr/0002-serialization.md)).

**Brier score.** Squared error between a forecast probability and the outcome (0 or 1). Lower is better. A proper scoring rule used for label calibration.

**Calibration.** A labeler is calibrated when things they call 80% likely are true about 80% of the time. Onyx rewards calibration, not agreement.

**Canary.** A task with a known answer mixed into a validator's work. Failing it shows the validator did not really check.

**Canonical encoding.** Exactly one byte string per value, so equal values hash equally.

**Challenge window.** Time after reveal and validation when anyone may post a bonded challenge before a root is final.

**Claim.** One value about one subject, with its source snapshot, verification method, label set (if used), rights and evidence. `onyx_core::Claim`.

**Commit-reveal.** Miners first post a hash of their result plus a secret salt, then reveal both. Stops miners copying each other.

**Commitment.** The finalized record of a batch: Merkle root, leaf count, recipe id, label set, source snapshot, schema id. `onyx_core::Commitment`.

**Confidence.** Probability in parts per million (0 to 1,000,000). Required on subjective claims, forbidden on objective claims.

**Data rights.** License, redistribution status (Allowed, Restricted, Forbidden), source, attribution and terms. Forbidden data is never ingested.

**Draw.** Dispute outcome for a determinism bug or version mismatch: nobody is slashed, bonds come back.

**EIP-191 / EIP-712.** Ethereum message signing standards (plain message / typed structured data). Used for EVM first-party attestations.

**Entity.** An organisation: exchange, ETF, fund, protocol, custodian, issuer, mining pool, treasury. Never an individual.

**Fuel.** Wasmtime's deterministic step budget. A recipe that runs out of fuel fails the same way on every machine.

**Label.** "This address belongs to this entity", with confidence and evidence.

**Label set.** A versioned, content-hashed list of labels, sorted by address. Every entity metric names the label set version it used.

**Lane.** Objective or subjective. Derived from the verification method.

**Leaf.** `keccak256(0x00 || row bytes)`, one per row in the Merkle tree.

**Log score.** Logarithm of the probability given to the outcome that happened. A proper scoring rule.

**Merkle root.** The single hash committing to all rows and the row count ([ADR-0003](docs/adr/0003-hash-and-merkle.md)). Each row has a proof against it.

**Miner.** Runs recipes on source data and submits results by commit-reveal.

**Objective lane.** Claims anyone can check by rerunning open code or matching open sources. Disputes settle by recompute.

**Proper scoring rule.** A score that is best in expectation when you report your true belief.

**Recipe.** Open, deterministic code compiled to WASM that computes a metric from source input.

**Recipe id.** Keccak-256 of the exact WASM bytes. A changed recipe is a new id.

**Round.** One cycle: commit, reveal, validate, challenge, finalized.

**Schema id.** Hash of a row layout descriptor.

**Shard.** A slice of a round's work (for example one entity or one block range), committed separately.

**Slash.** Taking part of a bond as a penalty for a proven fault.

**Snapshot.** The exact input state: a block (height + hash) or a document (publication time + content hash).

**Spread.** The range of a metric across label set versions, published as a confidence band.

**Subjective lane.** Claims that cannot be proven, such as wallet ownership. Versioned, with evidence and a scored confidence.

**Time decay.** Fresh data is paid; data older than a configured window becomes free (only if its rights allow).

**Track record.** Proof level 5: per source and metric, how the source's earlier statements turned out against later level 1 or 3 answers for the same facts, never against other sources. Information only: it never raises a value's level ([ADR-0036](docs/adr/0036-proof-ladder-level-5-track-record.md)).

**Validator.** Reruns a random sample of miner work, answers canaries, and records evidence.

**x402.** HTTP 402 pay-per-call protocol. Client pays (testnet USDC), a facilitator verifies and settles, the server returns data.
