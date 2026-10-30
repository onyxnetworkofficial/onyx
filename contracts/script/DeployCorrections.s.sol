// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";
import {OnyxCorrections} from "../src/OnyxCorrections.sol";
import {OnyxRegistry} from "../src/OnyxRegistry.sol";
import {IStakeToken} from "../src/IOnyxMarkets.sol";

/// @notice Deploys `OnyxCorrections` (CL7, ADR-0057) next to a deployed `OnyxRegistry` and the
/// markets' tONYX test token. Runs only on Base Sepolia (84532) or a local anvil (31337). The
/// deployer key is read from the environment at run time (BASE_SEPOLIA_PRIVATE_KEY) and never
/// logged; the deployer owns the contract (it only pairs metrics; it never decides a market) and
/// is the treasury that open fees and trading fees go to (testnet only). Inputs: ONYX_REGISTRY,
/// ONYX_TONYX (both required). Steps: docs/ops/runbooks/corrections-base-sepolia.md.
contract DeployCorrections is Script {
    uint256 internal constant BASE_SEPOLIA = 84_532;
    uint256 internal constant ANVIL = 31_337;

    error WrongChain(uint256 chainId);
    error ChainMismatch(uint256 rpcChainId, uint256 scriptChainId);

    function rpcChainId() public returns (uint256 id) {
        bytes memory raw = vm.rpc("eth_chainId", "[]");
        for (uint256 i = 0; i < raw.length; i++) {
            id = (id << 8) | uint8(raw[i]);
        }
    }

    function run() external returns (OnyxCorrections c) {
        uint256 real = rpcChainId();
        if (real != block.chainid) revert ChainMismatch(real, block.chainid);
        uint256 key = vm.envUint("BASE_SEPOLIA_PRIVATE_KEY");
        address deployer = vm.addr(key);
        OnyxRegistry registry = OnyxRegistry(vm.envAddress("ONYX_REGISTRY"));
        IStakeToken token = IStakeToken(vm.envAddress("ONYX_TONYX"));
        vm.startBroadcast(key);
        c = deploy(token, registry, deployer);
        vm.stopBroadcast();
    }

    function deploy(IStakeToken token, OnyxRegistry registry, address treasury)
        public
        returns (OnyxCorrections)
    {
        if (block.chainid != BASE_SEPOLIA && block.chainid != ANVIL) {
            revert WrongChain(block.chainid);
        }
        return new OnyxCorrections(token, registry, treasury, defaults());
    }

    /// Placeholders until measured round costs and the partners' numbers (ADR-0057 open
    /// question 1, NS12), in Base Sepolia blocks of 2 s: the open fee covers a recheck round
    /// (about 15 transactions); trading lasts at least an hour; the recheck gets at least a day;
    /// no market outlives 30 days; a recorded recheck can be resolved for a day after the
    /// deadline before anyone may void it.
    function defaults() public pure returns (OnyxCorrections.Params memory) {
        return OnyxCorrections.Params({
            openFee: 5e18,
            minBet: 1e18,
            maxBetPerAddress: 100e18,
            maxPoolPerMarket: 10_000e18,
            feePpm: 20_000,
            minTradingBlocks: 1_800,
            minRecheckBlocks: 43_200,
            maxMarketBlocks: 1_296_000,
            graceBlocks: 43_200
        });
    }
}
