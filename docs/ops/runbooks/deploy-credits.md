# Runbook: deploy the credits ledger on Base Sepolia (CL6, ADR-0056, lead only)

**Trigger:** the lead decides to put the credits ledger live: one balance per account, relayed
observer posts and deposits paid from credits. Nothing of CL6 is on Base Sepolia yet; everything
below was rehearsed only on a local anvil (`scripts/credits-anvil.sh`, in check-all).

**What it is:** two new contracts, no proxies (ADR-0005): `OnyxRevenueV2` (OnyxRevenue plus
`withdrawToCredits` and a 1% treasury floor) and `OnyxPrepaidV2` over it (the v1 voucher type and
header, plus `depositFor`, `depositWithAuthorization` and `payRelayer`). The node gains the
credits routes (`/v1/credits/...`) with `ONYX_PREPAID`, and `POST /v1/relay` with
`ONYX_RELAY_KEY_FILE`.

**Money:** test USDC and test ETH only. The relayer's key fronts gas in test ETH and is repaid in
credits by the reporter's pass (`payRelayer`, at most `ONYX_RELAY_CAP_PER_DAY` a day). Balances on
an old `OnyxPrepaid` never carry over: they leave by that contract's own exit.

**Who:** the operator with the deployer key in `.env` (never printed, never committed). Three
roles must be three different keys: the deployer (owns `OnyxPrepaidV2`), the reporter (the node's
`ONYX_OPERATOR_KEY_FILE`; may equal the deployer as today, but then a leaked key cannot be rotated
out, REV-L1-F4), and the **relayer** (a fresh key: the contract, the deploy script and the node all
refuse a relayer that is the reporter, an owner, the treasury or the coordinator; THREATS CL6-14).
**Time:** about 30 minutes. **Cost:** two contract creations (the dry run prints forge's estimate)
and test ETH for the relayer (about 0.0001 ETH covers hundreds of relays at Base Sepolia's usual
fees; measured gas per relayed action on anvil: deposit about 94,800, commit 179,700, receipt 112,700).

## Preconditions

1. A checkout of the commit to deploy with `scripts/check-all.sh` green; `forge`, `cast` in
   `~/.foundry/bin`.
2. `.env` holds `BASE_SEPOLIA_PRIVATE_KEY` and optionally `BASE_SEPOLIA_RPC_URL` (load it with
   `set -a; . ./.env; set +a` in the shell that runs forge; never `cat` it).
3. The deployment's addresses from the published manifest: the settlement (`ONYX_SETTLEMENT`), the
   test USDC (`ONYX_USDC`, Circle's Base Sepolia USDC, FiatToken v2.2: it has
   `receiveWithAuthorization` and `DOMAIN_SEPARATOR()`), the reporter address, the treasury.
4. A new relayer key file made on the server, 0600, never copied elsewhere:
   `umask 077; python3 -I -c 'import secrets; print(secrets.token_hex(32))' > /etc/onyx/relay.key`;
   its address: `cast wallet address --private-key "$(cat /etc/onyx/relay.key)"` (run it on the
   server, do not paste the key anywhere).

## Steps

1. Dry run (no transaction): from `contracts/`,
   ```sh
   ONYX_SETTLEMENT=<settlement> ONYX_USDC=<usdc> ONYX_REPORTER=<reporter> \
   ONYX_TREASURY=<treasury> ONYX_RELAYER=<relayer address> \
   ONYX_PREPAID_EXIT_DELAY=86400 ONYX_RELAY_CAP_PER_DAY=1000000 \
   forge script script/DeployCredits.s.sol --rpc-url "$BASE_SEPOLIA_RPC_URL"
   ```
   It refuses another chain, a relayer equal to the deployer, reporter or treasury, and a split
   that keeps under 1% for the treasury. Read the two addresses and the gas estimate.
2. Deploy: the same command with `--broadcast`. Record both addresses and the block number of the
   `OnyxPrepaidV2` creation (`cast receipt <tx> blockNumber`).
3. Check on chain (each must print the expected value):
   `cast call <prepaidV2> 'relayer()(address)'`, `'reporter()(address)'`, `'revenue()(address)'`
   (= the new revenue), `'token()(address)'`, `'maxRelayPerDay()(uint128)'`, and
   `cast call <revenueV2> 'MIN_TREASURY_BPS()(uint16)'` (100), `'treasuryBps()(uint16)'` (>= 100).
4. Open the observation books on the new revenue for every fact the node sells (from the reporter
   key, as for v1: docs/ops/run-api.md "Money loop"):
   `cast send <revenueV2> 'registerObservation(bytes32)' <fact> --private-key-file ...`.
5. Publish the new manifest: `revenue` = `OnyxRevenueV2`, `revenueCodeHash` =
   `cast keccak "$(cast code <revenueV2>)"`; clients pin `payTo` from it (REV-4). x402 receipts of
   payments made to the old revenue stay with the old contract's reporter pass until it is empty.
6. Fund the relayer with a little test ETH from the faucet key (`cast send <relayer> --value
   0.0002ether ...`).
7. Node: stop it, then set `ONYX_PREPAID=<prepaidV2>`, `ONYX_RELAY_KEY_FILE=/etc/onyx/relay.key`
   (systemd `LoadCredential`, as the operator key), `ONYX_CREDITS_FROM_BLOCK=<step 2 block>`, and
   optionally `ONYX_RELAY_FEES` (default `post_commit=1000,post_receipt=1000,
   post_sealed_reveal=3000,deposit=2000`). The prepaid journal of the new contract is
   `<ONYX_STATE_DIR>/prepaid-0x<prepaidV2>.journal`. If the state dir still holds a
   `prepaid.journal` of an old `OnyxPrepaid`, the node refuses to start (its vouchers are another
   contract's): move it aside together with its `.allocated` and `.domain` files, and keep it for a
   final reporter pass against the old contract (an old node run pointed at it) so the old
   contract's last vouchers are redeemed before its buyers exit.
8. Start the node. Its log must show `relay on: relayer 0x..., fees ... (hash 0x...)` and
   `deposits relayed`; a start that names a role clash or a wrong `relayer()` is the check working.

## Verify

1. `curl -s -A onyx-dev https://<host>/v1/credits/schedule`: four actions with their fees, the
   relayer's address, `relay: true`.
2. `curl -s -A onyx-dev https://<host>/v1/credits/<any address>`: `view: public` with chain numbers.
3. One relayed deposit with a fresh test account that holds no ETH (as `credits-rehearsal`
   does), then that account's `/v1/credits/<account>` with its own `ONYX-ACCOUNT-PROOF`: `credited.
   deposit` once the deposit is finalized, `spent` = the deposit fee.
4. After the next reporter pass: `cast call <usdc> 'balanceOf(address)(uint256)' <relayer>` grew
   by the fees, `cast call <prepaidV2> 'pool()(uint256)'` back to what answers still owe books.

## Roll back

Unset `ONYX_RELAY_KEY_FILE` and restart: relays stop (`404 relay_not_offered`), credits keep
working. To leave v2 entirely, point `ONYX_PREPAID` back at the old contract (its journal back
under its own name); buyers on v2 leave by `requestExit` and `withdraw` after the exit delay.
Nothing on chain needs undoing; the relayer's unused test ETH can be sent back to the faucet key.
