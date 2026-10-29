// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {StdInvariant} from "forge-std/StdInvariant.sol";
import {OnyxSignedBook} from "../../src/OnyxSignedBook.sol";
import {TrackCommitmentFixture} from "../OnyxSignedBookTrack.t.sol";

/// Handler for the track-commitment log invariants (ADR-0036 amendment 3 item 4): two posters
/// post successors, forks, replays and stale or future commitments on two tracks while time
/// moves. It mirrors every log it can change and counts what the book got wrong.
contract TrackLogHandler is TrackCommitmentFixture {
    OnyxSignedBook public book;
    address[2] internal posters = [address(0xA), address(0xB)];
    bytes32[2] internal tracks = [keccak256("t0"), keccak256("t1")];

    /// Mirror per (poster, track): every hash in order, and the latest as_of.
    mapping(uint256 => bytes32[]) internal mirror;
    mapping(uint256 => uint64) internal mirrorAsOf;
    uint256 public violations;
    uint256 public appended;
    uint256 public refused;
    uint256 internal salt;

    constructor() {
        vm.warp(T0);
        vm.roll(1000);
        book = new OnyxSignedBook(address(0xBEEF));
    }

    function _slot(uint256 p, uint256 t) internal pure returns (uint256) {
        return p * 2 + t;
    }

    /// Post a commitment whose `prev` is the latest (kind 0), an older entry (1), the first
    /// again as a genesis (2) or made up (3), with `as_of` behind, at or past the block time.
    function post(uint256 p, uint256 t, uint8 kind, uint64 dt, bool future) external {
        p = bound(p, 0, 1);
        t = bound(t, 0, 1);
        uint256 s = _slot(p, t);
        bytes32[] storage m = mirror[s];
        bytes32 latest = m.length == 0 ? bytes32(0) : m[m.length - 1];
        bytes32 prev;
        kind = uint8(bound(kind, 0, 3));
        if (kind == 0) prev = latest;
        else if (kind == 1) prev = m.length > 1 ? m[m.length - 2] : keccak256("none");
        else if (kind == 2) prev = bytes32(0);
        else prev = keccak256(abi.encodePacked("made up", salt));
        uint64 asOf = future
            ? uint64(block.timestamp) + 1 + uint64(bound(dt, 0, 100))
            : uint64(block.timestamp) - uint64(bound(dt, 0, 50));
        bytes memory c = _commitment(prev, asOf, bytes32(++salt));
        bool valid = prev == latest && asOf > mirrorAsOf[s] && asOf <= block.timestamp;
        vm.prank(posters[p]);
        try book.postTrackCommitment(tracks[t], uint64(block.number - 1), c) returns (uint64 i) {
            if (!valid || i != m.length) violations++;
            m.push(_hash(c));
            mirrorAsOf[s] = asOf;
            appended++;
        } catch {
            if (valid) violations++;
            refused++;
        }
    }

    function wait(uint64 secs) external {
        vm.warp(block.timestamp + bound(secs, 1, 600));
        vm.roll(block.number + 1);
    }

    /// The book's logs equal the mirror: same length, same entries, same latest and as_of.
    function check() external view returns (uint256 bad) {
        for (uint256 p = 0; p < 2; p++) {
            for (uint256 t = 0; t < 2; t++) {
                bytes32[] storage m = mirror[_slot(p, t)];
                (uint64 n, bytes32 latest, uint64 asOf) = book.trackHead(posters[p], tracks[t]);
                if (n != m.length) bad++;
                if (asOf != mirrorAsOf[_slot(p, t)]) bad++;
                if (m.length > 0 && latest != m[m.length - 1]) bad++;
                for (uint256 i = 0; i < m.length; i++) {
                    if (book.trackCommitmentAt(posters[p], tracks[t], uint64(i)) != m[i]) bad++;
                }
            }
        }
    }
}

contract TrackLogInvariants is StdInvariant {
    TrackLogHandler internal handler;

    function setUp() public {
        handler = new TrackLogHandler();
        targetContract(address(handler));
        bytes4[] memory sel = new bytes4[](2);
        sel[0] = TrackLogHandler.post.selector;
        sel[1] = TrackLogHandler.wait.selector;
        targetSelector(FuzzSelector({addr: address(handler), selectors: sel}));
    }

    /// Only a successor of the latest with a rising, past `as_of` enters; nothing else does.
    function invariant_OnlyValidSuccessorsEnter() public view {
        require(handler.violations() == 0, "the book took a bad post or refused a good one");
    }

    /// Logs only grow and never change an entry (TR-R2-5, TR-R1-11).
    function invariant_LogsOnlyGrowAndNeverChange() public view {
        require(handler.check() == 0, "a log differs from its append-only mirror");
    }
}
