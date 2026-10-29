# Governance

## Scope

Who may add or retire recipes, label sources, metrics, sectors and contract parameters.

## Current state (M0)

Prototype. The project owner and the second signer decide. Every decision is an ADR in [../adr/](../adr/).

## Plan

- Prototype (testnet): admin actions (register recipe as a metric's current version, allowlist a label source, change round parameters) by the project owner + the second signer, moving to a 2-of-2 multisig on Base Sepolia in M6.
- Recipes: anyone may register a recipe id (it is just content-addressed code). Making it the **current** recipe for a metric needs: open source, golden tests, determinism tests pass, review by a second person.
- Label sources: allowlisted with a stated rights basis ([legal-data-rights.md](legal-data-rights.md)). First-party attestations (BIP-322, EIP-191/712) are accepted from any entity that proves control.
- Disputes: objective disputes settle by deterministic recompute only. **Never by token vote.** Subjective claims are scored by calibration against later ground truth, not by agreement votes.
- Later: a published process (proposal, review period, multisig execution). Any token-holder role is advisory for subjective policy only.

## Registry (ADR-0012)
The registry (entities, sources with rights, address canons, aggregates, instrument namespaces,
metrics) decides what may be ingested and named. Rules:
1. Entities: organisations only (exchanges, ETFs, funds, protocols, custodians, issuers, mining
   pools, public treasuries). Each entry needs public evidence that the entity is an organisation
   (filing, company register, official site). No natural persons, ever.
2. Sources: registered with reviewed `DataRights`. A source whose license forbids redistribution
   cannot be registered (enforced in code). Rights changes are new entries in the log, never edits.
3. Canons: only from the fixed list of reviewed sector adapters, each with a versioned `canon_id`.
3a. Proof rules (`AddressProof`, ADR-0015): same as canons, one per namespace, versioned `proof_id`.
3b. Entity keys (ADR-0015): a 20-byte signing key is added only with evidence that the entity itself
   published it on a channel it controls (its official domain over HTTPS, a regulatory filing, or a
   signed message from an already registered key of that entity). The evidence hash goes in the log.
   Removing a compromised key is a new log entry; attestations it signed stop verifying at the next root.
4. Every change is appended to a registry log with the editor, date, evidence hash and the new
   `Registry::root()`. Every `ShardSpec` pins the root it is judged against, so a change never
   applies to a round already open.
5. Prototype: the project owner and the second signer approve changes. Later: a multisig with a public log.

## Open questions

- Multisig signers beyond the project owner and the second signer before mainnet?
- How a third party proposes a new sector: open PR process, or invitation only at first?
