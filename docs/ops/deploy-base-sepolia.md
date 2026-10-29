# Deploy the settlement contracts to Base Sepolia

The whole M8 run (deploy, funding, a round, serving, a paid request) is one command:
`scripts/demo-testnet.sh testnet` (docs/ops/demo-testnet.md); `--plan` first prints the exact
budget. This page is the manual deploy and its checks.

Testnet only. Base Sepolia chain id is 84532. The script refuses every other chain except a
local anvil (31337). It deploys a test token (MockUSDC: free mint, no value), the linked
`OnyxRound` library (forge deploys it first), `OnyxDisputes` and `OnyxSettlement` (whose
constructor creates its extension `OnyxSettlementExt`), then binds the dispute game to the
settlement (ADR-0024). The deployer is the coordinator. Nothing here touches a mainnet or real funds.

## Before you start (the project owner, about 10 minutes)
1. Make a fresh wallet used only for this testnet. Never reuse a key that holds real funds.
2. Copy the env file and fill in the key (the file is git-ignored; never commit it):
   ```sh
   cp .env.example .env
   # edit .env: BASE_SEPOLIA_PRIVATE_KEY=0x...   (keep BASE_SEPOLIA_RPC_URL=https://sepolia.base.org)
   ```
3. Fund the wallet with a little Base Sepolia ETH from a faucet you choose (Claude does not sign
   up for faucets). `scripts/demo-testnet.sh testnet --plan` prints the exact need at current
   fees (0.00135 ETH for the whole M8 run on 2026-10-10); 0.01 test ETH covers it with room.

## Deploy
From the repo root. Step 1 checks the node really is Base Sepolia before anything is signed.
Steps 2-3 run in a subshell `( ... )`, so the key is loaded only for that one command and is
gone from your shell afterwards (it is never exported to later commands).
1. Check the chain (must print `84532`):
   ```sh
   ( set -a; . ./.env; set +a; ~/.foundry/bin/cast chain-id --rpc-url "$BASE_SEPOLIA_RPC_URL" )
   ```
2. Dry run (no transactions sent):
   ```sh
   ( set -a; . ./.env; set +a; cd contracts && \
     ~/.foundry/bin/forge script script/Deploy.s.sol:Deploy --rpc-url "$BASE_SEPOLIA_RPC_URL" )
   ```
3. Deploy:
   ```sh
   ( set -a; . ./.env; set +a; cd contracts && \
     ~/.foundry/bin/forge script script/Deploy.s.sol:Deploy --rpc-url "$BASE_SEPOLIA_RPC_URL" --broadcast )
   ```
Do not pass `--chain`: the script asks the node for its chain id (`eth_chainId`) and refuses
when it differs from what the script runs as, or is not 84532 (Base Sepolia) or 31337 (anvil).

Optional overrides (defaults in `Deploy.defaults()`): `ONYX_DOMAIN` (onyx.testnet),
`ONYX_UNBOND_DELAY` (1800 blocks), `ONYX_MIN_STAKE` (100e6), `ONYX_MIN_COORD_BOND` (100e6),
`ONYX_CANARY_SLASH` (10e6). Amounts are MockUSDC base units (6 decimals). A value too large for
its field is refused (`ParamOutOfRange`) rather than silently truncated.

## After deploy: verification checklist
1. Note the addresses from the script output (MockUSDC, OnyxDisputes, OnyxSettlement, and the
   OnyxRound library) (also in `contracts/broadcast/`, which is
   git-ignored for every network; never commit it).
2. Set the public RPC and the addresses in your shell (no secrets here):
   `RPC=https://sepolia.base.org; ST=<settlement>; DS=<disputes>; CAST=~/.foundry/bin/cast`
3. `$CAST call $ST "token()(address)" --rpc-url $RPC` returns the MockUSDC address.
4. `$CAST call $ST "coordinator()(address)" --rpc-url $RPC` returns your deployer address.
5. `$CAST call $ST "domain()(string)" --rpc-url $RPC` returns `onyx.testnet`.
6. `$CAST call $ST "ledgerId()(bytes32)" --rpc-url $RPC` equals
   `keccak256("onyx-ledger-evm-v1" || uint256(84532) as 32 bytes big-endian || <settlement>)`;
   the Rust side computes the same with `onyx_core::evm_ledger_id(84532, <settlement>)` (used in
   every round tag). A u64 encoding of the chain id gives a different hash.
7. `$CAST codesize $ST --rpc-url $RPC` is at most 24576 (checked in CI by `forge build --sizes`);
   the same for `$($CAST call $ST "ext()(address)" --rpc-url $RPC)` and `$DS`.
8. The pair is bound both ways: `$CAST call $ST "disputes()(address)" --rpc-url $RPC` returns
   `$DS`, and `$CAST call $DS "settlement()(address)" --rpc-url $RPC` returns `$ST`. Until it
   is, `openRound` refuses (`NotDisputes`).
9. Record the addresses in STATUS.md and in the website config (M7/M8); do not record the key.

## If something goes wrong
- `WrongChain(...)`: the RPC is not Base Sepolia. Check `BASE_SEPOLIA_RPC_URL`.
- `ChainMismatch(rpc, script)`: a `--chain` flag or a misconfigured RPC disagrees with the node.
  Remove `--chain` and check the URL.
- `ParamOutOfRange(name, value)`: an override does not fit its field; fix the value in `.env`.
- Out of funds: top up from the faucet and rerun; a failed deploy costs only test ETH.

## Revenue contract (ADR-0025), after the settlement
`OnyxRevenue` is the API's x402 `payTo`; it splits each fee among the shard's miners,
validators, label payee and the treasury. Same subshell pattern; the deployer becomes the
reporter. `ONYX_USDC` is the EIP-3009 token x402 pays in (confirm the Base Sepolia USDC address
against Circle's list first; configuration, not code).
```sh
( set -a; . ./.env; set +a; cd contracts && ONYX_SETTLEMENT=<settlement> ONYX_USDC=<usdc> \
  ~/.foundry/bin/forge script script/DeployRevenue.s.sol:DeployRevenue --rpc-url "$BASE_SEPOLIA_RPC_URL" --broadcast )
```
Optional: `ONYX_TREASURY` (default the deployer; the treasury may replace the reporter with `setReporter`, so use a key other than the deployer if the reporter key could leak), `ONYX_REVENUE_BPS="5000,2000,1000,2000"`.

`OnyxRegistry` (ADR-0030) holds each metric's one template, the coordinator's preflight
attestations and one level-2 result per fact. Same subshell pattern. `ONYX_REGISTRY_OWNER` is
required and must NOT be the deployer, which is the settlement's coordinator (REG-R1-F3; the
constructor also refuses the coordinator). The owner registers metrics and label-set levels, once
each, and can never change them. `scripts/demo-testnet.sh` makes this key itself
(`.onyx-testnet/keys/registry-owner.key`) and funds it with gas only.
```sh
( set -a; . ./.env; set +a; cd contracts && ONYX_SETTLEMENT=<settlement> ONYX_REGISTRY_OWNER=<owner> \
  ~/.foundry/bin/forge script script/DeployRegistry.s.sol:DeployRegistry --rpc-url "$BASE_SEPOLIA_RPC_URL" --broadcast )
```
Check: `$CAST call <registry> "ledger()(address)" --rpc-url $RPC` returns the settlement. Before
each `openRound`, the owner registers the shard's label set level (`registerLabelSet`, once), and
the coordinator calls `attestShard` with the shard-id preimage for every shard (the chain recomputes
the shard id; a later call is refused); consumers verify rows with `verifyRowLeveled`, which also
returns the attested level and the metric.
Check: `$CAST call <revenue> "ledger()(address)" --rpc-url $RPC` returns the settlement, and
`"minersBps()(uint16)"` ... `"treasuryBps()(uint16)"` sum to 10000. Then run the API with
`pay_to` set to the revenue address (its `/v1/health` reports `payTo`). To settle payments into
`OnyxRevenue` directly (ADR-0026), give the API `self_settle::SelfSettle` (the reporter key, the
revenue and USDC addresses) as its facilitator and its `receipts()` log as `ApiState::receipts`;
the demo driver does this in `--mode testnet`.

## Signed book (ADR-0028 amendments 6 and 7, ROADMAP C10)
`OnyxSignedBook` (the level-3 book) is deployed on its own, next to the running ledger, with
`scripts/deploy-signed-book.sh testnet --reviewer <addr> --manifest <deployment.json> --plan`, then
`--yes`. The reviewer must not be the deployer (script, constructor and node refuse it). Lead only:
[runbooks/deploy-signed-book.md](runbooks/deploy-signed-book.md).
