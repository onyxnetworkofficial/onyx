// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxSignedBook} from "../src/OnyxSignedBook.sol";

/// fixtures/signed/vectors.json (written by crates/onyx-coordinator/tests/level3_chain_book.rs):
/// the contract computes Rust's key id, payload digest, fact key, value hash and report hash from
/// the Borsh bytes a source signed, for every `Subject` and `Value` variant, and refuses the
/// malformed payloads Rust refuses (ADR-0028 amendment 6).
contract OnyxSignedBookVectorsTest is Test {
    /// Field order is alphabetical: `vm.parseJson` decodes objects in key order.
    struct Case {
        bytes32 digest;
        bytes32 fact;
        uint256 observed;
        bytes payload;
        bytes32 reportHash;
        uint256 sequence;
        bytes signature;
        bytes32 valueHash;
    }

    struct Bad {
        string name;
        bytes payload;
    }

    function test_RustVectorsParseAndHashAlike() public {
        string memory json = vm.readFile("../fixtures/signed/vectors.json");
        // The reviewer is never the deployer (C10): another account deploys.
        vm.prank(address(0xDE9));
        OnyxSignedBook book = new OnyxSignedBook(address(this));
        address signer = vm.parseJsonAddress(json, ".signer");
        bytes32 id = book.registerKey(
            vm.parseJsonString(json, ".source"),
            signer,
            vm.parseJsonString(json, ".origin"),
            keccak256("keys document"),
            "vectors"
        );
        assertEq(id, vm.parseJsonBytes32(json, ".keyId"), "key id");
        Case[] memory cases = abi.decode(vm.parseJson(json, ".cases"), (Case[]));
        assertEq(cases.length, 24, "4 subjects x 6 values");
        for (uint256 i = 0; i < cases.length; i++) {
            Case memory c = cases[i];
            vm.warp(c.observed);
            (OnyxSignedBook.Outcome o, bytes32 fact) = book.postReport(c.payload, c.signature);
            assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Accepted), "accepted");
            assertEq(fact, c.fact, "fact key");
            OnyxSignedBook.Rep memory r = book.report(c.digest);
            assertEq(r.key, id, "recorded under its digest");
            assertEq(r.fact, c.fact);
            assertEq(r.valueHash, c.valueHash, "value hash");
            assertEq(r.reportHash, c.reportHash, "report hash");
            assertEq(r.observed, c.observed);
            assertEq(r.sequence, c.sequence);
            assertEq(book.sequenceOf(id, uint64(c.sequence)), c.digest);
        }
        Bad[] memory bad = abi.decode(vm.parseJson(json, ".bad"), (Bad[]));
        assertEq(bad.length, 12, "malformed cases");
        for (uint256 i = 0; i < bad.length; i++) {
            vm.expectRevert(OnyxSignedBook.BadPayload.selector);
            book.postReport(bad[i].payload, new bytes(65));
        }
    }
}
