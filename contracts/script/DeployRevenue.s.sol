// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";
import {OnyxRevenue, IOnyxLedgerView, IRevenueToken} from "../src/OnyxRevenue.sol";

/// @notice Deploys `OnyxRevenue` (ADR-0025) next to an already deployed settlement: the API's
/// x402 `payTo`. Runs only on Base Sepolia (84532) or a local anvil (31337). The deployer key is
/// read from the environment at run time (BASE_SEPOLIA_PRIVATE_KEY) and never logged; the
/// deployer becomes the reporter. Inputs: ONYX_SETTLEMENT (settlement address), ONYX_USDC (the
/// EIP-3009 token x402 pays in; configuration, confirm against Circle's list), optional
/// ONYX_TREASURY (default: the deployer; the treasury owns `setReporter`, so set it to another key
/// for a rotation that means something, REV-L1-F4). Shares: 50/20/10/20 (miners, validators, labels,
/// treasury), overridable with ONYX_REVENUE_BPS="m,v,l,t". See docs/ops/deploy-base-sepolia.md.
contract DeployRevenue is Script {
    uint256 internal constant BASE_SEPOLIA = 84_532;
    uint256 internal constant ANVIL = 31_337;

    error WrongChain(uint256 chainId);
    error ChainMismatch(uint256 rpcChainId, uint256 scriptChainId);
    error BadBps();

    function rpcChainId() public returns (uint256 id) {
        bytes memory raw = vm.rpc("eth_chainId", "[]");
        for (uint256 i = 0; i < raw.length; i++) {
            id = (id << 8) | uint8(raw[i]);
        }
    }

    function shares() public view returns (uint16[4] memory bps) {
        bps = [uint16(5_000), 2_000, 1_000, 2_000];
        uint256[] memory def = new uint256[](0);
        uint256[] memory o = vm.envOr("ONYX_REVENUE_BPS", ",", def);
        if (o.length == 0) return bps;
        if (o.length != 4) revert BadBps();
        for (uint256 i = 0; i < 4; i++) {
            if (o[i] > 10_000) revert BadBps();
            bps[i] = uint16(o[i]);
        }
    }

    function run() external returns (OnyxRevenue revenue) {
        uint256 real = rpcChainId();
        if (real != block.chainid) revert ChainMismatch(real, block.chainid);
        if (real != BASE_SEPOLIA && real != ANVIL) revert WrongChain(real);
        uint256 key = vm.envUint("BASE_SEPOLIA_PRIVATE_KEY");
        address deployer = vm.addr(key);
        address settlement = vm.envAddress("ONYX_SETTLEMENT");
        address usdc = vm.envAddress("ONYX_USDC");
        address treasury = vm.envOr("ONYX_TREASURY", deployer);
        uint16[4] memory bps = shares();
        vm.startBroadcast(key);
        revenue = deploy(usdc, settlement, deployer, treasury, bps);
        vm.stopBroadcast();
    }

    function deploy(
        address usdc,
        address settlement,
        address reporter,
        address treasury,
        uint16[4] memory bps
    ) public returns (OnyxRevenue) {
        if (block.chainid != BASE_SEPOLIA && block.chainid != ANVIL) {
            revert WrongChain(block.chainid);
        }
        return
            new OnyxRevenue(
                IRevenueToken(usdc), IOnyxLedgerView(settlement), reporter, treasury, bps
            );
    }
}
