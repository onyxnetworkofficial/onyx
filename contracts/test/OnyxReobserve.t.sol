// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxReobserve} from "../src/OnyxReobserve.sol";

/// THREATS LC16-1: an observer change takes effect only after `changeDelay`, so the owner cannot
/// pick the observer set of a task it already sees coming. The test is the bound stake contract.
contract OnyxReobserveDelayTest is Test {
    OnyxReobserve internal r;
    address internal a = makeAddr("a");
    address internal b = makeAddr("b");
    address internal c = makeAddr("c");
    address internal d = makeAddr("d");

    /// Absolute times: under via-IR `block.timestamp` can be read once per test.
    uint256 internal constant T0 = 1_790_000_000;

    function setUp() public {
        vm.warp(T0);
        r = new OnyxReobserve(3 days);
        r.bind(address(this));
        r.addObserver(a);
        r.addObserver(b);
        r.addObserver(c);
    }

    function _open(uint256 id) internal returns (address[] memory) {
        r.open(
            id,
            keccak256("fact"),
            uint64(block.timestamp) + 1 days,
            uint64(block.timestamp) + 2 days
        );
        return r.task(id).assigned;
    }

    function test_AnObserverAddedAfterTheDelayStartsOnlyCountsForLaterTasks() public {
        assertTrue(r.isObserver(a), "an insider from the moment it is added");
        assertEq(_open(1).length, 0, "nobody is assigned before the delay");
        vm.warp(T0 + 3 days);
        assertEq(_open(2).length, 3);
        r.addObserver(d);
        address[] memory t3 = _open(3);
        assertEq(t3.length, 3, "a new observer waits");
        for (uint256 i = 0; i < t3.length; i++) {
            assertTrue(t3[i] != d);
        }
        vm.warp(T0 + 6 days);
        assertEq(_open(4).length, 4, "then counts for later tasks");
    }

    function test_ARemovedObserverStaysAssignedUntilTheDelayPasses() public {
        vm.warp(T0 + 3 days);
        r.removeObserver(a);
        assertEq(_open(1).length, 3, "still assigned during the delay");
        vm.expectRevert(OnyxReobserve.WrongPhase.selector);
        r.prune(a);
        assertTrue(r.isObserver(a), "still an insider while leaving");
        vm.warp(T0 + 6 days);
        address[] memory t2 = _open(2);
        assertEq(t2.length, 2, "gone after the delay");
        r.prune(a);
        assertFalse(r.isObserver(a));
        assertEq(r.observers().length, 2);
        r.removeObserver(b);
        vm.expectRevert(OnyxReobserve.Known.selector);
        r.removeObserver(b);
    }

    function test_WithNoDelayChangesAreImmediate() public {
        OnyxReobserve z = new OnyxReobserve(0);
        z.bind(address(this));
        z.addObserver(a);
        z.open(1, keccak256("f"), uint64(block.timestamp) + 1, uint64(block.timestamp) + 2);
        assertEq(z.task(1).assigned.length, 1);
        z.removeObserver(a);
        assertFalse(z.isObserver(a));
    }
}
