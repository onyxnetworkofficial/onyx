// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {StorageFixture} from "../OnyxStorageFixture.sol";

/// Handler for the storage network's money invariants (ADR-0060, R-STOR-11): deals, extensions,
/// time, seeds, honest and forged answers, missed answers settled (by a rewarded reporter or by
/// the node's own operator, amendment 1 item 3), registrations that pay the 2% fee and are then
/// activated or cancelled (item 2), declines (one shard or a whole deal), exits, bond and earnings
/// withdrawals (operators' and reporters'), deal closes and treasury sweeps, in any order. Ghost
/// totals count every token that entered and left the contract from outside it.
contract StorageHandler is StorageFixture {
    uint256 public tokensIn;
    uint256 public tokensOut;
    /// Paths reached (the coverage test below drives each one).
    uint256 public passes;
    uint256 public fails;
    uint256 public settles;
    uint256 public closes;
    uint256 public declines;
    uint256 public registrations;
    uint256 public activations;
    uint256 public cancels;
    uint256 public rewardedSettles;
    uint256 public selfSettles;
    /// Ghost: rewards credited to a slashed node's own operator (must stay 0; an assertion inside
    /// a handler call is swallowed in invariant runs, so it is counted here and checked below).
    uint256 public ownRewarded;
    /// Who may report a miss: two outsiders and the handler itself (the node's operator is the
    /// other choice, which earns no reward).
    address[] internal reporters;
    /// Registrations made here, not yet activated or cancelled.
    uint64[] internal _pending;
    address[] internal _pendingOps;
    /// The node list from each epoch on, as the contract checkpoints it.
    uint64[] internal _from;
    bytes32[][] internal _lists;

    constructor() {
        _deploy();
        for (uint256 i = 0; i < 6; i++) {
            _addNode(i, 50);
        }
        tokensIn = 6 * uint256(MIN_BOND);
        reporters.push(address(0xA11CE));
        reporters.push(address(0xB0BB));
        reporters.push(address(this));
        _snap();
    }

    function _snap() internal {
        uint64 f = st.currentEpoch() + 1;
        bytes32[] memory a = st.activeNodes();
        if (_from.length > 0 && _from[_from.length - 1] == f) {
            _lists[_lists.length - 1] = a;
        } else {
            _from.push(f);
            _lists.push(a);
        }
    }

    function _listAt(uint64 e) internal view returns (bytes32[] memory l, bool ok) {
        for (uint256 i = _from.length; i > 0; i--) {
            if (_from[i - 1] <= e) {
                l = _lists[i - 1];
                return (l, keccak256(abi.encodePacked(l)) == st.setHashAt(e));
            }
        }
        return (l, false);
    }

    function _bal() internal view returns (uint256) {
        return tok.balanceOf(address(st));
    }

    function createDeal(uint8 r, uint8 epochs) external {
        r = uint8(bound(r, 1, 3));
        epochs = uint8(bound(epochs, 1, 5));
        uint256 b = _bal();
        vm.prank(publisher);
        try st.createDeal(header, payload, r, epochs, "") {
            tokensIn += _bal() - b;
        } catch {}
    }

    function extend(uint256 d, uint8 epochs) external {
        uint256 n = st.dealCount();
        if (n == 0) return;
        uint256 b = _bal();
        vm.prank(publisher);
        try st.extendDeal(uint64(d % n), uint32(bound(epochs, 1, 3))) {
            tokensIn += _bal() - b;
        } catch {}
    }

    /// Steer a random run onto the reward path (review STORAGE-S16-R4-6): a running deal, a seed
    /// for its epoch, nobody answering, then a report of that miss in the next epoch.
    function missAndSettle(uint256 who, uint8 by) external {
        if (st.dealCount() == 0) this.createDeal(3, 4);
        uint256 n = st.dealCount();
        if (n == 0) return;
        uint64 deal = uint64(n - 1);
        (,,,,,,, uint64 start, uint64 end,,,) = st.dealInfo(deal);
        uint64 e = st.currentEpoch();
        if (e < start) e = start;
        if (e >= end) return;
        if (st.currentEpoch() < e) vm.roll(st.epochStart(e) + 1);
        if (st.seedOf(e) == bytes32(0)) {
            if (vm.getBlockNumber() <= st.epochStart(e)) vm.roll(st.epochStart(e) + 1);
            try st.recordSeed(e) {} catch {}
        }
        vm.roll(st.epochStart(e + 1) + 1);
        _snap();
        this.settle(deal, uint32(who % K), who, 1, by);
    }

    function advance(uint16 blocks) external {
        vm.roll(vm.getBlockNumber() + bound(blocks, 1, 3 * EPOCH));
    }

    function seed() external {
        try st.recordSeed(st.currentEpoch()) {} catch {}
    }

    function answer(uint256 d, uint32 slot, uint256 who, bool forge) external {
        uint256 n = st.dealCount();
        if (n == 0) return;
        uint64 deal = uint64(d % n);
        uint64 e = st.currentEpoch();
        if (e == 0 || st.seedOf(e) == bytes32(0)) return;
        (bytes32[] memory l, bool ok) = _listAt(e - 1);
        if (!ok) return;
        slot = uint32(bound(slot, 0, K - 1));
        try st.challengedHolders(deal, e, slot, l) returns (uint32, bytes32[] memory hs) {
            if (hs.length == 0) return;
            bytes32 id = hs[who % hs.length];
            (bytes memory entry, bytes32[] memory es, bytes memory row, bytes32[] memory rs) =
                _proof(st.seedOf(e), deal, slot, id);
            if (forge) row[0] = bytes1(uint8(row[0]) ^ 0x55);
            vm.prank(_opOf(id));
            try st.answer(deal, e, slot, id, l, entry, es, row, rs) returns (bool ok) {
                if (ok) passes++;
                else fails++;
            } catch {}
            _snap();
        } catch {}
    }

    function settle(uint256 d, uint32 slot, uint256 who, uint8 back, uint8 by) external {
        uint256 n = st.dealCount();
        if (n == 0) return;
        uint64 cur = st.currentEpoch();
        if (cur < 3) return;
        uint64 e = cur - uint64(bound(back, 1, 2));
        (bytes32[] memory l, bool ok) = _listAt(e - 1);
        if (!ok) return;
        uint64 deal = uint64(d % n);
        slot = uint32(bound(slot, 0, K - 1));
        try st.challengedHolders(deal, e, slot, l) returns (uint32, bytes32[] memory hs) {
            if (hs.length == 0) return;
            bytes32 id = hs[who % hs.length];
            uint256 k = by % (reporters.length + 1);
            address reporter = k < reporters.length ? reporters[k] : _opOf(id);
            uint256 r0 = st.rewarded();
            vm.prank(reporter);
            try st.settleMissed(deal, e, slot, id, l) {
                settles++;
                if (st.rewarded() > r0) rewardedSettles++;
                if (reporter == _opOf(id)) {
                    selfSettles++;
                    ownRewarded += st.rewarded() - r0;
                }
            } catch {}
            _snap();
        } catch {}
    }

    /// A new operator registers (the bond comes in whole; 2% of it is the treasury's at once).
    function register(uint16 capGiB) external {
        address op = address(uint160(0xD000 + registrations));
        tok.mint(op, 1e22);
        vm.startPrank(op);
        tok.approve(address(st), type(uint256).max);
        uint256 b = _bal();
        try st.register(uint64(bound(capGiB, 1, 2000)), "http://127.0.0.1:1/") returns (
            uint64 idx
        ) {
            tokensIn += _bal() - b;
            registrations++;
            _pending.push(idx);
            _pendingOps.push(op);
        } catch {}
        vm.stopPrank();
    }

    /// A pending registration is activated (in its window, with room for its share) or, once its
    /// window passed, cancelled for its bond less the fee.
    function activateOrCancel(uint256 i) external {
        uint256 n = _pending.length;
        if (n == 0) return;
        i %= n;
        uint64 idx = _pending[i];
        address op = _pendingOps[i];
        uint256 b = _bal();
        vm.startPrank(op);
        try st.activate(idx) returns (bytes32 id) {
            activations++;
            ops.push(op);
            ids.push(id);
            _drop(i);
            _snap();
        } catch {
            try st.cancelRegistration(idx) {
                tokensOut += b - _bal();
                cancels++;
                _drop(i);
            } catch {}
        }
        vm.stopPrank();
    }

    function _drop(uint256 i) internal {
        _pending[i] = _pending[_pending.length - 1];
        _pending.pop();
        _pendingOps[i] = _pendingOps[_pendingOps.length - 1];
        _pendingOps.pop();
    }

    /// A reporter withdraws its rewards.
    function withdrawReward(uint256 who) external {
        address r = reporters[who % reporters.length];
        uint256 b = _bal();
        vm.prank(r);
        try st.withdrawEarnings() {
            tokensOut += b - _bal();
        } catch {}
    }

    /// THREATS STORAGE-16: a decline moves no token, whatever the order.
    function decline(uint256 d, uint32 index, uint256 who, bool whole) external {
        uint256 n = st.dealCount();
        if (n == 0) return;
        uint256 i = who % ids.length;
        uint32 idx = whole ? st.WHOLE_DEAL() : uint32(bound(index, 0, 1));
        uint256 b = _bal();
        vm.prank(ops[i]);
        try st.decline(uint64(d % n), idx, ids[i]) {
            declines++;
        } catch {}
        assertEq(_bal(), b);
    }

    function exit(uint256 who) external {
        uint256 i = who % ids.length;
        vm.prank(ops[i]);
        try st.requestExit(ids[i]) {} catch {}
        _snap();
    }

    function withdrawBond(uint256 who) external {
        uint256 i = who % ids.length;
        uint256 b = _bal();
        vm.prank(ops[i]);
        try st.withdrawBond(ids[i]) {
            tokensOut += b - _bal();
        } catch {}
    }

    function withdrawEarnings(uint256 who) external {
        uint256 i = who % ops.length;
        uint256 b = _bal();
        vm.prank(ops[i]);
        try st.withdrawEarnings() {
            tokensOut += b - _bal();
        } catch {}
    }

    function close(uint256 d) external {
        uint256 n = st.dealCount();
        if (n == 0) return;
        uint256 b = _bal();
        try st.closeDeal(uint64(d % n)) {
            tokensOut += b - _bal();
            closes++;
        } catch {}
    }

    function sweep() external {
        uint256 b = _bal();
        try st.sweepTreasury() {
            tokensOut += b - _bal();
        } catch {}
    }
}

contract StorageInvariants is StdInvariant, Test {
    StorageHandler internal h;

    function setUp() public {
        h = new StorageHandler();
        targetContract(address(h));
        bytes4[] memory sel = new bytes4[](16);
        sel[0] = StorageHandler.createDeal.selector;
        sel[1] = StorageHandler.extend.selector;
        sel[2] = StorageHandler.advance.selector;
        sel[3] = StorageHandler.seed.selector;
        sel[4] = StorageHandler.answer.selector;
        sel[5] = StorageHandler.settle.selector;
        sel[6] = StorageHandler.exit.selector;
        sel[7] = StorageHandler.withdrawBond.selector;
        sel[8] = StorageHandler.withdrawEarnings.selector;
        sel[9] = StorageHandler.close.selector;
        sel[10] = StorageHandler.sweep.selector;
        sel[11] = StorageHandler.decline.selector;
        sel[12] = StorageHandler.register.selector;
        sel[13] = StorageHandler.activateOrCancel.selector;
        sel[14] = StorageHandler.withdrawReward.selector;
        sel[15] = StorageHandler.missAndSettle.selector;
        targetSelector(FuzzSelector({addr: address(h), selectors: sel}));
    }

    /// R-STOR-11: every token held is a deposit not paid or refunded, a bond, an earning or a
    /// reward not withdrawn, or a slash or fee not rewarded or swept; nothing flows out above
    /// what flowed in.
    function invariant_conserved() public view {
        assertTrue(h.st().conserved());
    }

    /// Outside view: what came in equals what went out plus what is held.
    function invariant_tokensInEqualOutPlusHeld() public view {
        assertEq(h.tokensIn(), h.tokensOut() + h.tok().balanceOf(address(h.st())));
    }

    /// Review STORAGE-S16-R4-6: a node's own operator is never rewarded for reporting its miss.
    function invariant_ownOperatorNeverRewarded() public view {
        assertEq(h.ownRewarded(), 0);
    }

    /// No deal ever paid out more than its deposit.
    function invariant_dealsNeverOverpay() public view {
        uint256 n = h.st().dealCount();
        for (uint256 i = 0; i < n; i++) {
            (,,,,,,,,, uint256 dep, uint256 paidOut,) = h.st().dealInfo(uint64(i));
            assertLe(paidOut, dep);
        }
    }
}

/// The handler reaches every money path (so the invariants above are not vacuous): a fixed
/// sequence of registrations (activated and cancelled), deals, seeds, honest and forged answers,
/// misses settled by rewarded reporters and by nodes' own operators, closes and withdrawals.
contract StorageHandlerCoverage is Test {
    function test_handlerReachesEveryPath() public {
        StorageHandler h = new StorageHandler();
        h.register(100);
        h.register(100);
        h.advance(1);
        h.activateOrCancel(0);
        h.createDeal(3, 4);
        h.createDeal(2, 3);
        h.decline(0, 1, 4, false);
        h.decline(1, 0, 5, true);
        for (uint256 round = 0; round < 6; round++) {
            h.advance(uint16(10));
            h.seed();
            for (uint256 i = 0; i < 3; i++) {
                h.answer(i, uint32(i), i, i % 3 == 0);
            }
            h.advance(uint16(10));
            h.seed();
            // Every (deal, slot, holder) of the last epoch, reported in turn by each reporter
            // and by the node's own operator.
            for (uint256 i = 0; i < 12; i++) {
                h.settle(i % 2, uint32(i / 2 % 2), i / 4, uint8(1), uint8(i + round));
            }
        }
        h.advance(uint16(30));
        h.close(0);
        h.close(1);
        for (uint256 i = 0; i < 6; i++) {
            h.withdrawEarnings(i);
        }
        for (uint256 i = 0; i < 3; i++) {
            h.withdrawReward(i);
        }
        // The second registration's 256-block window has passed: it is cancelled.
        for (uint256 i = 0; i < 4; i++) {
            h.advance(uint16(30));
        }
        h.activateOrCancel(0);
        h.sweep();
        assertGt(h.passes(), 0, "no passed answer");
        assertGt(h.fails(), 0, "no failed answer");
        assertGt(h.settles(), 0, "no settled miss");
        assertGt(h.rewardedSettles(), 0, "no rewarded report");
        assertGt(h.selfSettles(), 0, "no self-report");
        assertGt(h.closes(), 0, "no close");
        assertEq(h.declines(), 2, "declines");
        assertEq(h.registrations(), 2, "registrations");
        assertEq(h.activations(), 1, "activations");
        assertEq(h.cancels(), 1, "cancels");
        assertGt(h.st().rewarded(), 0);
        assertTrue(h.st().conserved());
        assertEq(h.tokensIn(), h.tokensOut() + h.tok().balanceOf(address(h.st())));
    }
}
