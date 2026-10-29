# Runbook: redeploy the contracts and announce a new ledger id (L15 c, G5 playbook, LC-19)

**Trigger:** a leaked or lost key whose role cannot rotate (deployer = settlement coordinator and
OnyxRevenue owner/treasury; registry owner), or a contract bug (launch checklist section 4). No
contract has a proxy or pause: the fix is a new deployment.

**What changes:** the **ledger id** is `keccak256("onyx-ledger-evm-v1" || chain id || settlement
address)` (`OnyxSettlement.ledgerId()`), so a new settlement address is a new ledger. Old roots
stay final and checkable at the old address forever; nothing is migrated on chain. Clients must
move to the new manifest; the node needs a **new state directory** (its journals and indexer state
are bound to one chain, contract and deploy block, and refuse another).

**Who:** the project owner decides and supplies the deployer key (a **new** key in `.env` when the old one
leaked); the loop executes. **Time:** the testnet run itself takes about 60-80 minutes
([demo-testnet.md](../demo-testnet.md#3-the-real-run)); the switch and announcement about 20
minutes.

**Assumes** the systemd install in [deployment.md](../deployment.md#run-with-systemd). Testnet
only (`demo-testnet.sh` refuses any chain but 84532).

## Steps

1. Freeze the old deployment: stop opening rounds (no coordinator runs), and if a key leaked,
   treat everything it controls on the old ledger as hostile (launch checklist section 4,
   "Leaked key").

2. Archive the old ledger's files in the private ops store, named by chain and address:

   ```sh
   OLD=$(python3 -I -c 'import json; print(json.load(open("/etc/onyx/deployment.json"))["settlement"])')
   mkdir -p ops-store/ledgers
   sudo cp /etc/onyx/deployment.json "ops-store/ledgers/84532-$OLD.json"
   sudo cp /etc/onyx/catalog.json "ops-store/ledgers/84532-$OLD.catalog.json"
   # one proof from the old ledger, for step 8: any served shard (journal line
   # "indexer: round R shard 0x... serving it"); a free row answers 200, a priced one 402
   # (then save the proof a paid request returned instead)
   ROUND=...; SHARD=0x...
   curl -s -o "ops-store/ledgers/84532-$OLD.proof.json" -w '%{http_code}\n' \
     "http://127.0.0.1:8080/v1/proofs/$ROUND/$SHARD/0"
   ```

3. Deploy and settle on the new ledger from a checkout (deployer key in `.env`, never printed):

   ```sh
   scripts/demo-testnet.sh testnet --plan          # budget and balances; sends nothing
   scripts/demo-testnet.sh testnet --yes --export-node "$PWD/.onyx-testnet/node"
   ```

   Expected: the last line `REHEARSAL PASSED`; `.onyx-testnet/node/` holds `deployment.json`,
   `catalog.json`, `cas/`, `node.env`, `node.service.env`. The run makes OnyxRevenue's reporter
   the `operator` key in `.onyx-testnet/keys/` (a new address per deployment unless that file is
   reused).

4. Read the new ledger id and roles back from the chain:

   ```sh
   RPC=https://sepolia.base.org
   E=.onyx-testnet/node
   NEW=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["settlement"])' $E/deployment.json)
   REV=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["revenue"])' $E/deployment.json)
   cast call "$OLD" 'ledgerId()(bytes32)' --rpc-url "$RPC"
   cast call "$NEW" 'ledgerId()(bytes32)' --rpc-url "$RPC"   # must differ from the line above
   cast call "$NEW" 'coordinator()(address)' --rpc-url "$RPC"
   cast call "$REV" 'owner()(address)' --rpc-url "$RPC"
   cast call "$REV" 'reporter()(address)' --rpc-url "$RPC"
   ```

   Expected: two different ledger ids; coordinator and owner are the deployer you meant (a new
   address if the old key leaked); reporter is the export's operator key.

5. Drain and stop the node on the old ledger:

   ```sh
   curl -s http://127.0.0.1:8080/v1/health | python3 -c 'import json,sys; h=json.load(sys.stdin); print(h["receiptsPending"], h["settles"]["intentsOpen"], (h["reporter"] or {}).get("inFlight", 0))'   # 0 0 0 (payments off: reporter is null)
   sudo systemctl stop onyx-node
   ```

6. Move the old state aside (keep it: it holds the old ledger's receipts and intents) and install
   the new files. Merge `node.service.env` with your `/etc/onyx/node.env` by hand (keep
   `ONYX_PUBLIC_URL`, `ONYX_RPC_URL` or the URL file, `ONYX_INDEX_HEALTH`, `ONYX_TRUSTED_PROXIES`,
   and the live `ONYX_RIGHTS`: the export always writes `fallback`, a testnet node refuses to
   start without it, and `cleared` only once launch checklist LC-1 passed by path A):

   ```sh
   sudo mv /var/lib/private/onyx/state "/var/lib/private/onyx/state-84532-$OLD"
   sudo install -d -m 0700 /var/lib/private/onyx/state
   sudo chown --reference="/var/lib/private/onyx/state-84532-$OLD" /var/lib/private/onyx/state
   sudo install -m 0644 $E/deployment.json $E/catalog.json /etc/onyx/
   sudo cp -r $E/cas/. /var/lib/private/onyx/cas/
   sudo chown -R --reference=/var/lib/private/onyx/cas /var/lib/private/onyx/cas
   # payments on: keep the old ledger's key for the rollback, then install the new one
   sudo cp -p /etc/onyx/operator.key "/etc/onyx/operator.key.84532-$OLD"
   sudo install -m 0600 .onyx-testnet/keys/operator.key /etc/onyx/operator.key
   ```

   The old key file is only for the rollback: if the old key leaked, delete it now
   (`sudo shred -u "/etc/onyx/operator.key.84532-$OLD"`); otherwise once the rollback is ruled
   out. The offline copy stays in the ops store.

   Starting with the new manifest and the old state directory fails at once:
   `indexer state .../indexer.json: written for another chain, contract or deploy block (use a new
   state file)`.

7. Start and check:

   ```sh
   sudo systemctl start onyx-node
   journalctl -u onyx-node -n 20 --no-pager
   curl -s http://127.0.0.1:8080/v1/health | python3 -c 'import json,sys; h=json.load(sys.stdin); print(h["chainId"], h["contract"], h["indexer"]["servable"])'
   ```

   Expected: `operator key 0x... is OnyxRevenue's reporter`, `listening on ...`, then
   `indexer: round R shard 0x... serving it` per shard; health prints `84532`, `NEW` and the
   number of held shards. Every proof the node serves now names `NEW` in `verify.contract`.

## Verify

8. Old roots are still checkable at the old address, from the proof saved in step 2:

   ```sh
   P="ops-store/ledgers/84532-$OLD.proof.json"
   read -r ROUND SHARD ROW INDEX LEAFS SIBLINGS < <(python3 -I -c 'import json,sys; p=json.load(open(sys.argv[1])); print(p["round"], p["shard"], p["row"], p["proof"]["index"], p["proof"]["leafCount"], "[" + ",".join(p["proof"]["siblings"]) + "]")' "$P")
   cast call "$OLD" 'getFinalizedRoot(uint64,bytes32)(bytes32,bytes32,uint64)' "$ROUND" "$SHARD" --rpc-url "$RPC"
   cast call "$OLD" 'verifyRow(uint64,bytes32,bytes,uint64,uint64,bytes32[])(bool)' "$ROUND" "$SHARD" "$ROW" "$INDEX" "$LEAFS" "$SIBLINGS" --rpc-url "$RPC"
   BAD=${ROW%??}$(printf '%02x' $(( (0x${ROW: -2} + 1) % 256 )))   # last byte changed
   cast call "$OLD" 'verifyRow(uint64,bytes32,bytes,uint64,uint64,bytes32[])(bool)' "$ROUND" "$SHARD" "$BAD" "$INDEX" "$LEAFS" "$SIBLINGS" --rpc-url "$RPC"
   ```

   Expected: the old root, binding and finalized block (non-zero), then `true`, then `false`.

9. Health watch on the new addresses. Until step 10 updates
   `deploy/health-watch/thresholds.json`, pass an overlay naming the new contract (and the new
   deployer, if it changed):

   ```sh
   printf '{"addresses": {"revenue": "%s"}}\n' "$REV" > /tmp/watch-new-ledger.json
   ONYX_WATCH_RPC_URL="$RPC" python3 -I scripts/health-watch.py --once \
     --node http://127.0.0.1:8080 --config /tmp/watch-new-ledger.json; echo "exit $?"
   ```

   Expected: exit 0, `balances.revenue` read from `REV`
   ([health-watch.md](health-watch.md#steps)).

## Announce

10. Update the repository in one commit:
    - [deployment.md "Deployed addresses"](../deployment.md#deployed-addresses-base-sepolia):
      the new settlement, disputes, OnyxRevenue and deployer rows, with the date; move the old
      ones to a "Retired ledgers" line with the reason.
    - [deploy/health-watch/thresholds.json](../../../deploy/health-watch/thresholds.json)
      `addresses`, and `REVENUE` in `scripts/health-watch-tests.py` (a test fails when they
      differ from deployment.md).
    - The Run page data for the new run (`scripts/build-testnet-run.py`, demo-testnet.md).
    - Launch checklist section 2 (key map) if any role address changed.

11. Publish a notice on the website and in the API's documentation (no personal data):

    > **New ledger on Base Sepolia (testnet), <date UTC>.** Onyx moved to settlement
    > `<NEW>` (ledger id `<new ledgerId>`, chain 84532); pay-to (OnyxRevenue) is `<REV>`.
    > The previous ledger, settlement `<OLD>` (ledger id `<old ledgerId>`), is retired
    > because <reason>. Its finalized roots stay readable at `<OLD>`: check old rows with
    > `getFinalizedRoot` / `verifyRow` there. Do not pay the old pay-to address; this server no
    > longer serves or settles the old ledger. Clients: pin the new manifest
    > (`deployment.json`, settlement code hash `<hash>`).

    If a key leaked, say so in the notice: anyone holding it can still act on the old ledger.

12. Rebuild and republish the website in the mode that is live
    (`ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh`, or `ONYX_RIGHTS=cleared` once
    LC-1 passed by path A; without `ONYX_RIGHTS` the public build is refused), launch checklist
    LC-6.

## Rollback

Only if the old ledger is still sound (the redeploy was for a bug found to be harmless, not a
leaked key). `OLD` as in step 2:

```sh
sudo systemctl stop onyx-node
sudo cp "ops-store/ledgers/84532-$OLD.json" /etc/onyx/deployment.json
sudo cp "ops-store/ledgers/84532-$OLD.catalog.json" /etc/onyx/catalog.json
sudo chmod 0644 /etc/onyx/deployment.json /etc/onyx/catalog.json
sudo mv /var/lib/private/onyx/state /var/lib/private/onyx/state-new-ledger
sudo mv "/var/lib/private/onyx/state-84532-$OLD" /var/lib/private/onyx/state
sudo install -m 0600 "/etc/onyx/operator.key.84532-$OLD" /etc/onyx/operator.key   # payments on
sudo systemctl start onyx-node
curl -s http://127.0.0.1:8080/v1/health | python3 -c 'import json,sys; print(json.load(sys.stdin)["contract"])'   # OLD
```

Retract the notice. The new deployment stays on chain unused.

## Rehearsal

Rehearsed 2026-10-16 on a local anvil fork of Base Sepolia: two deployments with
`scripts/demo-testnet.sh fork --export-node` (nothing broadcast; anvil's public dev key as
deployer, so the coordinator did not change), the node switched from one to the other and back
([rehearsal report](../../audit/2026-10-16-runbook-rehearsal.md#c-redeploy-and-announce-a-new-ledger-id)).
The `testnet` deploy of step 3 and the announcement steps 10-12 were not run.
