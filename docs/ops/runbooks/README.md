# Runbooks

Step-by-step procedures. Each runbook: trigger, preconditions, steps, how to
verify, how to roll back.

Written:

- [health-watch.md](health-watch.md) (L12): install the health watch timer, read its alerts, wire an alert channel.
- [stop-serving-shard.md](stop-serving-shard.md) (L15 a): take one shard off the node (takedown, removal, bad final root).
- [rotate-reporter.md](rotate-reporter.md) (L15 b): replace OnyxRevenue's reporter key with `setReporter`.
- [redeploy-new-ledger.md](redeploy-new-ledger.md) (L15 c, G5 playbook): redeploy, move the node to the new ledger id, keep old roots checkable, announce.
- [deploy-signed-book.md](deploy-signed-book.md) (C10, lead only): deploy OnyxSignedBook next to the running ledger with a reviewer that is not the deployer, pin it in the manifest, restart the node; rehearsed by `scripts/level3-book.sh local|fork`.
- [release-source-terms.md](release-source-terms.md) (L15 d): before a release, re-read every served source's terms (`scripts/check-source-terms.py`).

The L15 runbooks were rehearsed once each on a local anvil fork of Base Sepolia
([report](../../audit/2026-10-16-runbook-rehearsal.md)).

Planned:

- `deploy-base-sepolia.md` (M6): deploy settlement contracts to Base Sepolia, record addresses in [../deployment.md](../deployment.md).
- `freeze-recipe.md` (M5): freeze a recipe after a determinism bug ([../incident-response.md](../incident-response.md)).
- `rotate-testnet-key.md` (M6): replace the testnet deployer key ([../key-management.md](../key-management.md)); its roles are immutable, so this is [redeploy-new-ledger.md](redeploy-new-ledger.md) with a new key in `.env`.
- `restore-state.md`: restore `ONYX_STATE_DIR` (receipts, intents, served heads) and the private
  intake journal with its feed ledger from a backup taken while the node was stopped.
- `rebuild-from-cas.md` (M7): rebuild query indexes from the content-addressed store.
