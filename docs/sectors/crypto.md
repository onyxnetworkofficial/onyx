# Sector: crypto

## M1 fixture

- Blocks 921080-921082 (`fixtures/btc/`), verified against block headers and Merkle roots;
  prevouts asserted by the public Esplora API (recheck against a node later).
- Label set `test.onyx.entities.btc` v1 (`fixtures/labels/`): 100 OKX proof-of-reserves addresses
  (43 active in the range) and 20 Bitwise BITB issuer addresses. Rights review pending.
- Schema crate `adapters/btc-schema` (no_std): `BtcSource`, `BalanceParams`, `EntityFlowRow`.

## Scope

Public blockchain data, starting with Bitcoin. Later Ethereum, Solana and BNB
Chain, which a vendor already indexes. Two kinds of claims:

- **Metrics** (objective lane): balances, flows, supply, computed by recipes from chain data.
- **Labels** (subjective lane): which addresses belong to which entity.

Every entity metric is `f(source data at block N, label set vK)`.

## Metrics (objective lane, `VerificationMethod::Recompute`)

Bitcoin first:

| Metric id | Subject | Value | Notes |
|---|---|---|---|
| `crypto.btc.entity_balance` | `Subject::Entity` (exchange, ETF, fund, treasury) | `Value::UInt` sats | Sum of UTXOs on labelled addresses at block N. Reference recipe `recipes/btc-entity-balance` (M1), ported from the the entity indexer logic |
| `crypto.btc.entity_netflow` | `Subject::Entity` | `Value::Int` sats | Inflow minus outflow over a block range; internal transfers between the same entity's addresses excluded |
| `crypto.btc.supply` | `Subject::Aggregate("btc.mainnet")` | `Value::UInt` sats | Sum of all UTXO values at block N |

Then ETH, SOL, BNB: entity balances and flows of native assets and major
tokens, same pattern, one adapter per chain (`adapters/source-eth`, …).

Snapshots are `Snapshot::Block { chain, height, block_hash }`. The block hash pins the fork.

## Labels (subjective lane)

Sources, highest confidence first:

1. **First-party attestation** (`FirstPartyAttestation`): the entity signs a message with the address key. BIP-322 for Bitcoin, EIP-191 / EIP-712 for EVM chains. Treated as top-confidence evidence (M2).
2. **Public disclosures**: proof-of-reserve address lists and ETF issuer disclosures published by the entity itself. Evidence stored by content hash.
3. **Contributor judgment** (`CalibratedJudgment`): heuristics such as clustering, scored later with a proper scoring rule against ground truth (M2).

Labels cover entities only (`EntityKind` in `onyx-labels`): exchanges, ETFs,
funds, protocols, custodians, issuers, mining pools, treasuries. No individuals.

Because labels are uncertain, Onyx publishes each metric under at least two
label set versions and reports the spread as a confidence band (M2).

## Rights

Public chain data: license `onyx:public-chain-data`, redistribution allowed.
Entity disclosures: record the disclosure's terms in `DataRights`; if they forbid redistribution, store only our derived label with a pointer, or skip.
Third-party label datasets: only if their license allows redistribution.

## Data sources

- M1: pinned block-range fixtures under `fixtures/btc/` with checksums.
- Later: Bitcoin Core RPC on the build machine 7920; a vendor indexers for ETH, SOL, BNB.

## Open questions

- Fixture size for M1: which block range and which entities give a meaningful but small test?
- Do ETF balances need custodian-level labels (one custodian holding for several ETFs)?
