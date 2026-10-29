// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";
import {OnyxRegistry, IRegistryLedger} from "../src/OnyxRegistry.sol";

/// @notice Deploys `OnyxRegistry` (ADR-0030) next to an already deployed settlement. Runs only on
/// Base Sepolia (84532) or a local anvil (31337). The deployer key is read from the environment at
/// run time (BASE_SEPOLIA_PRIVATE_KEY) and never logged. Inputs: ONYX_SETTLEMENT (settlement
/// address) and ONYX_REGISTRY_OWNER (required). The owner registers each metric's one template and
/// each label set's level; it is a single key on testnet (multisig or timelock: TODO REG-1) and
/// may never be the settlement's coordinator, which is the deployer here (REG-R1-F3).
contract DeployRegistry is Script {
    uint256 internal constant BASE_SEPOLIA = 84_532;
    uint256 internal constant ANVIL = 31_337;

    error WrongChain(uint256 chainId);
    error ChainMismatch(uint256 rpcChainId, uint256 scriptChainId);
    error OwnerIsDeployer();

    function rpcChainId() public returns (uint256 id) {
        bytes memory raw = vm.rpc("eth_chainId", "[]");
        for (uint256 i = 0; i < raw.length; i++) {
            id = (id << 8) | uint8(raw[i]);
        }
    }

    function run() external returns (OnyxRegistry registry) {
        uint256 real = rpcChainId();
        if (real != block.chainid) revert ChainMismatch(real, block.chainid);
        if (real != BASE_SEPOLIA && real != ANVIL) revert WrongChain(real);
        uint256 key = vm.envUint("BASE_SEPOLIA_PRIVATE_KEY");
        address deployer = vm.addr(key);
        address settlement = vm.envAddress("ONYX_SETTLEMENT");
        address owner = vm.envAddress("ONYX_REGISTRY_OWNER");
        // The deployer is the coordinator (Deploy.s.sol): it must not own the registry.
        if (owner == deployer) revert OwnerIsDeployer();
        vm.startBroadcast(key);
        registry = deploy(owner, settlement);
        vm.stopBroadcast();
    }

    function deploy(address owner, address settlement) public returns (OnyxRegistry) {
        if (block.chainid != BASE_SEPOLIA && block.chainid != ANVIL) {
            revert WrongChain(block.chainid);
        }
        return new OnyxRegistry(owner, IRegistryLedger(settlement));
    }
}
