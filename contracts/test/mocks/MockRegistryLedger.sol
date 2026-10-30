// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {IRegistryLedger} from "../../src/OnyxRegistry.sol";
import {OnyxLedger} from "../../src/OnyxLedger.sol";

/// @notice Test-only ledger for an `OnyxRegistry` with a fixed coordinator and no shards: enough
/// to announce level-2 draws (scripts/credits-anvil.sh relays observer posts to it). Never
/// deployed to a live network.
contract MockRegistryLedger is IRegistryLedger {
    address public immutable coordinator;

    constructor(address coordinator_) {
        coordinator = coordinator_;
    }

    function coordinatorEpoch() external pure returns (uint64) {
        return 1;
    }

    function isFinal(uint64, bytes32) external pure returns (bool) {
        return false;
    }

    function shardState(uint64, bytes32)
        external
        pure
        returns (OnyxLedger.ShardStatus, bool, uint64, bytes32, bytes32)
    {
        return (OnyxLedger.ShardStatus(0), false, 0, bytes32(0), bytes32(0));
    }

    function verifyRow(uint64, bytes32, bytes calldata, uint64, uint64, bytes32[] calldata)
        external
        pure
        returns (bool)
    {
        return false;
    }
}
