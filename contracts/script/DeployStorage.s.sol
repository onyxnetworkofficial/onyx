// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";
import {OnyxStorage, IStorageToken} from "../src/OnyxStorage.sol";
import {OnyxSignedBook} from "../src/OnyxSignedBook.sol";

/// @notice Deploys `OnyxStorage` (ADR-0060, docs/STORAGE-NETWORK.md) next to a deployed tONYX
/// test token and `OnyxSignedBook`. Runs only on Base Sepolia (84532) or a local anvil (31337).
/// The deployer key is read from the environment at run time (BASE_SEPOLIA_PRIVATE_KEY) and never
/// logged; the deployer gets no role (the contract has no owner). Inputs: ONYX_STORAGE_TOKEN,
/// ONYX_SIGNED_BOOK and ONYX_STORAGE_TREASURY (required); the parameters below are placeholders the
/// owner sets (docs/STORAGE-NETWORK.md "Cost model"), each overridable by its ONYX_STORAGE_* env
/// variable. Runbook: docs/ops/runbooks/deploy-storage.md.
contract DeployStorage is Script {
    uint256 internal constant BASE_SEPOLIA = 84_532;
    uint256 internal constant ANVIL = 31_337;

    /// One epoch: 43,200 Base blocks of 2 s (one day).
    uint64 public constant EPOCH_BLOCKS = 43_200;
    /// Placeholder price: 0.001 tONYX per GiB per replica per epoch.
    uint128 public constant PRICE_PER_GIB_EPOCH = 1e15;
    /// Placeholder bond: 0.01 tONYX per declared GiB, at least 10 tONYX.
    uint128 public constant BOND_PER_GIB = 1e16;
    uint128 public constant MIN_BOND = 10e18;
    /// A failed or missed answer takes 10% of the bond; 3 in a row jail the node.
    uint32 public constant SLASH_PPM = 100_000;
    uint32 public constant CHALLENGES_PER_DEAL = 2;
    uint32 public constant MAX_MISSES = 3;

    struct Params {
        uint64 epochBlocks;
        uint128 price;
        uint128 bondPerGiB;
        uint128 minBond;
        uint32 slashPpm;
        uint32 challenges;
        uint32 maxMisses;
    }

    error WrongChain(uint256 chainId);
    error ChainMismatch(uint256 rpcChainId, uint256 scriptChainId);
    error NoCode(address at);

    function rpcChainId() public returns (uint256 id) {
        bytes memory raw = vm.rpc("eth_chainId", "[]");
        for (uint256 i = 0; i < raw.length; i++) {
            id = (id << 8) | uint8(raw[i]);
        }
    }

    function defaults() public pure returns (Params memory) {
        return Params(
            EPOCH_BLOCKS,
            PRICE_PER_GIB_EPOCH,
            BOND_PER_GIB,
            MIN_BOND,
            SLASH_PPM,
            CHALLENGES_PER_DEAL,
            MAX_MISSES
        );
    }

    function fromEnv() public view returns (Params memory p) {
        p = defaults();
        p.epochBlocks = uint64(vm.envOr("ONYX_STORAGE_EPOCH_BLOCKS", uint256(p.epochBlocks)));
        p.price = uint128(vm.envOr("ONYX_STORAGE_PRICE_PER_GIB_EPOCH", uint256(p.price)));
        p.bondPerGiB = uint128(vm.envOr("ONYX_STORAGE_BOND_PER_GIB", uint256(p.bondPerGiB)));
        p.minBond = uint128(vm.envOr("ONYX_STORAGE_MIN_BOND", uint256(p.minBond)));
        p.slashPpm = uint32(vm.envOr("ONYX_STORAGE_SLASH_PPM", uint256(p.slashPpm)));
        p.challenges = uint32(vm.envOr("ONYX_STORAGE_CHALLENGES", uint256(p.challenges)));
        p.maxMisses = uint32(vm.envOr("ONYX_STORAGE_MAX_MISSES", uint256(p.maxMisses)));
    }

    function run() external returns (OnyxStorage s) {
        uint256 real = rpcChainId();
        if (real != block.chainid) revert ChainMismatch(real, block.chainid);
        uint256 key = vm.envUint("BASE_SEPOLIA_PRIVATE_KEY");
        address token = vm.envAddress("ONYX_STORAGE_TOKEN");
        address book = vm.envAddress("ONYX_SIGNED_BOOK");
        address treasury = vm.envAddress("ONYX_STORAGE_TREASURY");
        Params memory p = fromEnv();
        vm.startBroadcast(key);
        s = deploy(token, book, treasury, p);
        vm.stopBroadcast();
    }

    function deploy(address token, address book, address treasury, Params memory p)
        public
        returns (OnyxStorage s)
    {
        if (block.chainid != BASE_SEPOLIA && block.chainid != ANVIL) {
            revert WrongChain(block.chainid);
        }
        if (token.code.length == 0) revert NoCode(token);
        if (book.code.length == 0) revert NoCode(book);
        s = new OnyxStorage(
            IStorageToken(token),
            OnyxSignedBook(book),
            treasury,
            p.epochBlocks,
            p.price,
            p.bondPerGiB,
            p.minBond,
            p.slashPpm,
            p.challenges,
            p.maxMisses
        );
    }
}
