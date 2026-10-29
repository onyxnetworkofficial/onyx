// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxMerkle} from "../src/OnyxMerkle.sol";

/// The query API's settlement payload (onyx-api `GET /v1/settlement/{market}`,
/// written by adapters/source-btc/tests/api.rs from a round settled by the M4
/// code on blocks 921,080-921,082) verifies with the contract's Merkle code:
/// the API and the chain agree on rows, proofs and the root.
contract OnyxApiVectorTest is Test {
    string internal json;
    string internal multi;
    string internal merkle;

    function setUp() public {
        json = vm.readFile("../fixtures/api/settlement-vector.json");
        multi = vm.readFile("../fixtures/api/multi-leaf-vector.json");
        merkle = vm.readFile("../fixtures/merkle/vectors.json");
    }

    function _vector()
        internal
        view
        returns (bytes32 root, bytes memory row, uint64 index, uint64 n, bytes32[] memory sibs)
    {
        root = vm.parseJsonBytes32(json, ".root");
        row = vm.parseJsonBytes(json, ".row");
        index = uint64(vm.parseJsonUint(json, ".index"));
        n = uint64(vm.parseJsonUint(json, ".leafCount"));
        sibs = vm.parseJsonBytes32Array(json, ".siblings");
    }

    function test_ApiSettlementPayloadVerifies() public view {
        (bytes32 root, bytes memory row, uint64 index, uint64 n, bytes32[] memory sibs) = _vector();
        assertEq(
            root, 0xf912872a71710d45d000e4c2c3771e7f31e51e0fe3d0bad0557d8fedca3d86b2, "golden root"
        );
        assertTrue(OnyxMerkle.verify(root, row, index, n, sibs));
    }

    function test_ApiSettlementPayloadTamperedFails() public view {
        (bytes32 root, bytes memory row, uint64 index, uint64 n, bytes32[] memory sibs) = _vector();
        // One byte of the row changed: the proof no longer holds.
        row[row.length - 30] = bytes1(uint8(row[row.length - 30]) ^ 0x01);
        assertFalse(OnyxMerkle.verify(root, row, index, n, sibs));
    }

    function test_ApiSettlementPayloadWrongIndexFails() public view {
        (bytes32 root, bytes memory row, uint64 index, uint64 n, bytes32[] memory sibs) = _vector();
        assertFalse(OnyxMerkle.verify(root, row, index ^ 1, n, sibs));
    }

    function _case(uint256 i)
        internal
        view
        returns (bytes32 root, bytes memory row, uint64 index, uint64 n, bytes32[] memory sibs)
    {
        string memory k = string.concat(".cases[", vm.toString(i), "]");
        root = vm.parseJsonBytes32(multi, string.concat(k, ".root"));
        row = vm.parseJsonBytes(multi, string.concat(k, ".row"));
        index = uint64(vm.parseJsonUint(multi, string.concat(k, ".index")));
        n = uint64(vm.parseJsonUint(multi, string.concat(k, ".leafCount")));
        sibs = vm.parseJsonBytes32Array(multi, string.concat(k, ".siblings"));
    }

    /// 13 rows served by the API (onyx-api tests): every case verifies, and
    /// its proof has the same shape as the shared Merkle vector for the same
    /// (index, leafCount) in fixtures/merkle/vectors.json.
    function test_ApiMultiLeafPayloadsVerify() public view {
        for (uint256 i = 0; i < 4; i++) {
            (bytes32 root, bytes memory row, uint64 index, uint64 n, bytes32[] memory sibs) =
                _case(i);
            assertEq(n, 13, "leaf count");
            assertTrue(OnyxMerkle.verify(root, row, index, n, sibs), "verifies");
            assertFalse(OnyxMerkle.verify(root, row, index ^ 1, n, sibs), "wrong index");
            // fixtures/merkle lists leafCount 1, 2, 3, 5, 8 before 13: offset 19.
            string memory m = string.concat(".cases[", vm.toString(19 + uint256(index)), "]");
            assertEq(vm.parseJsonUint(merkle, string.concat(m, ".leafCount")), 13);
            assertEq(vm.parseJsonUint(merkle, string.concat(m, ".index")), index);
            assertEq(
                vm.parseJsonBytes32Array(merkle, string.concat(m, ".siblings")).length,
                sibs.length,
                "same proof shape as the shared Merkle vector"
            );
        }
    }

    function test_ApiMultiLeafTamperedSiblingFails() public view {
        (bytes32 root, bytes memory row, uint64 index, uint64 n, bytes32[] memory sibs) = _case(1);
        sibs[sibs.length - 1] = keccak256("forged");
        assertFalse(OnyxMerkle.verify(root, row, index, n, sibs));
    }
}
