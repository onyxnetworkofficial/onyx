# btc-entity-balance (M1)

Input: UTXO set changes for a pinned block range (from `adapters/source-btc` fixtures) plus a
label set version. Output: rows `(entity, balance_sats, address_count)` sorted by entity, Borsh-encoded.
Ported from the entity indexer logic. Not built yet.
