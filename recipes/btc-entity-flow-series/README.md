# btc-entity-flow-series

Input: the same as `btc-entity-balance` (a pinned block range from `adapters/source-btc` plus a
label set version); `params` must be empty. Output: one `EntityBlockFlowRow` per (entity, block)
where the entity was touched, keyed `entity ‖ 0x00 ‖ height (u64 BE)`, Borsh-encoded. Summed over
the range, an entity's rows equal its `btc-entity-balance` row (R-SERIES-2). Spec:
docs/spec/recipes.md (R-SERIES-1..3).
