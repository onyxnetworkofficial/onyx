# Key management

## Scope

Signing keys for contract deployment, coordinator operations, miners, validators,
attestation test keys and x402 pay-to addresses.

## Current state (M0)

- One key: the Base Sepolia testnet deployer key, in `.env` only, supplied by the project owner.
- BIP-322 / EIP-191 / EIP-712 attestation keys for tests (M2) are generated
  deterministically from fixed test seeds inside test code. They never hold funds and
  are published on purpose.
- No mainnet keys exist and none may be created for this project yet.

## Plan

- Separate key per role, even on testnet: deployer, coordinator, x402 pay-to,
  each simulated miner and validator. A leak of one does not compromise the others.
- Prototype admin actions (recipe registry, label source allowlist) move to a
  2-of-2 multisig (the project owner + the second signer) before any mainnet discussion ([governance.md](governance.md)).
- Before mainnet: keys in an HSM or cloud KMS; no raw private keys on disk; signing through the KMS API.
- Rotation: only two role keys rotate in place on the deployed contracts (table below).
  Every other role is an immutable constructor argument, so its address must be a multisig or
  KMS key chosen before deploy (launch checklist K-2), and losing it means a redeploy
  (K-3). Runbooks: [rotate the reporter](runbooks/rotate-reporter.md); a lost or leaked
  immutable key means [a redeploy and a new ledger id](runbooks/redeploy-new-ledger.md).
- Leak response: see [incident-response.md](incident-response.md).

## What rotates on chain (contracts as deployed; L2 audit, THREATS L2C-5)

| Role | Contract | Rotates in place? | If the key is lost or leaked |
|---|---|---|---|
| Revenue reporter | OnyxRevenue | Yes: the owner calls `setReporter` | Owner sets a new reporter; credits already made stay |
| Receipt signer | OnyxRegistry | Yes: the coordinator calls `setReceiptSigner` | Coordinator sets a new signer |
| Coordinator | OnyxSettlement (+Ext) | No (immutable) | Redeploy the settlement: a new ledger id (chain id + address), so every client, the registry and revenue move to the new deployment |
| Dispute game binding | OnyxSettlement / OnyxDisputes | No (immutable, bound once) | Redeploy both: a new ledger id |
| Registry owner | OnyxRegistry | No (immutable) | Redeploy the registry against the ledger; a new manifest |
| Revenue owner = treasury | OnyxRevenue | No (immutable) | Redeploy revenue against the ledger; a new manifest |

The settlement's ledger id is bound to its address, so any settlement redeploy starts a new
ledger: old roots stay readable at the old address ([upgrades.md](upgrades.md)). A registry or
revenue redeploy keeps the ledger id but changes the deployment manifest clients pin.

## Open questions

- Which KMS/HSM (cloud provider vs hardware) when we leave testnet? Paid services need the project owner's approval.
- Do miners and validators bring their own keys (permissionless) from day one, or are they allowlisted in early testnet rounds?
