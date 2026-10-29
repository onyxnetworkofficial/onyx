# Incident response

## Scope

What to do when something breaks: severity levels and playbooks.

## Current state (M0)

Prototype on testnet. No real funds at risk. Playbooks exist so the habits are in place before mainnet.

Severity levels:

- **SEV1**: wrong data finalized, a key leaked, or funds at risk. Stop finalization now.
- **SEV2**: finalization blocked, determinism mismatch, payments failing.
- **SEV3**: degraded (slow API, one validator down).

## Plan

### Determinism bug (recipe gives different bytes on different machines)

1. Freeze the recipe id: no new rounds may use it.
2. Open disputes on rounds using it resolve as **draw**: no slash, bonds returned ([ADR-0004](../adr/0004-wasm-determinism.md), docs/spec/disputes.md).
3. Find the cause (host config, recipe code, toolchain). Add a regression test.
4. Ship a fixed recipe as a **new** recipe id. Never mutate the old one ([upgrades.md](upgrades.md)).
5. Write a post-mortem in this folder.

### Key leak

1. Treat as SEV1. Stop any process using the key.
2. Rotate the role key ([key-management.md](key-management.md)); on testnet, redeploy if the key was the deployer.
3. Find how it leaked (logs, commit, CI). Purge from logs; if it reached git, rotate first, then rewrite only with the project owner's approval.
4. Post-mortem.

### Wrong data finalized

1. Pause new finalizations. 2. Open a challenge with the recompute evidence. 3. Publish a notice on the API for affected roots.

### Chain indexer stuck or confused

`/v1/health` `indexer`: a `lastError` that repeats, or `nextBlock` far below `finalizedHead`,
means the node is refusing ranges; check the node URL first. `conflicts` above zero means a
backend answered two different logs at one position. `candidatesDropped` or `candidatesEvicted`
above zero means a position filled up (4 logs), which an honest node never causes.

Threat model (ADR-0033 Amendments 4 and 5): the indexer and the per-request root read
(`ChainRoots`) both use your RPC node (`ONYX_RPC_URL`), so this server is only as correct as that
node. A lying node can make it serve a wrong or unfinal root; what catches that is a client
checking the root on its own node (the default `ClientNode::verify`, or `verify_response` with a
chain answer; `chain_checked: false` means the client opted out with `verify_response_unchecked`). If you suspect your node lied: switch nodes and restart (served
shards are read again on the next rescan; two reads on two polls refuting one take it out,
`removed` on `/v1/health`), and tell clients which rounds to re-check. A lying node can also keep
a shard you hold unserved (shown as `voided`, `empty`, `no_valid_root`, `unresolved`,
`not_on_chain`, `rows_missing`, `binding_mismatch`, or not listed at all) until it stops lying
and a held-shard rescan or a restart runs. So, if a shard you hold is final on chain but not
served:
1. Switch to an RPC node you trust (`ONYX_RPC_URL`).
2. Restart the node: the third poll after the start reads the held shards' logs again from
   `deployBlock` and judges every saved shard again (or wait for the next periodic rescan,
   `ONYX_INDEX_RESCAN_POLLS` or `ONYX_INDEX_RESCAN_SECS`, `rescans` on `/v1/health`; it reads
   `ONYX_INDEX_RESCAN_CALLS` ranges per poll, so an old deployment's rescan spans many polls).
3. Still missing after the rescan finished (`rescanning` false): reset the cursor as in
   [deployment.md](deployment.md) "Chain indexer".

## Open questions

- Who is on call (the project owner, the second signer)? How are they paged?
- Do we want an onchain pause switch in M6, and who may trigger it?
