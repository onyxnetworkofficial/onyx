// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxSignedBook} from "../src/OnyxSignedBook.sol";
import {SignedBookFixture} from "./OnyxSignedBook.t.sol";

/// Builds Borsh `onyx_track::TrackCommitment` bytes: `prev`, `as_of` (u64 LE), then 248 bytes of
/// the other fields (their content does not matter to the book, only to its hash).
contract TrackCommitmentFixture is SignedBookFixture {
    function _commitment(bytes32 prev, uint64 asOf, bytes32 salt)
        internal
        pure
        returns (bytes memory c)
    {
        c = abi.encodePacked(prev, _le(asOf, 8));
        for (uint256 i = 0; i < 7; i++) {
            c = abi.encodePacked(c, keccak256(abi.encodePacked(salt, i)));
        }
        c = abi.encodePacked(c, bytes24(salt));
    }

    function _hash(bytes memory c) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("onyx-track-commitment-v1", c));
    }
}

/// ADR-0036 amendment 3 items 4-5: the book's track-commitment logs and declared operators
/// (THREATS TR-R2-5, TR-R1-11, TR-S-14, TR-R2-4, TR-C7-*).
contract OnyxSignedBookTrackTest is TrackCommitmentFixture {
    OnyxSignedBook internal book;
    address internal constant REVIEWER = address(0xBEEF);
    address internal constant POSTER = address(0xC0FFEE);
    bytes32 internal constant TRACK = keccak256("track");

    function setUp() public {
        vm.warp(T0);
        vm.roll(100);
        book = new OnyxSignedBook(REVIEWER);
    }

    function _post(bytes32 prev, uint64 asOf, bytes32 salt) internal returns (bytes32 h) {
        bytes memory c = _commitment(prev, asOf, salt);
        vm.prank(POSTER);
        book.postTrackCommitment(TRACK, 99, c);
        return _hash(c);
    }

    function test_ATrackLogIsOneChainAndItsLatestIsPublic() public {
        (uint64 n, bytes32 latest, uint64 asOf) = book.trackHead(POSTER, TRACK);
        assertEq(n, 0);
        assertEq(latest, bytes32(0));
        assertEq(asOf, 0);
        bytes memory c0 = _commitment(bytes32(0), T0 - 100, "a");
        vm.expectEmit(true, true, true, true, address(book));
        emit OnyxSignedBook.TrackCommitted(POSTER, TRACK, 0, _hash(c0), T0 - 100, 99, c0);
        vm.prank(POSTER);
        assertEq(book.postTrackCommitment(TRACK, 99, c0), 0);
        bytes32 h0 = _hash(c0);
        bytes32 h1 = _post(h0, T0 - 50, "b");
        (n, latest, asOf) = book.trackHead(POSTER, TRACK);
        assertEq(n, 2);
        assertEq(latest, h1);
        assertEq(asOf, T0 - 50);
        assertEq(book.trackCommitmentAt(POSTER, TRACK, 0), h0);
        assertEq(book.trackCommitmentAt(POSTER, TRACK, 1), h1);
        assertEq(book.trackCommitmentAt(POSTER, TRACK, 2), bytes32(0));
    }

    /// TR-R1-11, TR-S-14: a second successor of a known commitment (a fork) cannot enter the log,
    /// nor can the first commitment again.
    function test_AForkOrAReplayIsRefused() public {
        bytes32 h0 = _post(bytes32(0), T0 - 100, "a");
        _post(h0, T0 - 50, "b");
        bytes memory fork = _commitment(h0, T0 - 40, "fork");
        vm.prank(POSTER);
        vm.expectRevert(OnyxSignedBook.TrackNotChained.selector);
        book.postTrackCommitment(TRACK, 99, fork);
        bytes memory genesis = _commitment(bytes32(0), T0 - 30, "a2");
        vm.prank(POSTER);
        vm.expectRevert(OnyxSignedBook.TrackNotChained.selector);
        book.postTrackCommitment(TRACK, 99, genesis);
        // The first post of a log must be a genesis (prev zero).
        bytes memory orphan = _commitment(keccak256("elsewhere"), T0 - 30, "o");
        vm.prank(POSTER);
        vm.expectRevert(OnyxSignedBook.TrackNotChained.selector);
        book.postTrackCommitment(keccak256("other track"), 99, orphan);
    }

    /// TR-R2-5: `as_of` only rises and never passes the block time; the head is a past block.
    function test_TimeOnlyMovesForward() public {
        bytes32 h0 = _post(bytes32(0), T0 - 100, "a");
        bytes memory same = _commitment(h0, T0 - 100, "b");
        vm.prank(POSTER);
        vm.expectRevert(OnyxSignedBook.TrackTimeNotForward.selector);
        book.postTrackCommitment(TRACK, 99, same);
        bytes memory ahead = _commitment(h0, T0 + 1, "b");
        vm.prank(POSTER);
        vm.expectRevert(OnyxSignedBook.FromFuture.selector);
        book.postTrackCommitment(TRACK, 99, ahead);
        bytes memory ok = _commitment(h0, T0, "b");
        vm.prank(POSTER);
        vm.expectRevert(OnyxSignedBook.TrackHeadNotPast.selector);
        book.postTrackCommitment(TRACK, 100, ok);
        vm.prank(POSTER);
        book.postTrackCommitment(TRACK, 99, ok);
        // A first commitment at as_of 0 is refused too (as_of is a block time).
        bytes memory zero = _commitment(bytes32(0), 0, "z");
        vm.prank(POSTER);
        vm.expectRevert(OnyxSignedBook.TrackTimeNotForward.selector);
        book.postTrackCommitment(keccak256("t2"), 99, zero);
    }

    function test_MalformedPostsAreRefused() public {
        bytes memory c = _commitment(bytes32(0), T0, "a");
        vm.startPrank(POSTER);
        vm.expectRevert(OnyxSignedBook.BadTrackCommitment.selector);
        book.postTrackCommitment(bytes32(0), 99, c);
        vm.expectRevert(OnyxSignedBook.BadTrackCommitment.selector);
        book.postTrackCommitment(TRACK, 99, abi.encodePacked(c, uint8(0)));
        bytes memory short = new bytes(287);
        vm.expectRevert(OnyxSignedBook.BadTrackCommitment.selector);
        book.postTrackCommitment(TRACK, 99, short);
        vm.stopPrank();
    }

    /// Each poster keeps its own logs: nobody appends to another poster's log, and a client that
    /// pins (poster, track) reads only that one.
    function test_LogsArePerPosterAndTrack() public {
        bytes32 h0 = _post(bytes32(0), T0 - 100, "a");
        bytes memory next = _commitment(h0, T0 - 50, "b");
        vm.prank(address(0xBAD));
        vm.expectRevert(OnyxSignedBook.TrackNotChained.selector);
        book.postTrackCommitment(TRACK, 99, next);
        // Its own genesis lands in its own log and leaves POSTER's untouched.
        vm.prank(address(0xBAD));
        book.postTrackCommitment(TRACK, 99, _commitment(bytes32(0), T0 - 10, "spam"));
        (uint64 n, bytes32 latest,) = book.trackHead(POSTER, TRACK);
        assertEq(n, 1);
        assertEq(latest, h0);
    }

    /// TR-R2-4: the reviewer declares a source's operator once, before its first key.
    function test_AnOperatorIsDeclaredOnceBeforeTheFirstKey() public {
        bytes32 op = keccak256("operator one");
        vm.expectRevert(OnyxSignedBook.NotReviewer.selector);
        book.declareOperator("example.affiliate", op, "x");
        vm.startPrank(REVIEWER);
        vm.expectRevert(OnyxSignedBook.BadOperator.selector);
        book.declareOperator("example.affiliate", bytes32(0), "x");
        vm.expectRevert(OnyxSignedBook.BadOperator.selector);
        book.declareOperator("", op, "x");
        book.declareOperator("example.affiliate", op, "same operator as example.signed");
        assertEq(book.operatorOf(keccak256("example.affiliate")), op);
        vm.expectRevert(OnyxSignedBook.OperatorSet.selector);
        book.declareOperator("example.affiliate", keccak256("other"), "regroup");
        // A source with a key can no longer be grouped: its reports and statements already exist.
        book.registerKey(SOURCE, vm.addr(0xA11CE), ORIGIN, PROV, "setup");
        vm.expectRevert(OnyxSignedBook.SourceHasKeys.selector);
        book.declareOperator(SOURCE, op, "late");
        vm.stopPrank();
        assertEq(book.operatorOf(keccak256(bytes(SOURCE))), bytes32(0));
    }

    function test_GasPostTrackCommitment() public {
        bytes32 h0 = _post(bytes32(0), T0 - 100, "a");
        bytes memory c = _commitment(h0, T0 - 50, "b");
        vm.prank(POSTER);
        book.postTrackCommitment(TRACK, 99, c);
    }

    /// The book hashes Rust's Borsh `TrackCommitment` bytes to Rust's `TrackCommitment::hash`
    /// and reads its `prev` and `as_of` (fixtures/track/commitment-vectors.json, written by
    /// `onyx_track::chain` tests).
    function test_RustCommitmentVectors() public {
        string memory json = vm.readFile("../fixtures/track/commitment-vectors.json");
        uint256 n = vm.parseJsonUint(json, ".count");
        assertEq(n, 3, "three chained commitments");
        for (uint256 i = 0; i < n; i++) {
            string memory k = string.concat(".commitments[", vm.toString(i), "]");
            bytes memory c = vm.parseJsonBytes(json, string.concat(k, ".borsh"));
            bytes32 h = vm.parseJsonBytes32(json, string.concat(k, ".hash"));
            uint64 asOf = uint64(vm.parseJsonUint(json, string.concat(k, ".asOf")));
            vm.warp(asOf);
            vm.prank(POSTER);
            assertEq(book.postTrackCommitment(TRACK, 99, c), i);
            (uint64 count, bytes32 latest, uint64 at) = book.trackHead(POSTER, TRACK);
            assertEq(count, i + 1);
            assertEq(latest, h, "Rust hash");
            assertEq(at, asOf, "Rust as_of");
        }
    }

    /// Whatever sequence of posts arrives, the log is one chain with rising `as_of` and no entry
    /// ever changes.
    function testFuzz_ATrackLogOnlyGrows(uint64[8] memory times, uint8 picks) public {
        bytes32[] memory seen = new bytes32[](8);
        uint64 count;
        uint64 lastAsOf;
        bytes32 latest;
        for (uint256 i = 0; i < 8; i++) {
            uint64 asOf = uint64(bound(times[i], 0, T0));
            // Half the posts chain to the latest, half to an older or a made-up hash.
            bytes32 prev =
                (picks >> i) & 1 == 0 ? latest : (i > 0 ? seen[i / 2] : keccak256("made up"));
            bytes memory c = _commitment(prev, asOf, bytes32(i));
            vm.prank(POSTER);
            try book.postTrackCommitment(TRACK, 99, c) returns (uint64 idx) {
                assertEq(prev, latest, "only a successor of the latest enters");
                assertGt(asOf, lastAsOf, "as_of rises");
                assertEq(idx, count);
                seen[count] = _hash(c);
                latest = seen[count];
                lastAsOf = asOf;
                count++;
            } catch {
                assertTrue(prev != latest || asOf <= lastAsOf, "refused a valid successor");
            }
            for (uint64 j = 0; j < count; j++) {
                assertEq(book.trackCommitmentAt(POSTER, TRACK, j), seen[j], "entries never change");
            }
        }
        (uint64 n, bytes32 head, uint64 at) = book.trackHead(POSTER, TRACK);
        assertEq(n, count);
        assertEq(head, latest);
        assertEq(at, lastAsOf);
    }
}
