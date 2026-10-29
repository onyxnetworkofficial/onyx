// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";
import {OnyxSignedBook} from "../src/OnyxSignedBook.sol";

/// @notice Deploys `OnyxSignedBook` (ADR-0028 amendments 6 and 7, ROADMAP C10): the level-3 book
/// of record, a contract of its own with no link to the settlement, so it is deployed next to a
/// running ledger without a redeploy. Runs only on Base Sepolia (84532) or a local anvil (31337).
/// The deployer key is read from the environment at run time (BASE_SEPOLIA_PRIVATE_KEY) and never
/// logged. Input: ONYX_SIGNED_REVIEWER (required), the one immutable key that registers, retires,
/// compromises, reinstates and resolves (a multisig or timelock before mainnet: ROADMAP H1). It
/// may never be the deployer, which is the settlement's coordinator (THREATS C10-1); the book's
/// constructor refuses its sender as reviewer too. Driven by scripts/deploy-signed-book.sh;
/// runbook docs/ops/runbooks/deploy-signed-book.md.
contract DeploySignedBook is Script {
    uint256 internal constant BASE_SEPOLIA = 84_532;
    uint256 internal constant ANVIL = 31_337;

    error WrongChain(uint256 chainId);
    error ChainMismatch(uint256 rpcChainId, uint256 scriptChainId);
    error NoReviewer();
    error ReviewerIsDeployer();
    error ReviewerNotSet(address want, address got);

    function rpcChainId() public returns (uint256 id) {
        bytes memory raw = vm.rpc("eth_chainId", "[]");
        for (uint256 i = 0; i < raw.length; i++) {
            id = (id << 8) | uint8(raw[i]);
        }
    }

    /// The reviewer rules, checked before anything is signed: set, and not the deployer.
    function checkReviewer(address reviewer, address deployer) public pure {
        if (reviewer == address(0)) revert NoReviewer();
        if (reviewer == deployer) revert ReviewerIsDeployer();
    }

    function run() external returns (OnyxSignedBook book) {
        uint256 real = rpcChainId();
        if (real != block.chainid) revert ChainMismatch(real, block.chainid);
        if (real != BASE_SEPOLIA && real != ANVIL) revert WrongChain(real);
        uint256 key = vm.envUint("BASE_SEPOLIA_PRIVATE_KEY");
        address deployer = vm.addr(key);
        address reviewer = vm.envAddress("ONYX_SIGNED_REVIEWER");
        checkReviewer(reviewer, deployer);
        vm.startBroadcast(key);
        book = deploy(reviewer);
        vm.stopBroadcast();
    }

    function deploy(address reviewer) public returns (OnyxSignedBook book) {
        if (block.chainid != BASE_SEPOLIA && block.chainid != ANVIL) {
            revert WrongChain(block.chainid);
        }
        book = new OnyxSignedBook(reviewer);
        // Read back: the immutable is what was asked for.
        if (book.reviewer() != reviewer) revert ReviewerNotSet(reviewer, book.reviewer());
    }
}
