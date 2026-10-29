# Runbook: stop serving a shard (L15 a, LC-19)

**Trigger:** a data-rights takedown, a label removal that reaches a served shard, a bad root that
is already final on chain (launch checklist section 4), or a source whose terms changed
([release-source-terms.md](release-source-terms.md)).

**What it does:** the node stops answering for one shard (its rows, proofs, export and any market
on it). It does **not** change the chain: the root stays final and readable at the settlement
address, and anyone holding the rows can still check them there. To tell clients, publish a
notice (step 9).

**Who:** the project owner decides for a takedown or removal; the loop executes. **Time:** about 10 minutes,
plus the node's drain on stop: it prints its bound when it stops (32 s with payments off, as on
the hosted node in October 2026; at most about 66 s with payments on; the unit allows 75 s).
In the 2026-10-28 drill the stop took 0.06 s and the node served again 1.8 s after its start.

**Assumes** the systemd install in [deployment.md](../deployment.md#run-with-systemd)
(`/etc/onyx/node.env`, `/etc/onyx/catalog.json`, state and rows under `/var/lib/private/onyx/`)
with the chain indexer on (`ONYX_INDEXER=on`, the catalog's `held` list). With the indexer off,
do the same to the `shards` list. Docker: the same files are the container's mounts; replace the
`systemctl` lines with `docker stop -t 75 onyx-node` / `docker start onyx-node`.

## Steps

1. Define a helper that runs the node's offline config check with the service's settings
   (no network call; prints addresses and shard ids, never a key):

   ```sh
   onyx_check() { sudo bash -c 'set -a; . /etc/onyx/node.env; set +a
     export ONYX_STATE_DIR=/var/lib/private/onyx/state ONYX_CAS_DIR=/var/lib/private/onyx/cas
     [ -f /etc/onyx/operator.key ] && export ONYX_OPERATOR_KEY_FILE=/etc/onyx/operator.key
     exec env "$@" /usr/local/bin/onyx-node --check-config' _ "$@"; }
   ```

2. Find the shard's entry. Write the shard id down as `SHARD`:

   ```sh
   onyx_check
   SHARD=0x...   # the shard to stop
   journalctl -u onyx-node --no-pager | grep "shard $SHARD" | tail -1
   ROUND=...     # R from that line: "indexer: round R shard 0x... serving it"
   ```

   Expected: `config ok: ...` and one line per entry, e.g.
   `onyx-node: catalog held[0] metric btc.entity-flow shard 0xd129...1878`. Note the index `N`
   of the line whose shard is `SHARD`.

3. Back up the catalog (keep the backup for the rollback):

   ```sh
   sudo cp -p /etc/onyx/catalog.json /etc/onyx/catalog.json.bak-$(date -u +%Y%m%d)
   ```

4. Write the new catalog next to the old one: drop `held[N]` and every market on `SHARD`
   (a market on a shard the catalog no longer has is refused at start):

   ```sh
   N=0; LIST=held   # from step 2: held[N], or LIST=shards for a shards[N] line (indexer off)
   sudo python3 -I - /etc/onyx/catalog.json /etc/onyx/catalog.json.new "$LIST" "$N" "$SHARD" <<'PY'
   import json, sys
   src, dst, lst, i, shard = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4]), sys.argv[5].lower()
   c = json.load(open(src))
   gone = c[lst].pop(i)
   markets = c.get("markets", [])
   kept = [m for m in markets if m["shard"].lower() != shard]
   if "markets" in c:
       c["markets"] = kept
   with open(dst, "w") as f:
       json.dump(c, f, indent=2)
       f.write("\n")
   print(f"removed {lst}[{i}] ({gone['metric']}) and {len(markets) - len(kept)} market(s) on {shard}")
   PY
   onyx_check ONYX_CATALOG=/etc/onyx/catalog.json.new
   ```

   Expected: `removed held[N] (<metric>) and <k> market(s) on 0x...`, then `config ok` with one
   held shard fewer and no line naming `SHARD`. If any line still names `SHARD`, stop: `N` was
   wrong.

5. Put it in place and restart (the stop drains requests and their settles, up to the bound it
   prints: 32 s with payments off, about 66 s with payments on):

   ```sh
   sudo install -m 0644 /etc/onyx/catalog.json.new /etc/onyx/catalog.json
   sudo systemctl restart onyx-node
   journalctl -u onyx-node -n 20 --no-pager
   ```

   Expected: `stopped (drained: ...)`, `config ok`, `listening on ...`, then `indexer: round R
   shard 0x... serving it` for every remaining shard and none for `SHARD`.

6. Takedown only: move the shard's row batch out of the served store, so no later catalog edit
   can serve it by mistake. Its file name is the root:

   ```sh
   RPC=https://sepolia.base.org   # or your ONYX_RPC_URL
   SETTLEMENT=$(python3 -I -c 'import json; print(json.load(open("/etc/onyx/deployment.json"))["settlement"])')
   cast call "$SETTLEMENT" 'getFinalizedRoot(uint64,bytes32)(bytes32,bytes32,uint64)' "$ROUND" "$SHARD" --rpc-url "$RPC"
   ROOT=...   # first line, without 0x
   sudo install -d -m 0700 /var/lib/onyx-withheld
   sudo mv "/var/lib/private/onyx/cas/rows/$ROOT" /var/lib/onyx-withheld/
   ```

   Keep the file (restricted) until the project owner decides; label-removal.md says history is not
   destroyed without him.

## Verify

7. The shard and its markets are gone; the others still answer:

   ```sh
   B=http://127.0.0.1:8080
   curl -s -o /dev/null -w '%{http_code}\n' "$B/v1/proofs/$ROUND/$SHARD/0"   # 404
   curl -s -o /dev/null -w '%{http_code}\n' "$B/v1/export/$ROUND/$SHARD"     # 404
   curl -s "$B/v1/health" | python3 -c 'import json,sys; i=json.load(sys.stdin)["indexer"]; print(i["servable"], i["notHeld"])'
   ```

   Expected: `404` (`{"error":{"code":"unknown_shard",...}}`), a market on it `404`
   (`unknown_market`), `servable` one lower and `notHeld` one higher than before (the indexer
   still sees the chain event; it just holds no rows to serve). With `ONYX_INDEX_HEALTH=full` the
   shard's own status reads `not_held`.

8. Run the health watch once ([health-watch.md](health-watch.md#steps) step 1, with `--node`
   and the RPC URL): exit 0.

9. Tell people: rebuild and republish the website if it shows the shard (launch checklist
   section 5, "Website"), and post a notice naming the round, shard and reason (never a person's
   data). Record the action where the trigger came from (label-removal.md log, data-sources.md,
   or the incident's post-mortem).

## Rollback: serve it again

1. Put the rows back if step 6 moved them:
   `sudo mv /var/lib/onyx-withheld/$ROOT /var/lib/private/onyx/cas/rows/`.
2. Restore the backup and restart:
   `sudo install -m 0644 /etc/onyx/catalog.json.bak-<date> /etc/onyx/catalog.json && sudo systemctl restart onyx-node`.
3. Verify: the journal shows `indexer: round R shard SHARD ... serving it` (in the rehearsal it
   came on the first poll, about 1 s after the start, because the indexer state file still had
   the event) and `/v1/proofs/R/SHARD/0` answers again (200, or 402 for a priced row).

## A level-2 series (an observations store)

The daily values (earthquakes, weather, the Treasury's debt) are not catalog shards: each series
is a results store named in `ONYX_OBSERVATIONS` and pinned in `ONYX_OBSERVATION_TEMPLATES`
(`metric=0x<template id>`), both in `/etc/onyx/node.env`. Drilled 2026-10-28 on a local node
with the deployed export (north-star loop NL20).

1. Back up the settings: `sudo cp -p /etc/onyx/node.env /etc/onyx/node.env.bak-$(date -u +%Y%m%d)`.
2. Remove the series from **both** lists (the store path from `ONYX_OBSERVATIONS`, its
   `metric=0x...` from `ONYX_OBSERVATION_TEMPLATES`). Removing only the store is refused at
   start: `ONYX_OBSERVATION_TEMPLATES pins <metric> but no ONYX_OBSERVATIONS store serves it`,
   and the node does not come up (the drill hit this; all other series went down with it).
3. `sudo systemctl restart onyx-node`, then check:
   `curl -s "http://127.0.0.1:8080/v1/observations/<metric>?subject=<a served day>"` answers 404
   `unknown_metric`, and another series still answers 200.
4. **Also remove it from the export** you deploy from (`<export>/observations/<file>.json` and
   its line in `<export>/observations/pins`): `scripts/deploy-api.sh` installs every store in the
   export's `observations/` folder, so the next deploy would serve the series again
   (THREATS NL20-3).
5. Rollback: restore `node.env.bak-<date>` (and the export files), restart; the drill's series
   answered 200 again right after the restart.

## Rehearsal

Rehearsed 2026-10-16 on a local anvil fork of Base Sepolia with a local onyx-node, the same
steps with the export directory in place of `/etc/onyx`
([rehearsal report](../../audit/2026-10-16-runbook-rehearsal.md#a-stop-serving-a-shard)).
