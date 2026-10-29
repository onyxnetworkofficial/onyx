# Upgrade and versioning policy

## Scope

How recipes, label sets, schemas and contracts change over time, and the gate before any mainnet deploy.

## Current state (M0)

- Recipe id = Keccak-256 of the exact WASM bytes ([ADR-0003](../adr/0003-hash-and-merkle.md)). A recipe can never change; a fix is a new recipe id.
- Label sets: `(name, version, content_hash)`. Versions are monotonic per name; an old version is never edited.
- Row schema id = hash of the schema descriptor; a layout change is a new schema id.
- Contracts: not yet deployed.

## Plan

- Recipes: registry keeps every id forever. A metric points to its current recipe id; switching is a governance action ([governance.md](governance.md)) and is visible in every finalized root (roots carry recipe id and label set hash).
- Contracts: immutable core contracts. No proxies in the prototype. An upgrade is a new deployment plus a migration note; old roots stay readable at the old address. Testnet redeploy playbook (ROADMAP G5, L15): [runbooks/redeploy-new-ledger.md](runbooks/redeploy-new-ledger.md).
- Toolchain: `rust-toolchain.toml` and `solc_version` are pinned. A toolchain bump must rebuild every reference recipe and confirm identical recipe ids or record new ones.

### API answer compatibility

`onyx_api::client` refuses any answer key its endpoint does not write (THREATS M7-L41-F4): a
server cannot slip unchecked fields past a verifying client. So a new answer key breaks clients
built before it (they fail closed) while new clients still accept the old answer. Each such
change is listed here; update the client before (or with) the server. The workspace version is
still 0.0.1 (no release published), so a change is named by date and commit.

| Date | Change | Old client on a new server | New client on an old server | Commit |
|---|---|---|---|---|
| 2026-10-12 | `onchain.transcriptComplete` hint on level-2 answers (REVLOG-R1-L1) | refuses the answer | accepts | `5c90894` |
| 2026-10-17 | `limits` on `/v1/observations` and `/attempts` answers (ROADMAP B4, delivery.md §Level-2 limits) | refuses the answer | accepts | branch `revlog-api` |
| 2026-10-17 | `sealed` on level-2 rounds and the `onchain.drawRevealKey`, `revealLogComplete`, `attemptMatches` hints (ROADMAP C11, delivery.md "Sealed reveals"); the client's on-chain step asks those calls and needs an `OnyxRegistry` with sealed reveals (C4) | refuses the answer | accepts the answer; refused on chain by a registry from before C4 (the calls revert) | branch `sealed-client` |
| 2026-10-17 | New route `/v1/served/{metric}` and `onyx_api::client::verify_row` (ROADMAP D3, delivery.md G-6); no answer key changed | unaffected | the client's reserve check gets 404 from an old server (no list) and cannot run; plain answers verify as before | branch `reserve-client` |

Level 2 is not live on any public server yet (THREATS REVLOG-5; `--level2` is fork-only until
ROADMAP B6), so none of these changes has broken a deployed buyer. The
x402 `PAYMENT-REQUIRED` header is not strict (unknown fields ignored), so its new `extensions`
breaks no client; the 402 JSON body is not read by the client.

### Mainnet gate (all must be true; the project owner and the second signer sign off)

1. M0-M8 accepted on testnet.
2. External audit of settlement contracts.
3. Threat model ([../../THREATS.md](../../THREATS.md)) has no open high items.
4. Keys in KMS/HSM; admin behind multisig.
5. Legal review of data rights for every live source ([legal-data-rights.md](legal-data-rights.md)).

## Open questions

- Do we ever want proxies (for example a pausable router) on mainnet, or only redeploys?
- Deprecation window for an old recipe id once a new one replaces it.
