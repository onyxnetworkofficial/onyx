// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {StorageFixture} from "./OnyxStorageFixture.sol";
import {OnyxStorage, IStorageToken} from "../src/OnyxStorage.sol";
import {OnyxStorageLib} from "../src/OnyxStorageLib.sol";

contract OnyxStorageTest is StorageFixture {
    function setUp() public {
        _deploy();
        for (uint256 i = 0; i < 5; i++) {
            _addNode(i, 100);
        }
    }

    function _deal() internal returns (uint64 d) {
        vm.prank(publisher);
        d = st.createDeal(header, payload, 3, 4, "http://127.0.0.1:2/");
    }

    // ---------------------------------------------------------------- registration

    function test_registerTakesBondAndIdComesFromTheRegistrationBlock() public {
        address op = address(0xC0DE);
        tok.mint(op, 1e22);
        vm.startPrank(op);
        tok.approve(address(st), type(uint256).max);
        uint256 before = tok.balanceOf(op);
        uint64 idx = st.register(5000, "http://127.0.0.1:3/");
        assertEq(before - tok.balanceOf(op), 5000 * uint256(BOND_PER_GIB));
        uint256 regBlock = vm.getBlockNumber();
        // Same block: the id would be known from a hash nobody can know yet; refused.
        vm.expectRevert(OnyxStorage.ActivationWindow.selector);
        st.activate(idx);
        vm.roll(vm.getBlockNumber() + 1);
        bytes32 id = st.activate(idx);
        assertEq(id, OnyxStorageLib.nodeId(op, idx, blockhash(regBlock)));
        vm.expectRevert(OnyxStorage.NotRegistration.selector);
        st.activate(idx);
        vm.stopPrank();
        assertEq(st.activeNodes().length, 6);
    }

    function test_activationWindowAndCancel() public {
        address op = address(0xC0DF);
        tok.mint(op, 1e22);
        vm.startPrank(op);
        tok.approve(address(st), type(uint256).max);
        uint64 idx = st.register(1, "http://x/");
        vm.expectRevert(OnyxStorage.TooEarly.selector);
        st.cancelRegistration(idx);
        vm.roll(vm.getBlockNumber() + 257);
        vm.expectRevert(OnyxStorage.ActivationWindow.selector);
        st.activate(idx);
        uint256 b = tok.balanceOf(op);
        st.cancelRegistration(idx);
        assertEq(tok.balanceOf(op) - b, MIN_BOND);
        vm.stopPrank();
        vm.prank(address(0xDEAD));
        vm.expectRevert(OnyxStorage.NotOperator.selector);
        st.activate(0);
        assertTrue(st.conserved());
    }

    function test_registerRefusesBadCapacity() public {
        vm.expectRevert(OnyxStorage.BadParams.selector);
        st.register(0, "http://x/");
        uint64 tooMuch = st.MAX_CAPACITY_GIB() + 1;
        vm.expectRevert(OnyxStorage.BadParams.selector);
        st.register(tooMuch, "http://x/");
    }

    function test_nodeSetChangesFromTheNextEpoch() public {
        uint64 e = st.currentEpoch();
        bytes32 now_ = st.setHashAt(e);
        bytes32 next = st.setHashAt(e + 1);
        assertEq(next, keccak256(abi.encodePacked(st.activeNodes())));
        // Every node joined in this epoch: the list in force now is still empty.
        assertEq(now_, keccak256(""));
    }

    // ---------------------------------------------------------------- deals

    function test_createDealBindsTheAnchorAndTakesTheDeposit() public {
        uint256 b = tok.balanceOf(publisher);
        uint64 d = _deal();
        // 4 GiB × 3 replicas × 4 epochs × price.
        assertEq(b - tok.balanceOf(publisher), uint256(PRICE) * 4 * 3 * 4);
        (
            address payer,
            bytes32 mr,
            bytes32 sr,,
            uint32 sc,
            uint8 r,
            uint64 bc,
            uint64 se,
            uint64 ee,
            uint256 dep,,
        ) = st.dealInfo(d);
        assertEq(payer, publisher);
        assertEq(mr, _manifestRoot());
        assertEq(sr, _root2(entries[0], entries[1]));
        assertEq(sc, 2);
        assertEq(r, 3);
        assertEq(bc, BYTES);
        assertEq(se, st.currentEpoch() + 2);
        assertEq(ee, se + 4);
        assertEq(dep, uint256(PRICE) * 48);
        assertEq(st.committedGiB(), 12);
        assertTrue(st.conserved());
    }

    function test_createDealRefusesUnanchoredOrForeignManifests() public {
        // A header whose root the book never saw.
        bytes memory h2 = bytes.concat(header);
        h2[h2.length - 1] = bytes1(uint8(h2[h2.length - 1]) ^ 1);
        bytes memory p2 = _payloadFor(keccak256(abi.encodePacked("onyx-bulk-manifest-v1", h2)), 43);
        vm.startPrank(publisher);
        vm.expectRevert(OnyxStorage.NotAnchored.selector);
        st.createDeal(h2, p2, 3, 4, "");
        // The anchored payload under another header: the value is not this root.
        vm.expectRevert(OnyxStorageLib.BadPayload.selector);
        st.createDeal(h2, payload, 3, 4, "");
        // A truncated payload.
        vm.expectRevert(OnyxStorageLib.BadPayload.selector);
        st.createDeal(header, bytes.concat(payload, hex"00"), 3, 4, "");
        // A broken header.
        bytes memory h3 = bytes.concat(header);
        h3[0] = 0x02;
        vm.expectRevert(OnyxStorageLib.BadHeader.selector);
        st.createDeal(h3, payload, 3, 4, "");
        vm.stopPrank();
    }

    function test_createDealRefusesACompromisedKeysManifest() public {
        bytes32 key = book.keyId(PUBLISHER, vm.addr(signerKey));
        vm.prank(reviewer);
        book.markCompromised(key, T0 - 1, "test");
        vm.prank(publisher);
        vm.expectRevert(OnyxStorage.NotAnchored.selector);
        st.createDeal(header, payload, 3, 4, "");
    }

    function test_createDealRefusesBadParamsAndOverCapacity() public {
        vm.startPrank(publisher);
        vm.expectRevert(OnyxStorage.BadParams.selector);
        st.createDeal(header, payload, 0, 4, "");
        vm.expectRevert(OnyxStorage.BadParams.selector);
        st.createDeal(header, payload, 9, 4, "");
        vm.expectRevert(OnyxStorage.BadParams.selector);
        st.createDeal(header, payload, 3, 0, "");
        // 6 replicas with 5 nodes.
        vm.expectRevert(OnyxStorage.NotEnoughNodes.selector);
        st.createDeal(header, payload, 6, 4, "");
        // 500 GiB of capacity, 12 GiB per deal: the 42nd deal does not fit.
        for (uint256 i = 0; i < 41; i++) {
            st.createDeal(header, payload, 3, 1, "");
        }
        vm.expectRevert(OnyxStorage.OverCapacity.selector);
        st.createDeal(header, payload, 3, 1, "");
        vm.stopPrank();
    }

    /// Review STORAGE-R1-6: placement is even, so a joining node must have declared at least its
    /// even share of what is already committed; a smaller one cannot join.
    function test_activateRefusesACapacityBelowTheShare() public {
        vm.startPrank(publisher);
        for (uint256 i = 0; i < 3; i++) {
            st.createDeal(header, payload, 3, 1, "");
        }
        vm.stopPrank();
        // 36 GiB of copies over 6 nodes once it joins: at least 6 GiB.
        address op = address(0xA0FF);
        tok.mint(op, 1e22);
        vm.startPrank(op);
        tok.approve(address(st), type(uint256).max);
        uint64 small = st.register(5, "http://127.0.0.1:1/");
        uint64 enough = st.register(6, "http://127.0.0.1:1/");
        vm.roll(vm.getBlockNumber() + 1);
        vm.expectRevert(OnyxStorage.BelowShare.selector);
        st.activate(small);
        st.activate(enough);
        vm.stopPrank();
    }

    /// Review STORAGE-R2-2: one small node (that declines everything) cannot freeze new deals.
    function test_aOneGiBNodeCannotFreezeDeals() public {
        bytes32 tiny = _addNode(5, 1);
        vm.prank(publisher);
        uint64 d = st.createDeal(header, payload, 3, 1, "");
        uint32 whole = st.WHOLE_DEAL();
        vm.prank(_opOf(tiny));
        st.decline(d, whole, tiny);
        vm.prank(publisher);
        st.createDeal(header, payload, 3, 1, "");
        assertEq(st.committedGiB(), 24);
    }

    function test_createDealWithoutApprovalPaysNothingAndStoresNothing() public {
        address stranger = address(0x5A5A);
        tok.mint(stranger, 1e24);
        vm.prank(stranger);
        vm.expectRevert(); // the token refuses the transfer (THREATS STORAGE-5)
        st.createDeal(header, payload, 3, 4, "");
        assertEq(st.dealCount(), 0);
    }

    // ---------------------------------------------------------------- seeds

    function test_seedWindow() public {
        uint64 e = st.currentEpoch() + 1;
        _toEpoch(e, 0);
        vm.expectRevert(OnyxStorage.SeedWindow.selector);
        st.recordSeed(e);
        vm.roll(vm.getBlockNumber() + 1);
        bytes32 s = st.recordSeed(e);
        assertEq(s, OnyxStorageLib.seed(blockhash(st.epochStart(e)), e));
        vm.expectRevert(OnyxStorage.SeedAlready.selector);
        st.recordSeed(e);
        // A later epoch whose window passed has no seed: its challenges never run.
        _toEpoch(e + 1, 257);
        vm.expectRevert(OnyxStorage.SeedWindow.selector);
        st.recordSeed(e + 1);
    }

    // ---------------------------------------------------------------- answers

    function _start() internal returns (uint64 d, uint64 e, bytes32[] memory list) {
        d = _deal();
        (,,,,,,, e,,,,) = st.dealInfo(d);
        list = _list();
        _seed(e);
    }

    function test_honestHoldersArePaidOncePerSlot() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _start();
        uint256 pay = st.slotPay(d);
        // 4 GiB × 3 × price over 2 slots × 3 replicas.
        assertEq(pay, uint256(PRICE) * 12 / 6);
        uint256 answered;
        for (uint32 slot = 0; slot < K; slot++) {
            (, bytes32[] memory hs) = st.challengedHolders(d, e, slot, list);
            assertEq(hs.length, 3);
            for (uint256 i = 0; i < hs.length; i++) {
                uint256 before = st.earned(_opOf(hs[i]));
                assertTrue(_answer(hs[i], d, e, slot, list));
                assertEq(st.earned(_opOf(hs[i])) - before, pay);
                assertEq(st.slotStatus(d, e, slot, hs[i]), st.PASSED());
                answered++;
            }
            vm.expectRevert(OnyxStorage.AlreadyAnswered.selector);
            this.answerAs(hs[0], d, e, slot, list);
        }
        (,,,,,,,,,, uint256 paidOut,) = st.dealInfo(d);
        assertEq(paidOut, pay * answered);
        assertTrue(st.conserved());
    }

    /// Helper so `expectRevert` sees one external call.
    function answerAs(bytes32 id, uint64 d, uint64 e, uint32 slot, bytes32[] memory list)
        external
        returns (bool)
    {
        return _answer(id, d, e, slot, list);
    }

    function test_answerRefusals() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _start();
        (, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        (bytes memory entry, bytes32[] memory es, bytes memory row, bytes32[] memory rs) =
            _proof(st.seedOf(e), d, 0);
        // Only the node's operator answers.
        vm.expectRevert(OnyxStorage.NotOperator.selector);
        st.answer(d, e, 0, hs[0], list, entry, es, row, rs);
        vm.startPrank(_opOf(hs[0]));
        // Another epoch.
        vm.expectRevert(OnyxStorage.WrongEpoch.selector);
        st.answer(d, e + 1, 0, hs[0], list, entry, es, row, rs);
        // A forged node list.
        bytes32[] memory forged = new bytes32[](3);
        forged[0] = hs[0];
        forged[1] = hs[1];
        forged[2] = hs[2];
        vm.expectRevert(OnyxStorage.WrongNodeList.selector);
        st.answer(d, e, 0, hs[0], forged, entry, es, row, rs);
        // A slot past the deal's slots.
        vm.expectRevert(OnyxStorage.NotChallenged.selector);
        st.answer(d, e, K, hs[0], list, entry, es, row, rs);
        vm.stopPrank();
        // A node that is not challenged for this shard.
        for (uint256 i = 0; i < list.length; i++) {
            bool isHolder = list[i] == hs[0] || list[i] == hs[1] || list[i] == hs[2];
            if (!isHolder) {
                vm.prank(_opOf(list[i]));
                vm.expectRevert(OnyxStorage.NotChallenged.selector);
                st.answer(d, e, 0, list[i], list, entry, es, row, rs);
                break;
            }
        }
        // After the epoch: too late.
        _toEpoch(e + 1, 0);
        vm.prank(_opOf(hs[0]));
        vm.expectRevert(OnyxStorage.WrongEpoch.selector);
        st.answer(d, e, 0, hs[0], list, entry, es, row, rs);
    }

    function test_aForgedRowIsRefusedAndSlashed() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _start();
        (uint32 idx, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        (bytes memory entry, bytes32[] memory es, bytes memory row, bytes32[] memory rs) =
            _proof(st.seedOf(e), d, 0);
        row[0] = bytes1(uint8(row[0]) ^ 1);
        uint256 bond = _bond(hs[1]);
        vm.prank(_opOf(hs[1]));
        assertFalse(st.answer(d, e, 0, hs[1], list, entry, es, row, rs));
        assertEq(st.slotStatus(d, e, 0, hs[1]), st.FAILED());
        assertEq(bond - _bond(hs[1]), bond * SLASH_PPM / 1e6);
        assertEq(st.earned(_opOf(hs[1])), 0);
        assertEq(st.slashed(), bond * SLASH_PPM / 1e6);
        // Excluded from that shard from the next epoch: not asked again while its replacement
        // fetches, and the replacement holds it from the epoch after.
        assertEq(st.exclusionFrom(d, idx, hs[1]), e + 1);
        assertTrue(st.conserved());
    }

    function test_wrongShardOrWrongRowFails() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _start();
        (uint32 idx, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        uint256 r = uint256(
            keccak256(abi.encodePacked("onyx-storage-row-v1", st.seedOf(e), d, uint32(0)))
        ) % 2;
        // The other row of the right shard, with its own (valid) path: wrong index.
        bytes memory otherRow = rows[idx][1 - r];
        vm.prank(_opOf(hs[0]));
        assertFalse(
            st.answer(
                d,
                e,
                0,
                hs[0],
                list,
                entries[idx],
                _one(_leaf(entries[1 - idx])),
                otherRow,
                _one(_leaf(rows[idx][r]))
            )
        );
        // The other shard's entry.
        vm.prank(_opOf(hs[1]));
        assertFalse(
            st.answer(
                d,
                e,
                0,
                hs[1],
                list,
                entries[1 - idx],
                _one(_leaf(entries[idx])),
                rows[1 - idx][0],
                _one(_leaf(rows[1 - idx][1]))
            )
        );
        // An entry of the wrong length.
        vm.prank(_opOf(hs[2]));
        assertFalse(
            st.answer(
                d, e, 0, hs[2], list, hex"00", new bytes32[](0), rows[idx][r], new bytes32[](0)
            )
        );
    }

    function test_aMissIsSettledAfterTheEpochAndSlashed() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _start();
        (uint32 idx, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        assertTrue(_answer(hs[0], d, e, 0, list));
        // Not before the epoch ends.
        vm.expectRevert(OnyxStorage.SettleWindow.selector);
        st.settleMissed(d, e, 0, hs[1], list);
        _toEpoch(e + 1, 0);
        uint256 bond = _bond(hs[1]);
        vm.prank(address(0x1234)); // anyone
        st.settleMissed(d, e, 0, hs[1], list);
        assertEq(st.slotStatus(d, e, 0, hs[1]), st.MISSED());
        assertEq(bond - _bond(hs[1]), bond / 10);
        assertEq(st.exclusionFrom(d, idx, hs[1]), e + 2);
        // Not twice, and never for an answered slot.
        vm.expectRevert(OnyxStorage.AlreadyAnswered.selector);
        st.settleMissed(d, e, 0, hs[1], list);
        vm.expectRevert(OnyxStorage.AlreadyAnswered.selector);
        st.settleMissed(d, e, 0, hs[0], list);
        // After the window: nothing.
        _toEpoch(e + 3, 0);
        vm.expectRevert(OnyxStorage.SettleWindow.selector);
        st.settleMissed(d, e, 0, hs[2], list);
        assertTrue(st.conserved());
    }

    function test_exclusionMovesTheCopyToTheNextRankedNode() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _start();
        (uint32 idx, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        bytes32 bad = hs[0];
        _toEpoch(e + 1, 0);
        st.settleMissed(d, e, 0, bad, list); // excluded from e + 2
        assertEq(st.exclusionFrom(d, idx, bad), e + 2);
        // Epoch e + 2: the replacement is fetching, so a slot on shard idx asks the two other
        // holders only; from e + 3 it asks three, never `bad` (also covered by the vectors).
        uint256 seen;
        for (uint64 ek = e + 2; ek <= e + 5; ek++) {
            _seed(ek);
            for (uint32 slot = 0; slot < K; slot++) {
                (uint32 i2, bytes32[] memory h2) = st.challengedHolders(d, ek, slot, list);
                if (i2 != idx) continue;
                seen++;
                assertEq(h2.length, ek == e + 2 ? 2 : 3);
                for (uint256 k = 0; k < h2.length; k++) {
                    assertTrue(h2[k] != bad);
                }
            }
        }
        assertGt(seen, 0, "no slot picked the shard in four epochs");
    }

    function _asked(bytes32 n, uint64 e, bytes32[] memory list) internal view returns (uint256 c) {
        for (uint64 d = 0; d < 3; d++) {
            for (uint32 slot = 0; slot < K; slot++) {
                (, bytes32[] memory hs) = st.challengedHolders(d, e, slot, list);
                for (uint256 i = 0; i < hs.length; i++) {
                    if (hs[i] == n) c++;
                }
            }
        }
    }

    function test_maxMissesJailsAndRemovesTheNode() public {
        // Three deals in the same epoch: six slots, 18 challenges over 5 nodes; the node asked
        // most often (at least 4 times) misses them all.
        uint64 d0 = _deal();
        _deal();
        _deal();
        (,,,,,,, uint64 e,,,,) = st.dealInfo(d0);
        bytes32[] memory list = _list();
        _seed(e);
        bytes32 bad;
        uint256 most;
        for (uint256 i = 0; i < list.length; i++) {
            uint256 c = _asked(list[i], e, list);
            if (c > most) (most, bad) = (c, list[i]);
        }
        assertGe(most, MAX_MISSES);
        _toEpoch(e + 1, 0);
        uint256 misses;
        for (uint64 d = 0; d < 3 && misses < MAX_MISSES; d++) {
            for (uint32 slot = 0; slot < K && misses < MAX_MISSES; slot++) {
                (, bytes32[] memory hs) = st.challengedHolders(d, e, slot, list);
                for (uint256 i = 0; i < hs.length; i++) {
                    if (hs[i] == bad) {
                        st.settleMissed(d, e, slot, bad, list);
                        misses++;
                    }
                }
            }
        }
        assertEq(misses, MAX_MISSES);
        (, uint8 status,,,, uint64 removedFrom, uint64 jailedFrom,) = st.nodeInfo(bad);
        assertEq(status, st.JAILED());
        assertEq(removedFrom, st.currentEpoch() + 1);
        assertEq(jailedFrom, removedFrom);
        assertEq(st.activeNodes().length, 4);
        assertEq(st.totalCapacityGiB(), 400);
    }

    // ---------------------------------------------------------------- exit and money out

    function test_exitThenBondAfterTheSettleWindow() public {
        bytes32 id = ids[0];
        address op = ops[0];
        vm.prank(address(0x99));
        vm.expectRevert(OnyxStorage.NotOperator.selector);
        st.requestExit(id);
        vm.prank(op);
        st.requestExit(id);
        (,,,,, uint64 removedFrom,,) = st.nodeInfo(id);
        assertEq(removedFrom, st.currentEpoch() + 1);
        vm.prank(op);
        vm.expectRevert(OnyxStorage.TooEarly.selector);
        st.withdrawBond(id);
        _toEpoch(removedFrom + st.SETTLE_EPOCHS(), 0);
        vm.prank(op);
        vm.expectRevert(OnyxStorage.TooEarly.selector);
        st.withdrawBond(id);
        _toEpoch(removedFrom + st.SETTLE_EPOCHS() + 1, 0);
        uint256 b = tok.balanceOf(op);
        vm.prank(op);
        st.withdrawBond(id);
        assertEq(tok.balanceOf(op) - b, MIN_BOND);
        vm.prank(op);
        vm.expectRevert(OnyxStorage.NotActive.selector);
        st.withdrawBond(id);
        assertTrue(st.conserved());
    }

    function test_closeRefundsExactlyWhatWasNotPaid() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _start();
        (, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        _answer(hs[0], d, e, 0, list);
        vm.expectRevert(OnyxStorage.DealOpen.selector);
        st.closeDeal(d);
        (,,,,,,,, uint64 end, uint256 dep,,) = st.dealInfo(d);
        _toEpoch(end, 0);
        uint256 b = tok.balanceOf(publisher);
        st.closeDeal(d);
        assertEq(tok.balanceOf(publisher) - b, dep - st.slotPay(d));
        assertEq(st.committedGiB(), 0);
        vm.expectRevert(OnyxStorage.Closed.selector);
        st.closeDeal(d);
        // Withdrawals and the sweep empty every account but the bonds.
        vm.prank(_opOf(hs[0]));
        st.withdrawEarnings();
        vm.expectRevert(OnyxStorage.BadAmount.selector);
        st.sweepTreasury();
        assertEq(tok.balanceOf(address(st)), 5 * uint256(MIN_BOND));
        assertTrue(st.conserved());
    }

    function test_slashesGoOnlyToTheTreasury() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _start();
        (, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        _toEpoch(e + 1, 0);
        st.settleMissed(d, e, 0, hs[0], list);
        uint256 amt = st.slashed();
        vm.prank(address(0x4444)); // anyone triggers it; only the treasury receives
        st.sweepTreasury();
        assertEq(tok.balanceOf(treasury), amt);
        assertEq(tok.balanceOf(address(0x4444)), 0);
        assertTrue(st.conserved());
    }

    function test_extendDeal() public {
        uint64 d = _deal();
        (,,,,,,,, uint64 end, uint256 dep,,) = st.dealInfo(d);
        vm.prank(publisher);
        st.extendDeal(d, 2);
        (,,,,,,,, uint64 end2, uint256 dep2,,) = st.dealInfo(d);
        assertEq(end2, end + 2);
        assertEq(dep2 - dep, uint256(PRICE) * 4 * 3 * 2);
        _toEpoch(end2, 0);
        vm.prank(publisher);
        vm.expectRevert(OnyxStorage.DealOver.selector);
        st.extendDeal(d, 1);
    }

    function test_constructorRefusesBadParams() public {
        vm.expectRevert(OnyxStorage.BadParams.selector);
        new OnyxStorage(
            IStorageToken(address(tok)),
            book,
            treasury,
            1,
            PRICE,
            BOND_PER_GIB,
            MIN_BOND,
            SLASH_PPM,
            K,
            MAX_MISSES
        );
        vm.expectRevert(OnyxStorage.BadParams.selector);
        new OnyxStorage(
            IStorageToken(address(tok)),
            book,
            treasury,
            EPOCH,
            PRICE,
            0,
            MIN_BOND,
            SLASH_PPM,
            K,
            MAX_MISSES
        );
        vm.expectRevert(OnyxStorage.BadParams.selector);
        new OnyxStorage(
            IStorageToken(address(tok)),
            book,
            treasury,
            EPOCH,
            PRICE,
            BOND_PER_GIB,
            MIN_BOND,
            1_000_001,
            K,
            MAX_MISSES
        );
        vm.chainId(1);
        vm.expectRevert(abi.encodeWithSelector(OnyxStorage.WrongChain.selector, 1));
        new OnyxStorage(
            IStorageToken(address(tok)),
            book,
            treasury,
            EPOCH,
            PRICE,
            BOND_PER_GIB,
            MIN_BOND,
            SLASH_PPM,
            K,
            MAX_MISSES
        );
    }

    // ---------------------------------------------------------------- decline

    /// THREATS STORAGE-16: a node that cannot get a shard (a withholding publisher) gives it up
    /// without a slash; the decline counts from the next epoch, so a challenge already drawn
    /// still stands.
    function test_declineMovesTheCopyWithoutASlash() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _start();
        (uint32 idx, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        bytes32 x = hs[0];
        uint256 bond = _bond(x);
        vm.prank(address(0x99));
        vm.expectRevert(OnyxStorage.NotOperator.selector);
        st.decline(d, idx, x);
        vm.prank(_opOf(x));
        vm.expectRevert(OnyxStorage.BadParams.selector);
        st.decline(d, 2, x);
        vm.prank(_opOf(x));
        st.decline(d, idx, x);
        assertEq(st.exclusionFrom(d, idx, x), e + 1);
        assertEq(_bond(x), bond, "no slash");
        vm.prank(_opOf(x));
        vm.expectRevert(OnyxStorage.AlreadyExcluded.selector);
        st.decline(d, idx, x);
        // Still asked in this epoch.
        (, bytes32[] memory again) = st.challengedHolders(d, e, 0, list);
        assertEq(again[0], x);
        // Never asked for that shard again.
        uint256 seen;
        for (uint64 ek = e + 1; ek <= e + 4; ek++) {
            _seed(ek);
            for (uint32 slot = 0; slot < K; slot++) {
                (uint32 i2, bytes32[] memory h2) = st.challengedHolders(d, ek, slot, list);
                if (i2 != idx) continue;
                seen++;
                for (uint256 k = 0; k < h2.length; k++) {
                    assertTrue(h2[k] != x);
                }
            }
        }
        assertGt(seen, 0);
        assertTrue(st.conserved());
    }

    /// Review STORAGE-R2-6: a decline in the epoch of a drawn challenge does not escape it; the
    /// miss is settled and slashed after the epoch, and the earlier exclusion is kept.
    function test_aDeclineDoesNotEscapeADrawnChallenge() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _start();
        (uint32 idx, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        bytes32 x = hs[0];
        uint32 whole = st.WHOLE_DEAL();
        vm.prank(_opOf(x));
        st.decline(d, whole, x);
        _toEpoch(e + 1, 0);
        uint256 bond = _bond(x);
        st.settleMissed(d, e, 0, x, list);
        assertEq(st.slotStatus(d, e, 0, x), st.MISSED());
        assertEq(bond - _bond(x), bond / 10);
        // The decline's exclusion (e + 1) stays; the slash's (e + 2) does not move it later.
        assertEq(st.exclusionFrom(d, idx, x), e + 1);
        assertTrue(st.conserved());
    }

    function test_declineAWholeDeal() public {
        (uint64 d, uint64 e,) = _start();
        bytes32 x = ids[2];
        uint32 whole = st.WHOLE_DEAL();
        vm.prank(_opOf(x));
        st.decline(d, whole, x);
        assertEq(st.exclusionFrom(d, 0, x), e + 1);
        assertEq(st.exclusionFrom(d, 1, x), e + 1);
        // Another deal is not touched.
        assertEq(st.exclusionFrom(d + 1, 0, x), 0);
    }

    /// Amounts are kept in 128 bits; anything larger is refused before a token moves, never
    /// truncated.
    function test_amountsOver128BitsAreRefused() public {
        OnyxStorage big = new OnyxStorage(
            IStorageToken(address(tok)),
            book,
            treasury,
            EPOCH,
            type(uint128).max,
            type(uint128).max,
            MIN_BOND,
            SLASH_PPM,
            K,
            MAX_MISSES
        );
        vm.expectRevert(OnyxStorage.BadAmount.selector);
        big.register(2, "http://x/");
    }

    // ---------------------------------------------------------------- scale

    /// The gas of one answer at the node cap (128 nodes in the list, 4 KiB of calldata for it),
    /// for docs/STORAGE-NETWORK.md; the 5-node answer is in the other tests' gas.
    function test_answerGasAtTheNodeCap() public {
        for (uint256 i = 5; i < st.MAX_NODES(); i++) {
            _addNode(i, 100);
        }
        vm.expectRevert(OnyxStorage.TooManyNodes.selector);
        this.addOneMore();
        (uint64 d, uint64 e, bytes32[] memory list) = _start();
        assertEq(list.length, 128);
        (, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        (bytes memory entry, bytes32[] memory es, bytes memory row, bytes32[] memory rs) =
            _proof(st.seedOf(e), d, 0);
        vm.prank(_opOf(hs[0]));
        uint256 g = gasleft();
        assertTrue(st.answer(d, e, 0, hs[0], list, entry, es, row, rs));
        emit log_named_uint("answer gas with 128 nodes (execution only)", g - gasleft());
    }

    /// THREATS STORAGE-S16-R1-2: the worst answer at the node cap. Every node but the three
    /// lowest-weighted declines the shard, so `_challenged` passes over 125 excluded nodes
    /// before it finds the three holders.
    function test_answerGasAtTheNodeCapWithDeclines() public {
        for (uint256 i = 5; i < st.MAX_NODES(); i++) {
            _addNode(i, 100);
        }
        uint64 d = _deal();
        (,,, bytes32 salt, uint32 shards,,, uint64 e,,,,) = st.dealInfo(d);
        bytes32[] memory list = _list();
        for (uint32 s = 0; s < shards; s++) {
            bytes32 key = OnyxStorageLib.shardKey(salt, s);
            for (uint256 i = 0; i < list.length; i++) {
                uint256 below;
                uint256 wi = OnyxStorageLib.weight(key, list[i]);
                for (uint256 j = 0; j < list.length; j++) {
                    if (OnyxStorageLib.weight(key, list[j]) < wi) below++;
                }
                if (below < 3) continue;
                vm.prank(_opOf(list[i]));
                st.decline(d, s, list[i]);
            }
        }
        _seed(e);
        (, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        assertEq(hs.length, 3);
        (bytes memory entry, bytes32[] memory es, bytes memory row, bytes32[] memory rs) =
            _proof(st.seedOf(e), d, 0);
        vm.prank(_opOf(hs[0]));
        uint256 g = gasleft();
        assertTrue(st.answer(d, e, 0, hs[0], list, entry, es, row, rs));
        uint256 used = g - gasleft();
        emit log_named_uint("answer gas with 128 nodes, 125 declined (execution only)", used);
        // 5,972,284 with the old scan per pick; 1,634,354 with the heap (2026-11-02).
        assertLt(used, 2_000_000);
    }

    function addOneMore() external {
        _addNode(500, 1);
    }

    // ---------------------------------------------------------------- fuzz

    /// Payment rounding (THREATS STORAGE-10): k × r slot shares never exceed the epoch's pay,
    /// and what is left is under k × r base units.
    function testFuzz_slotPayNeverExceedsEpochPay(uint128 price, uint64 gib, uint8 r, uint8 k)
        public
        pure
    {
        price = uint128(bound(price, 1, 1e30));
        gib = uint64(bound(gib, 1, 1 << 34));
        r = uint8(bound(r, 1, 8));
        k = uint8(bound(k, 1, 16));
        uint256 epochPay = uint256(price) * gib * r;
        uint256 slot = epochPay / (uint256(k) * r);
        assertLe(slot * k * r, epochPay);
        assertLt(epochPay - slot * k * r, uint256(k) * r);
    }

    /// A forged row never passes, whichever byte changes.
    function testFuzz_forgedRowFails(uint256 at, uint8 flip) public {
        vm.assume(flip != 0);
        (uint64 d, uint64 e, bytes32[] memory list) = _start();
        (, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        (bytes memory entry, bytes32[] memory es, bytes memory row, bytes32[] memory rs) =
            _proof(st.seedOf(e), d, 0);
        at = bound(at, 0, row.length - 1);
        row[at] = bytes1(uint8(row[at]) ^ flip);
        vm.prank(_opOf(hs[0]));
        assertFalse(st.answer(d, e, 0, hs[0], list, entry, es, row, rs));
    }
}
