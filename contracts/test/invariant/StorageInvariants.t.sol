// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {StorageFixture} from "../OnyxStorageFixture.sol";

/// Handler for the storage network's money invariants (ADR-0060, R-STOR-11): deals, extensions,
/// time, seeds, honest and forged answers, missed answers settled, declines (one shard or a whole
/// deal), exits, bond and earnings withdrawals, deal closes and treasury sweeps, in any order. Ghost totals count every token
/// that entered and left the contract from outside it.
contract StorageHandler is StorageFixture {
    uint256 public tokensIn;
    uint256 public tokensOut;
    /// Paths reached (the coverage test below drives each one).
    uint256 public passes;
    uint256 public fails;
    uint256 public settles;
    uint256 public closes;
    uint256 public declines;
    /// The node list from each epoch on, as the contract checkpoints it.
    uint64[] internal _from;
    bytes32[][] internal _lists;

    constructor() {
        _deploy();
        for (uint256 i = 0; i < 6; i++) {
            _addNode(i, 50);
        }
        tokensIn = 6 * uint256(MIN_BOND);
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
                _proof(st.seedOf(e), deal, slot);
            if (forge) row[0] = bytes1(uint8(row[0]) ^ 0x55);
            vm.prank(_opOf(id));
            try st.answer(deal, e, slot, id, l, entry, es, row, rs) returns (bool ok) {
                if (ok) passes++;
                else fails++;
            } catch {}
            _snap();
        } catch {}
    }

    function settle(uint256 d, uint32 slot, uint256 who, uint8 back) external {
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
            try st.settleMissed(deal, e, slot, hs[who % hs.length], l) {
                settles++;
            } catch {}
            _snap();
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
        bytes4[] memory sel = new bytes4[](12);
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
        targetSelector(FuzzSelector({addr: address(h), selectors: sel}));
    }

    /// R-STOR-11: every token held is a deposit not paid or refunded, a bond, an earning or a
    /// slash not swept; nothing flows out above what flowed in.
    function invariant_conserved() public view {
        assertTrue(h.st().conserved());
    }

    /// Outside view: what came in equals what went out plus what is held.
    function invariant_tokensInEqualOutPlusHeld() public view {
        assertEq(h.tokensIn(), h.tokensOut() + h.tok().balanceOf(address(h.st())));
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
/// sequence of deals, seeds, honest and forged answers, settled misses and closes.
contract StorageHandlerCoverage is Test {
    function test_handlerReachesEveryPath() public {
        StorageHandler h = new StorageHandler();
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
            for (uint256 i = 0; i < 12; i++) {
                h.settle(i, uint32(i), i, uint8(1));
            }
        }
        h.advance(uint16(30));
        h.close(0);
        h.close(1);
        for (uint256 i = 0; i < 6; i++) {
            h.withdrawEarnings(i);
        }
        h.sweep();
        assertGt(h.passes(), 0, "no passed answer");
        assertGt(h.fails(), 0, "no failed answer");
        assertGt(h.settles(), 0, "no settled miss");
        assertGt(h.closes(), 0, "no close");
        assertEq(h.declines(), 2, "declines");
        assertTrue(h.st().conserved());
        assertEq(h.tokensIn(), h.tokensOut() + h.tok().balanceOf(address(h.st())));
    }
}
