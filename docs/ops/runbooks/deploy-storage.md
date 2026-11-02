# Runbook: deploy the storage network on Base Sepolia (ADR-0060, lead only)

**Trigger:** the lead decides to put the storage network live (ROADMAP S15). Deployed on Base Sepolia
2026-11-01: `OnyxStorage` `0x2423837572b58caf82896491ff6ee23080849419` (block 52986889), with
`scripts/storage-sepolia.sh` (it runs the steps below; state in `.onyx-testnet/storage-sepolia.json`).
The public endpoint answers `eth_getLogs` for at most 200 blocks per call (THREATS STORAGE-S15-1)
and HTTP 429 past its rate limit: the five nodes share one address, so each runs one pass a minute
and keeps what it read across failed passes (STORAGE-S15-4). Many nodes: use your own endpoint.
Rehearsal: `scripts/storage-network-anvil.sh`. Plain-English overview: docs/STORAGE-NETWORK.md.

**What it is:** one new contract, `OnyxStorage` (no proxy, no owner, every parameter immutable),
next to the deployed `OnyxSignedBook` (`0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5`, CL5) and a
tONYX test token. Storage nodes are onyx-node processes with `ONYX_STORAGE_*` set.

**Money:** tONYX (no value) for deposits, bonds and pay; test ETH for gas. Measured gas on anvil:
`createDeal` about 292,000, `register` about 106,000 + `activate` about 181,000, `answer` about
171,000 with 5 nodes (about 511,000 of execution with 128), `settleMissed` about 145,000,
`recordSeed` about 46,000.

**Who:** the operator with the deployer key in `.env` (never printed, never committed). The
deployer gets no role in the contract. The treasury is an address the owner names (a multisig
before mainnet, ROADMAP H1). Each storage node operator uses its own fresh key.
**Time:** about 30 minutes for the contract, then about 10 minutes per storage node.

## Preconditions

1. A checkout of the commit to deploy with `scripts/check-all.sh` green; `forge`, `cast` in
   `~/.foundry/bin`.
2. `.env` holds `BASE_SEPOLIA_PRIVATE_KEY` and `BASE_SEPOLIA_RPC_URL`; load it with
   `set -a; . ./.env; set +a` in the shell that runs forge; never `cat` it.
3. A tONYX token on Base Sepolia (from `DeployMarkets`, or a fresh `OnyxTestToken` whose minter
   is the deployer) and its address.
4. The owner's parameters (docs/STORAGE-NETWORK.md "Cost model"). The script's defaults are
   placeholders: one-day epochs (43,200 blocks), 0.001 tONYX per GiB per copy per epoch, 0.01
   tONYX bond per declared GiB with a 10 tONYX minimum, 10% slash, 2 checks per deal per epoch,
   jail after 3 misses.

## Steps

1. Dry run (no transaction): from `contracts/`,
   ```sh
   ONYX_STORAGE_TOKEN=<tONYX> ONYX_SIGNED_BOOK=0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5 \
   ONYX_STORAGE_TREASURY=<treasury> \
   forge script script/DeployStorage.s.sol:DeployStorage --rpc-url "$BASE_SEPOLIA_RPC_URL"
   ```
   It refuses any chain but 84532 and 31337 and a token or book address without code. Override a
   parameter with its variable (`ONYX_STORAGE_EPOCH_BLOCKS`, `ONYX_STORAGE_PRICE_PER_GIB_EPOCH`,
   `ONYX_STORAGE_BOND_PER_GIB`, `ONYX_STORAGE_MIN_BOND`, `ONYX_STORAGE_SLASH_PPM`,
   `ONYX_STORAGE_CHALLENGES`, `ONYX_STORAGE_MAX_MISSES`).
2. Deploy: the same command with `--broadcast --slow`. Note the address and the deploy block.
3. Read back: `cast call <storage> 'params()(uint64,uint128,uint128,uint128,uint32,uint32,uint32,uint256)'`
   equals the parameters asked; `cast codesize <storage>` is under 24,576; `conserved()` is true.
4. Record the address, deploy block and parameters in the deployment manifest and in
   docs/STORAGE-NETWORK.md "Status".

## Each storage node

1. On the node's server, a fresh key file (0600): `umask 077; python3 -I -c 'import secrets;
   print(secrets.token_hex(32))' > /etc/onyx/storage.key`. Fund its address with test ETH and the
   bond in tONYX.
2. Register (bond, register, activate in a later block):
   `onyx-storage register --rpc "$RPC" --chain-id 84532 --contract <storage> --from-block <deploy block>
   --key /etc/onyx/storage.key --capacity-gib <usable GiB> --url https://<node host>/`.
   It prints `NODE_ID=0x…`.
3. Configure onyx-node (docs/ops/config.md "Storage network"): `ONYX_BULK_DIR` (the disk),
   `ONYX_STORAGE_CONTRACT`, `ONYX_STORAGE_NODE_ID`, `ONYX_STORAGE_KEY_FILE`,
   `ONYX_STORAGE_FROM_BLOCK`; raise `ONYX_BULK_QUOTA_GIB` and `ONYX_BULK_MAX_GIB` to the disk. The
   public URL must be HTTPS (peers refuse plain HTTP except on loopback). Start it.
4. Check: `onyx-storage status --rpc "$RPC" --contract <storage> --from-block <deploy block> --node <id>`
   lists the node and, once a deal exists, its shards for this and the next epoch.

## A first deal

1. The publisher's dataset is anchored (docs/ops/bulk-publish.md) and served by its own node.
2. `onyx-storage deal --rpc "$RPC" --chain-id 84532 --contract <storage> --from-block <deploy block>
   --key <publisher gas key> --dir <dataset dir> --r 3 --epochs <N> --source-url https://<publisher node>/`.
3. After two epochs: `onyx-storage epoch ... --epoch <start>` shows every challenge passed.
4. When the deals end: `scripts/storage-sepolia.sh close` closes each recorded deal (the unpaid
   deposit goes back to the payer), withdraws each node's earnings and prints `conserved()`.

## Rollback

There is no admin path. To stop: no new deals; nodes `exit` (bonds back after two epochs); running
deals end at their last epoch and `close` refunds the rest. A broken contract is replaced by a new
deployment, never upgraded.
