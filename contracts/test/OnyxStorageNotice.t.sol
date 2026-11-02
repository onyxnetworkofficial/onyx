// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Vm} from "forge-std/Vm.sol";
import {StorageFixture} from "./OnyxStorageFixture.sol";
import {OnyxStorage} from "../src/OnyxStorage.sol";

/// ADR-0060 amendment 2 item 2 (THREATS STORAGE-S16-R2-3, STORAGE-S16-R6): a decline of a shard
/// the node passed a check on in the `NOTICE_EPOCHS` epochs before the decline takes effect one
/// epoch later (`exclusionFrom`). Most cases use a one-shard deal, where both challenge slots of
/// an epoch ask for that shard; the whole-deal case uses the fixture's two-shard deal.
contract OnyxStorageNoticeTest is StorageFixture {
    string internal constant ONE = "synthetic.one";

    bytes[2] internal rows1;
    bytes internal entry1;
    bytes internal header1;
    bytes internal payload1;

    function setUp() public {
        _deploy();
        for (uint256 i = 0; i < 5; i++) {
            _addNode(i, 100);
        }
        _buildOneShard();
    }

    /// A one-shard dataset of two rows (dataset `synthetic.one`) and its anchor in the book.
    function _buildOneShard() internal {
        rows1[0] = "one-shard row 0";
        rows1[1] = "one-shard row 1";
        entry1 = _entry(0, _root2(rows1[0], rows1[1]));
        bytes32 shardsRoot = keccak256(abi.encodePacked(bytes1(0x02), uint64(1), _leaf(entry1)));
        header1 = abi.encodePacked(
            uint8(1),
            _str(bytes(ONE)),
            _str(bytes(PUBLISHER)),
            hex"0100000000000000000000000000000000000000000000000000000000000000000000000000",
            _le(1, 4),
            _le(2, 8),
            _le(BYTES, 8),
            _le(0, 16),
            _le(1, 16),
            shardsRoot
        );
        bytes32 root = keccak256(abi.encodePacked("onyx-bulk-manifest-v1", header1));
        bytes32 key = book.keyId(PUBLISHER, vm.addr(signerKey));
        payload1 = abi.encodePacked(
            _str(bytes(PUBLISHER)),
            key,
            _str("bulk.manifest"),
            uint8(3),
            _str(bytes(ONE)),
            uint8(4),
            _le(32, 4),
            root,
            _le(T0, 8),
            _le(43, 8)
        );
        book.postReport(payload1, _sign(payload1));
    }

    /// A one-shard deal of 20 epochs and its first challenged epoch (seeded); the node list.
    function _oneShardDeal() internal returns (uint64 d, uint64 e, bytes32[] memory list) {
        vm.prank(publisher);
        d = st.createDeal(header1, payload1, 3, 20, "");
        (,,,,,,, e,,,,) = st.dealInfo(d);
        list = _list();
        _seed(e);
    }

    /// Holder `node`'s honest answer to slot `slot` of the one-shard deal `d` in epoch `e`.
    function _answer1(bytes32 node, uint64 d, uint64 e, uint32 slot, bytes32[] memory list)
        internal
        returns (bool)
    {
        uint256 r = _rowOf(st.seedOf(e), d, slot, node);
        vm.prank(_opOf(node));
        return st.answer(
            d, e, slot, node, list, entry1, new bytes32[](0), rows1[r], _one(_leaf(rows1[1 - r]))
        );
    }

    /// Helper so `expectRevert` sees one external call.
    function answer1As(bytes32 node, uint64 d, uint64 e, uint32 slot, bytes32[] memory list)
        external
        returns (bool)
    {
        return _answer1(node, d, e, slot, list);
    }

    function _has(bytes32[] memory a, bytes32 x) internal pure returns (bool) {
        for (uint256 i = 0; i < a.length; i++) {
            if (a[i] == x) return true;
        }
        return false;
    }

    function _decline(uint64 d, uint32 index, bytes32 node) internal {
        vm.prank(_opOf(node));
        st.decline(d, index, node);
    }

    function test_rulesVersionAndWindow() public view {
        assertEq(st.RULES_VERSION(), 3);
        assertEq(st.NOTICE_EPOCHS(), 7);
    }

    /// A holder that passed at c - 1 declines its one shard at c: still challenged at c + 1 (it
    /// answers and is paid), out from c + 2. The `Declined` event carries the raw epoch c + 1.
    function test_aDeclineAfterARecentPassIsNoticed() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _oneShardDeal();
        (, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        bytes32 x = hs[0];
        assertTrue(_answer1(x, d, e, 0, list));
        uint64 c = e + 1;
        _toEpoch(c, 0);
        vm.expectEmit(address(st));
        emit OnyxStorage.Declined(x, d, 0, c + 1);
        _decline(d, 0, x);
        assertEq(st.exclusionFrom(d, 0, x), c + 2);
        // Epoch c + 1: both slots still ask x; it answers and is paid.
        _seed(c + 1);
        for (uint32 slot = 0; slot < K; slot++) {
            (uint32 idx, bytes32[] memory h) = st.challengedHolders(d, c + 1, slot, list);
            assertEq(idx, 0);
            assertTrue(_has(h, x), "not asked under notice");
        }
        uint256 before = st.earned(_opOf(x));
        assertTrue(_answer1(x, d, c + 1, 0, list));
        assertEq(st.earned(_opOf(x)) - before, st.slotPay(d));
        // A pass under notice does not stretch it.
        assertEq(st.exclusionFrom(d, 0, x), c + 2);
        // Epoch c + 2: x is out (its place still counts while the next node fetches).
        _seed(c + 2);
        (, bytes32[] memory h2) = st.challengedHolders(d, c + 2, 0, list);
        assertFalse(_has(h2, x));
        assertEq(h2.length, 2);
        vm.expectRevert(OnyxStorage.NotChallenged.selector);
        this.answer1As(x, d, c + 2, 0, list);
        // Epoch c + 3: three holders again, never x.
        _seed(c + 3);
        (, bytes32[] memory h3) = st.challengedHolders(d, c + 3, 0, list);
        assertFalse(_has(h3, x));
        assertEq(h3.length, 3);
        assertTrue(st.conserved());
    }

    /// A holder that never passed a check on the shard (an empty last pass is no pass) is out
    /// from the next epoch, as before the amendment.
    function test_noNoticeWithoutAPass() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _oneShardDeal();
        (, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        bytes32 x = hs[0];
        _decline(d, 0, x);
        assertEq(st.exclusionFrom(d, 0, x), e + 1);
        _seed(e + 1);
        (, bytes32[] memory h) = st.challengedHolders(d, e + 1, 0, list);
        assertFalse(_has(h, x));
        vm.expectRevert(OnyxStorage.NotChallenged.selector);
        this.answer1As(x, d, e + 1, 0, list);
    }

    /// The window is counted from the decline's epoch c: a pass at c - K gives notice, one at
    /// c - K - 1 does not. Read one epoch later, so a window counted from "now" would differ
    /// (review STORAGE-S16-R6 M1).
    function test_noNoticeWhenTheLastPassIsOlderThanTheWindow() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _oneShardDeal();
        (, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        (bytes32 x, bytes32 y) = (hs[0], hs[1]);
        assertTrue(_answer1(x, d, e, 0, list));
        assertTrue(_answer1(y, d, e, 0, list));
        uint64 k = st.NOTICE_EPOCHS();
        // x declines at e + K: its pass at e is in the window, out from e + K + 2.
        _toEpoch(e + k, 0);
        _decline(d, 0, x);
        // y declines at e + K + 1: its pass is one epoch too old, out from e + K + 2 as well.
        _toEpoch(e + k + 1, 0);
        _decline(d, 0, y);
        assertEq(st.exclusionFrom(d, 0, x), e + k + 2, "in the window");
        assertEq(st.exclusionFrom(d, 0, y), e + k + 2, "outside the window");
        _toEpoch(e + k + 3, 0);
        assertEq(st.exclusionFrom(d, 0, x), e + k + 2, "the record never moves");
        assertEq(st.exclusionFrom(d, 0, y), e + k + 2);
    }

    /// A whole-deal decline is under notice only on shards the node passed in the window: a
    /// shard of the same deal it never passed (a withheld one) is out from the next epoch.
    function test_aWholeDealDeclineDelaysOnlyShardsPassedInTheWindow() public {
        vm.prank(publisher);
        uint64 d = st.createDeal(header, payload, 3, 4, "");
        (,,,,,,, uint64 e,,,,) = st.dealInfo(d);
        bytes32[] memory list = _list();
        _seed(e);
        (uint32 i, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        bytes32 x = hs[0];
        assertTrue(_answer(x, d, e, 0, list));
        uint32 j = 1 - i;
        uint64 c = e + 1;
        _toEpoch(c, 0);
        _decline(d, st.WHOLE_DEAL(), x);
        assertEq(st.exclusionFrom(d, i, x), c + 2, "the passed shard: notice");
        assertEq(st.exclusionFrom(d, j, x), c + 1, "the shard never passed: none");
        assertEq(st.exclusionFrom(d, st.WHOLE_DEAL(), x), c + 1, "the raw whole-deal record");
        // Another node with no pass on the deal: out from c + 1 everywhere.
        bytes32 z = hs[1];
        _decline(d, st.WHOLE_DEAL(), z);
        assertEq(st.exclusionFrom(d, i, z), c + 1);
        assertEq(st.exclusionFrom(d, j, z), c + 1);
    }

    /// A pass in the decline's own epoch, after the decline (the challenge drawn then still
    /// stands), puts the decline under notice.
    function test_aPassInTheDeclineEpochAfterTheDeclineDelaysIt() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _oneShardDeal();
        (, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        bytes32 x = hs[0];
        _decline(d, 0, x);
        assertEq(st.exclusionFrom(d, 0, x), e + 1);
        assertTrue(_answer1(x, d, e, 0, list));
        assertEq(st.exclusionFrom(d, 0, x), e + 2);
        _seed(e + 1);
        assertTrue(_answer1(x, d, e + 1, 1, list));
        assertEq(st.exclusionFrom(d, 0, x), e + 2);
    }

    /// Review STORAGE-S16-R6 M2, the wiped store: x passed at c - 1, misses at c, declines at
    /// c + 1 (notice: out from c + 3), and the miss is settled at c + 1 (slash: out from c + 2).
    /// The earlier wins: x is not asked at c + 2. The `Slashed` event carries the raw c + 2.
    function test_aSlashAfterANoticedDeclinePullsTheExclusionIn() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _oneShardDeal();
        (, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        bytes32 x = hs[0];
        assertTrue(_answer1(x, d, e, 0, list));
        uint64 c = e + 1;
        _seed(c);
        _seed(c + 1);
        _decline(d, 0, x);
        assertEq(st.exclusionFrom(d, 0, x), c + 3, "noticed");
        uint256 slash = _bond(x) * SLASH_PPM / 1e6;
        uint8 missed = st.MISSED();
        vm.expectEmit(address(st));
        emit OnyxStorage.Slashed(x, d, c, 0, 0, slash, c + 2, missed);
        st.settleMissed(d, c, 0, x, list);
        assertEq(st.exclusionFrom(d, 0, x), c + 2, "the slash is earlier");
        _seed(c + 2);
        (, bytes32[] memory h) = st.challengedHolders(d, c + 2, 0, list);
        assertFalse(_has(h, x));
        // The other order: once slashed, a decline of that shard is refused.
        bytes32 y = hs[1];
        st.settleMissed(d, c + 1, 0, y, list);
        vm.prank(_opOf(y));
        vm.expectRevert(OnyxStorage.AlreadyExcluded.selector);
        st.decline(d, 0, y);
        assertTrue(st.conserved());
    }

    /// The latest pass counts, not the first: x passed at e and at p = e + K + 1, then declines
    /// at p. The pass at e is out of the window, the one at p is in it (review STORAGE-S16-R7 M2:
    /// a contract that kept the first pass would put x out one epoch before its mirror does).
    function test_theLatestPassCountsForTheNotice() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _oneShardDeal();
        (, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        bytes32 x = hs[0];
        assertTrue(_answer1(x, d, e, 0, list));
        uint64 p = e + st.NOTICE_EPOCHS() + 1;
        _seed(p);
        assertTrue(_answer1(x, d, p, 0, list));
        _decline(d, 0, x);
        assertEq(st.exclusionFrom(d, 0, x), p + 2, "the pass at p gives notice");
    }

    /// A later slash on the same shard never moves the exclusion: x misses at e and at e + 1
    /// (still asked then); the first settle puts it out from e + 2 and the second, at e + 2,
    /// leaves that (review STORAGE-S16-R7 M2). Both `Slashed` events carry the raw e + 2.
    function test_aSecondSlashDoesNotMoveTheExclusion() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _oneShardDeal();
        (, bytes32[] memory hs) = st.challengedHolders(d, e, 0, list);
        bytes32 x = hs[0];
        uint8 missed = st.MISSED();
        _seed(e + 1);
        st.settleMissed(d, e, 0, x, list);
        assertEq(st.exclusionFrom(d, 0, x), e + 2);
        (, bytes32[] memory h1) = st.challengedHolders(d, e + 1, 0, list);
        assertTrue(_has(h1, x), "still asked at e + 1");
        _seed(e + 2);
        uint256 slash = _bond(x) * SLASH_PPM / 1e6;
        vm.expectEmit(address(st));
        emit OnyxStorage.Slashed(x, d, e + 1, 0, 0, slash, e + 2, missed);
        st.settleMissed(d, e + 1, 0, x, list);
        assertEq(st.exclusionFrom(d, 0, x), e + 2, "a later slash never moves it");
        assertTrue(st.conserved());
    }

    /// The `Answered` event names the shard the slot asked for.
    function test_answeredNamesTheShard() public {
        (uint64 d, uint64 e, bytes32[] memory list) = _oneShardDeal();
        (, bytes32[] memory hs) = st.challengedHolders(d, e, 1, list);
        vm.recordLogs();
        assertTrue(_answer1(hs[2], d, e, 1, list));
        Vm.Log[] memory logs = vm.getRecordedLogs();
        bool seen;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics[0] != OnyxStorage.Answered.selector) continue;
            (uint32 slot, uint32 shard, bool passed, uint256 pay) =
                abi.decode(logs[i].data, (uint32, uint32, bool, uint256));
            assertEq(slot, 1);
            assertEq(shard, 0);
            assertTrue(passed);
            assertEq(pay, st.slotPay(d));
            seen = true;
        }
        assertTrue(seen);
    }
}
