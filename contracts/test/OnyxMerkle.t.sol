// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxMerkle} from "../src/OnyxMerkle.sol";

contract OnyxMerkleTest is Test {
    /// Fields in alphabetical order, as vm.parseJson requires.
    struct Case {
        uint256 index;
        uint256 leafCount;
        bytes32 root;
        bytes row;
        bytes32[] siblings;
    }

    string internal json;

    function setUp() public {
        json = vm.readFile("../fixtures/merkle/vectors.json");
    }

    function _case(uint256 i)
        internal
        view
        returns (bytes32 root, bytes memory row, uint64 index, uint64 n, bytes32[] memory sibs)
    {
        string memory p = string.concat(".cases[", vm.toString(i), "]");
        root = vm.parseJsonBytes32(json, string.concat(p, ".root"));
        row = vm.parseJsonBytes(json, string.concat(p, ".row"));
        index = uint64(vm.parseJsonUint(json, string.concat(p, ".index")));
        n = uint64(vm.parseJsonUint(json, string.concat(p, ".leafCount")));
        sibs = vm.parseJsonBytes32Array(json, string.concat(p, ".siblings"));
    }

    /// Every Rust-generated proof verifies in Solidity: the two layouts agree.
    function test_RustVectorsVerify() public view {
        Case[] memory cases = abi.decode(vm.parseJson(json, ".cases"), (Case[]));
        assertEq(cases.length, vm.parseJsonUint(json, ".count"));
        assertGt(cases.length, 300);
        for (uint256 i = 0; i < cases.length; i++) {
            Case memory c = cases[i];
            assertTrue(
                OnyxMerkle.verify(c.root, c.row, uint64(c.index), uint64(c.leafCount), c.siblings),
                vm.toString(i)
            );
        }
    }

    function test_WrongRowFails() public view {
        (bytes32 root,, uint64 index, uint64 n, bytes32[] memory sibs) = _case(10);
        assertFalse(OnyxMerkle.verify(root, bytes("not-a-row"), index, n, sibs));
    }

    function test_WrongCountFails() public view {
        (bytes32 root, bytes memory row, uint64 index, uint64 n, bytes32[] memory sibs) = _case(10);
        assertFalse(OnyxMerkle.verify(root, row, index, n + 1, sibs));
    }

    /// Fuzz: random index, count and siblings never verify a fixed row against a vector root.
    function testFuzz_RandomProofRejected(uint64 index, uint64 n, bytes32[] memory sibs)
        public
        view
    {
        (bytes32 root, bytes memory row, uint64 realIndex, uint64 realN,) = _case(10);
        vm.assume(index != realIndex || n != realN);
        assertFalse(OnyxMerkle.verify(root, row, index, n, sibs));
    }

    /// R-TEST-1: random siblings at the true index and count never verify,
    /// for every vector case.
    function testFuzz_RandomSiblingsAtTrueIndexRejected(uint256 pick, bytes32[] memory sibs)
        public
        view
    {
        uint256 count = vm.parseJsonUint(json, ".count");
        (bytes32 root, bytes memory row, uint64 index, uint64 n, bytes32[] memory real) =
            _case(pick % count);
        vm.assume(keccak256(abi.encode(sibs)) != keccak256(abi.encode(real)));
        assertFalse(OnyxMerkle.verify(root, row, index, n, sibs));
    }

    /// R-TEST-2: same length as the real proof, one sibling changed in one bit.
    function testFuzz_OneBitSiblingFlipRejected(uint256 pick, uint256 which, uint8 bit)
        public
        view
    {
        uint256 count = vm.parseJsonUint(json, ".count");
        (bytes32 root, bytes memory row, uint64 index, uint64 n, bytes32[] memory sibs) =
            _case(pick % count);
        vm.assume(sibs.length > 0);
        uint256 k = which % sibs.length;
        sibs[k] = sibs[k] ^ bytes32(uint256(1) << bit);
        assertFalse(OnyxMerkle.verify(root, row, index, n, sibs));
    }

    function testFuzz_IndexOutOfRangeRejected(bytes32 root, bytes32 leaf, uint64 n, uint64 extra)
        public
        pure
    {
        uint64 index = uint64(bound(extra, n, type(uint64).max));
        bytes32[] memory sibs = new bytes32[](0);
        assertFalse(OnyxMerkle.verifyLeaf(root, leaf, index, n, sibs));
    }
}
