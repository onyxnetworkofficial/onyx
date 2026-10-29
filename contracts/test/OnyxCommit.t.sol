// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxCommit} from "../src/OnyxCommit.sol";

contract OnyxCommitTest is Test {
    string internal json;

    function setUp() public {
        json = vm.readFile("../fixtures/commit/vectors.json");
    }

    function _b32(uint256 i, string memory key) internal view returns (bytes32) {
        return vm.parseJsonBytes32(json, string.concat(".cases[", vm.toString(i), "].", key));
    }

    function _fields(uint256 i) internal view returns (OnyxCommit.CommitmentFields memory c) {
        c.root = _b32(i, "root");
        c.leafCount =
            uint64(vm.parseJsonUint(json, string.concat(".cases[", vm.toString(i), "].leafCount")));
        c.recipeId = _b32(i, "recipeId");
        c.runtimeHash = _b32(i, "runtimeHash");
        c.labelSetHash = _b32(i, "labelSetHash");
        c.sourceHash = _b32(i, "sourceHash");
        c.schemaId = _b32(i, "schemaId");
        c.rightsHash = _b32(i, "rightsHash");
    }

    /// Rust and Solidity compute the same binding and commit hashes (R-COMMIT-2).
    function test_RustVectorsMatch() public view {
        uint256 count = vm.parseJsonUint(json, ".count");
        assertGt(count, 0);
        for (uint256 i = 0; i < count; i++) {
            string memory p = string.concat(".cases[", vm.toString(i), "]");
            bytes32 binding = OnyxCommit.bindingHash(_fields(i));
            assertEq(binding, _b32(i, "bindingHash"), "binding");
            uint64 round = uint64(vm.parseJsonUint(json, string.concat(p, ".round")));
            address miner = vm.parseJsonAddress(json, string.concat(p, ".miner"));
            bytes32 commit =
                OnyxCommit.commitHash(round, _b32(i, "shard"), binding, miner, _b32(i, "salt"));
            assertEq(commit, _b32(i, "commitHash"), "commit");
            bytes32 fields = OnyxCommit.shardFieldsHash(_fields(i));
            assertEq(fields, _b32(i, "shardFieldsHash"), "shard fields");
            assertEq(
                OnyxCommit.shardId(fields, _b32(i, "paramsHash"), _b32(i, "registryRoot")),
                _b32(i, "shardId"),
                "shard id"
            );
            assertEq(
                OnyxCommit.challengeHash(
                    round,
                    _b32(i, "shard"),
                    _b32(i, "claimedRoot"),
                    vm.parseJsonAddress(json, string.concat(p, ".defender")),
                    vm.parseJsonAddress(json, string.concat(p, ".challenger")),
                    _b32(i, "challengeSalt")
                ),
                _b32(i, "challengeHash"),
                "challenge"
            );
        }
    }

    /// Case 3 has no recipe or runtime: zero sentinels hash the same in Rust and Solidity.
    function test_ZeroSentinelCase() public view {
        assertEq(_b32(3, "recipeId"), bytes32(0));
        assertEq(_b32(3, "runtimeHash"), bytes32(0));
    }

    /// Root and count are miner work; every other field is pinned by the shard (R-SHARD-1).
    function testFuzz_ShardFieldsIgnoreRootOnly(bytes32 root, uint64 n, bytes32 runtime)
        public
        view
    {
        OnyxCommit.CommitmentFields memory c = _fields(0);
        bytes32 pinned = OnyxCommit.shardFieldsHash(c);
        c.root = root;
        c.leafCount = n;
        assertEq(OnyxCommit.shardFieldsHash(c), pinned);
        vm.assume(runtime != c.runtimeHash);
        c.runtimeHash = runtime;
        assertTrue(OnyxCommit.shardFieldsHash(c) != pinned);
    }

    /// A commit copied from another miner never verifies for the copier.
    function testFuzz_CopiedCommitUseless(address copier) public view {
        vm.assume(copier != vm.parseJsonAddress(json, ".cases[0].miner"));
        bytes32 binding = OnyxCommit.bindingHash(_fields(0));
        uint64 round = uint64(vm.parseJsonUint(json, ".cases[0].round"));
        bytes32 c = OnyxCommit.commitHash(round, _b32(0, "shard"), binding, copier, _b32(0, "salt"));
        assertTrue(c != _b32(0, "commitHash"));
    }

    /// Changing the label set hash (or any field) changes the binding (R-BIND-1).
    function testFuzz_LabelSetBound(bytes32 other) public view {
        OnyxCommit.CommitmentFields memory c = _fields(0);
        vm.assume(other != c.labelSetHash);
        bytes32 before = OnyxCommit.bindingHash(c);
        c.labelSetHash = other;
        assertTrue(OnyxCommit.bindingHash(c) != before);
    }
}
