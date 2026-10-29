// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxRegistryTest} from "./OnyxRegistry.t.sol";
import {OnyxRegistry} from "../src/OnyxRegistry.sol";

/// Review L2RND-R4 (untracked): post-beacon choice is limited to sequential re-draws, capped by
/// the subject bound, whatever the coordinator pipelines or holds.
contract OnyxRegistryDrawBoundTest is OnyxRegistryTest {
    bytes32 internal constant SPEC = keccak256("spec");

    function _ann(bytes32 s, uint64 w, uint32 a, bytes32 pool) internal returns (uint64 h) {
        h = uint64(vm.getBlockNumber()) + 1;
        reg.announceDraw(FEED_METRIC, s, w, a, reg.drawCommit(pool, SPEC, CFG), h);
    }

    function _fix(bytes32 s, uint64 w, uint32 a, uint64 h) internal {
        vm.roll(h + 1);
        vm.setBlockhash(h, keccak256(abi.encode(s, w, a)));
        reg.fixBeacon(reg.factKey(FEED_METRIC, s, w), a);
    }

    /// Pipelining: three attempts announced back to back, all beacons fixed. Only the latest
    /// records or passes drawMatches; the coordinator cannot pick the best of the three.
    function test_PipelinedAttemptsOnlyTheLatestCounts() public {
        _reg();
        bytes32 s = keccak256("pipe");
        bytes32 f = reg.factKey(FEED_METRIC, s, WINDOW);
        uint64[3] memory h;
        bytes32[3] memory pools = [keccak256("p0"), keccak256("p1"), keccak256("p2")];
        for (uint32 a = 0; a < 3; a++) {
            h[a] = _ann(s, WINDOW, a, pools[a]);
            vm.roll(h[a]); // next announce must still precede its own beacon
        }
        for (uint32 a = 0; a < 3; a++) {
            _fix(s, WINDOW, a, h[a]);
        }
        for (uint32 a = 0; a < 2; a++) {
            OnyxRegistry.Draw memory d = reg.drawOf(f, a);
            assertFalse(
                reg.drawMatches(FEED_METRIC, s, WINDOW, a, d.commit, d.beaconHeight, d.beacon)
            );
            vm.expectRevert(OnyxRegistry.DrawMismatch.selector);
            reg.recordObservation(FEED_METRIC, s, WINDOW, SPEC, keccak256("r"), pools[a], CFG);
        }
        reg.recordObservation(FEED_METRIC, s, WINDOW, SPEC, keccak256("r"), pools[2], CFG);
    }

    /// Holding: draws pre-announced on other windows and recorded later to free strikes never
    /// give a target fact a 4th try. Each superseded attempt of the target is a permanent strike.
    function test_HeldDrawsOnOtherWindowsDoNotBuyAFourthTry() public {
        _reg();
        bytes32 s = keccak256("hold");
        uint64 w2 = WINDOW + 1;
        uint64 w3 = WINDOW + 2;
        _fix(s, w2, 0, _ann(s, w2, 0, POOL));
        _fix(s, w3, 0, _ann(s, w3, 0, POOL));
        // Target try 1 (strikes 3), rejected; free a strike by recording a held window.
        _fix(s, WINDOW, 0, _ann(s, WINDOW, 0, POOL));
        reg.recordObservation(FEED_METRIC, s, w2, SPEC, keccak256("r2"), POOL, CFG);
        // Target try 2, rejected; free another.
        _fix(s, WINDOW, 1, _ann(s, WINDOW, 1, POOL));
        reg.recordObservation(FEED_METRIC, s, w3, SPEC, keccak256("r3"), POOL, CFG);
        // Target try 3 (strikes 3: two superseded target attempts + this one).
        _fix(s, WINDOW, 2, _ann(s, WINDOW, 2, POOL));
        (uint32 strikes, uint32 allowance) = reg.drawBudget(FEED_METRIC, s);
        assertEq(strikes, 3);
        assertEq(allowance, 3);
        // No 4th try on the target, nor any new window: nothing left to record but try 3.
        uint64 h = uint64(vm.getBlockNumber()) + 1;
        bytes32 c = reg.drawCommit(POOL, SPEC, CFG);
        vm.expectRevert(OnyxRegistry.DrawsExhausted.selector);
        reg.announceDraw(FEED_METRIC, s, WINDOW, 3, c, h);
        vm.expectRevert(OnyxRegistry.DrawsExhausted.selector);
        reg.announceDraw(FEED_METRIC, s, WINDOW + 3, 0, c, h);
    }

    /// Beacon expiry: a draw left unfixed past 256 blocks is dead, still a strike, and its fact
    /// cannot record; letting a seen beacon lapse is just an abandon.
    function test_AnExpiredBeaconIsAStrikeAndNeverRecords() public {
        _reg();
        bytes32 s = keccak256("expire");
        bytes32 f = reg.factKey(FEED_METRIC, s, WINDOW);
        uint64 h = _ann(s, WINDOW, 0, POOL);
        vm.roll(h + 300);
        vm.expectRevert(OnyxRegistry.BeaconUnavailable.selector);
        reg.fixBeacon(f, 0);
        vm.expectRevert(OnyxRegistry.NoDraw.selector);
        reg.recordObservation(FEED_METRIC, s, WINDOW, SPEC, keccak256("r"), POOL, CFG);
        (uint32 strikes,) = reg.drawBudget(FEED_METRIC, s);
        assertEq(strikes, 1);
    }
}
