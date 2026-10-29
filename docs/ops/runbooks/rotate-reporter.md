# Runbook: rotate the OnyxRevenue reporter (L15 b, LC-19)

**Trigger:** the operator key (`/etc/onyx/operator.key`, OnyxRevenue's **reporter**) leaked,
was lost, or is due for rotation. The reporter is the one role that rotates in place
([key-management.md](../key-management.md#what-rotates-on-chain-contracts-as-deployed-l2-audit-threatsmd-l2c-5)):
the revenue **owner** (= treasury, today the deployer key in `.env`) calls `setReporter(new)`.
No redeploy, no new ledger id. Credits the old key already made stay.

**Who:** the project owner decides; the loop executes. The owner key signs one transaction. **Time:** about
15 minutes. Payments stop for the minutes between steps 5 and 9.

**Assumes** the systemd install in [deployment.md](../deployment.md#run-with-systemd) with
payments on (`self-settle`, or `facilitator` with the reporter on). Testnet only. The owner key
is typed at cast's prompt (`--interactive`), never put on a command line or in shell history.

## Steps

1. Read the current roles. `OLD` must equal the node's key address, which the node's offline
   config check prints (`operator key address 0x...`):

   ```sh
   onyx_check() { sudo bash -c 'set -a; . /etc/onyx/node.env; set +a
     export ONYX_STATE_DIR=/var/lib/private/onyx/state ONYX_CAS_DIR=/var/lib/private/onyx/cas
     [ -f /etc/onyx/operator.key ] && export ONYX_OPERATOR_KEY_FILE=/etc/onyx/operator.key
     exec env "$@" /usr/local/bin/onyx-node --check-config' _ "$@"; }
   onyx_check | grep 'operator key address'
   OLD=0x...   # the address printed (cast prints it checksummed: compare ignoring case)
   RPC=https://sepolia.base.org   # or your ONYX_RPC_URL
   REV=$(python3 -I -c 'import json; print(json.load(open("/etc/onyx/deployment.json"))["revenue"])')
   cast chain-id --rpc-url "$RPC"                          # 84532
   cast call "$REV" 'reporter()(address)' --rpc-url "$RPC" # OLD
   cast call "$REV" 'owner()(address)' --rpc-url "$RPC"    # the key that must sign step 6
   ```

2. Make the new key on the node host, readable by root only, and read its address (the node's
   config check prints the address, never the key):

   ```sh
   sudo bash -c 'umask 077; openssl rand -hex 32 > /etc/onyx/operator.key.new'
   onyx_check ONYX_OPERATOR_KEY_FILE=/etc/onyx/operator.key.new | grep 'operator key address'
   NEW=0x...   # the address printed
   ```

   Expected: `onyx-node: operator key address 0x...`. Keep an offline copy of the new key in the
   ops store (deployment.md "Backups").

3. Give `NEW` gas (it pays for every settle and credit it sends; for scale, the whole A2 run's
   198 transactions used 0.000262 test ETH, launch checklist LC-12). From the deployer, typing
   its key at the prompt:

   ```sh
   cast send --interactive "$NEW" --value 0.0005ether --rpc-url "$RPC"
   cast balance "$NEW" --ether --rpc-url "$RPC"
   ```

4. Let in-flight work drain: wait until the node shows nothing pending, then stop it.

   ```sh
   curl -s http://127.0.0.1:8080/v1/health | python3 -c 'import json,sys; h=json.load(sys.stdin); print(h["receiptsPending"], h["settles"]["intentsOpen"], h["reporter"]["inFlight"])'
   ```

   Expected: `0 0 0`. (Not zero after a few minutes, or the old key leaked: stop anyway. Credits
   in flight stay on disk and the new key posts them after step 9; credits are idempotent.)

5. Stop the node:

   ```sh
   sudo systemctl stop onyx-node
   journalctl -u onyx-node -n 3 --no-pager   # "stopped (drained: ...)"
   ```

6. The owner rotates the reporter (type the owner key at the prompt):

   ```sh
   cast send --interactive "$REV" 'setReporter(address)' "$NEW" --rpc-url "$RPC"
   ```

   Expected: `status 1 (success)` and one log with topic
   `0x4f9f7d6f8a1eaf88f8e7f717e3445582b9469143b222592ec075c4b4dfc92cef`
   (`ReporterSet(address)`) whose data is `NEW`. Any other signer reverts with `NotOwner`
   (`0x30cd7471`).

7. Check the chain: `cast call "$REV" 'reporter()(address)' --rpc-url "$RPC"` prints `NEW`.

8. Install the new key in place of the old one (keep the old file only until step 10):

   ```sh
   sudo mv /etc/onyx/operator.key /etc/onyx/operator.key.old
   sudo mv /etc/onyx/operator.key.new /etc/onyx/operator.key
   sudo chmod 0600 /etc/onyx/operator.key
   ```

9. Start the node:

   ```sh
   sudo systemctl start onyx-node
   journalctl -u onyx-node -n 10 --no-pager
   ```

   Expected: `operator key 0x<NEW> is OnyxRevenue's reporter`, then `listening on ...`. The node
   reads `reporter()` at every start and **refuses to start** with any other key (exit 1):
   `ONYX_OPERATOR_KEY_FILE holds the key of 0x<OLD>, but OnyxRevenue.reporter() is 0x<NEW>`.

## Verify

10. The node and the chain agree, and the old key can do nothing:

    ```sh
    curl -s http://127.0.0.1:8080/v1/health | python3 -c 'import json,sys; h=json.load(sys.stdin); print(h["acceptingPayments"], h["reporter"])'
    cast call --from "$OLD" "$REV" 'registerObservation(bytes32)' 0x$(printf '%064x' 1) --rpc-url "$RPC"
    ```

    Expected: `True` and a reporter block with `lastError: None`; the call from `OLD` reverts with
    `0xe5ce9c09` (`NotReporter`). If paid traffic exists, one paid request answers 200
    (deployment.md "Smoke test") and `reporter.applied` or the self-settle count rises.
    Then delete the old key: `sudo shred -u /etc/onyx/operator.key.old` (if it leaked it is now
    powerless; if it did not, the offline copy is enough).

11. Run the health watch once, as in [health-watch.md](health-watch.md#steps) step 1
    (`ONYX_WATCH_RPC_URL=... python3 -I scripts/health-watch.py --once --node http://127.0.0.1:8080`):
    exit 0. With the timer installed, `systemctl --user start onyx-health-watch.service` does the same.

## Rollback

Only when the new key is lost or broken **and the old key did not leak**:

1. `sudo systemctl stop onyx-node`.
2. Owner: `cast send --interactive "$REV" 'setReporter(address)' "$OLD" --rpc-url "$RPC"`.
3. `sudo mv /etc/onyx/operator.key.old /etc/onyx/operator.key` (or restore it from the offline
   copy), `sudo systemctl start onyx-node`, and check step 9's line names `OLD`.

If the old key leaked, never roll back: make another new key and run the steps again.

## Rehearsal

Rehearsed 2026-10-16 on a local anvil fork of Base Sepolia with a local onyx-node; the owner
there is anvil's public dev account, so steps 3 and 6 used `cast send --unlocked --from <owner>`
instead of `--interactive`
([rehearsal report](../../audit/2026-10-16-runbook-rehearsal.md#b-rotate-the-reporter)).
