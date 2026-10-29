// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {MockUSDC} from "./MockUSDC.sol";

/// @notice Test-only USDC with the EIP-3009 nonce state: `transferWithAuthorization` moves funds
/// as a facilitator settle would and marks the nonce used. It checks no signature (the real
/// token does; the Base Sepolia fork test covers that); the 7-argument form checks `to`'s
/// validity window and the nonce like FiatToken v2.2. Never deployed.
contract MockUSDC3009 is MockUSDC {
    mapping(address => mapping(bytes32 => bool)) public authorizationState;

    function transferWithAuthorization(address from, address to, uint256 value, bytes32 nonce)
        external
    {
        require(!authorizationState[from][nonce], "nonce used");
        authorizationState[from][nonce] = true;
        balanceOf[from] -= value;
        balanceOf[to] += value;
    }

    /// FiatToken v2.2 form (bytes signature): checks the window and nonce, not the signature.
    function transferWithAuthorization(
        address from,
        address to,
        uint256 value,
        uint256 validAfter,
        uint256 validBefore,
        bytes32 nonce,
        bytes calldata
    ) external virtual {
        require(block.timestamp > validAfter, "authorization is not yet valid");
        require(block.timestamp < validBefore, "authorization is expired");
        require(!authorizationState[from][nonce], "authorization is used or canceled");
        authorizationState[from][nonce] = true;
        balanceOf[from] -= value;
        balanceOf[to] += value;
    }
}
